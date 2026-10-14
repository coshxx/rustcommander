//! Read directories (fastest Win32 way) and sort them; runs on loader threads.

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::time::{Duration, Instant};

use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM, FIND_FIRST_EX_LARGE_FETCH, FindClose,
    FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW, GetFileAttributesW,
    INVALID_FILE_ATTRIBUTES, WIN32_FIND_DATAW,
};
use windows::core::PCWSTR;

use crate::files::names::{natural_cmp, to_utf16_nul};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Parent,
    Dir,
    File,
    Image,
    Audio,
    Video,
    Archive,
    Code,
    Exe,
    Doc,
}

pub struct Entry {
    /// UTF-16 without null terminator – can be passed straight to DirectWrite.
    pub name: Box<[u16]>,
    pub kind: Kind,
    pub hidden: bool,
    pub size: u64,
    /// FILETIME (100 ns ticks since 1601, UTC).
    pub mtime: u64,
    pub marked: bool,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, Kind::Dir | Kind::Parent)
    }

    pub fn os_name(&self) -> OsString {
        OsString::from_wide(&self.name)
    }

    pub fn parent() -> Entry {
        Entry {
            name: [b'.' as u16, b'.' as u16].into(),
            kind: Kind::Parent,
            hidden: false,
            size: 0,
            mtime: 0,
            marked: false,
        }
    }
}

pub struct Listing {
    pub dir: PathBuf,
    pub entries: Vec<Entry>,
    /// Entry to put the cursor on (e.g. when a file path was entered).
    pub select: Option<Box<[u16]>>,
    pub dirs: usize,
    pub files: usize,
    pub bytes: u64,
    pub elapsed: Duration,
}

/// Long paths get the `\\?\` prefix so MAX_PATH doesn't apply.
fn long_path(path: &Path) -> Vec<u16> {
    let w: Vec<u16> = path.as_os_str().encode_wide().collect();
    if w.len() < 240 || w.starts_with(&[b'\\' as u16, b'\\' as u16, b'?' as u16]) {
        return w;
    }
    let mut out: Vec<u16>;
    if w.starts_with(&[b'\\' as u16, b'\\' as u16]) {
        out = r"\\?\UNC".encode_utf16().collect();
        out.extend_from_slice(&w[1..]);
    } else {
        out = r"\\?\".encode_utf16().collect();
        out.extend_from_slice(&w);
    }
    out
}

fn kind_from_extension(name: &[u16]) -> Kind {
    let Some(dot) = name.iter().rposition(|&c| c == b'.' as u16) else {
        return Kind::File;
    };
    let ext = &name[dot + 1..];
    if ext.is_empty() || ext.len() > 5 {
        return Kind::File;
    }
    let mut buf = [0u8; 5];
    for (i, &c) in ext.iter().enumerate() {
        if c >= 128 {
            return Kind::File;
        }
        buf[i] = (c as u8).to_ascii_lowercase();
    }
    match &buf[..ext.len()] {
        b"png" | b"jpg" | b"jpeg" | b"gif" | b"bmp" | b"webp" | b"svg" | b"ico" | b"tif" | b"tiff" | b"heic"
        | b"avif" | b"psd" | b"raw" => Kind::Image,
        b"mp3" | b"wav" | b"flac" | b"ogg" | b"m4a" | b"aac" | b"wma" | b"opus" => Kind::Audio,
        b"mp4" | b"mkv" | b"avi" | b"mov" | b"wmv" | b"webm" | b"m4v" | b"flv" => Kind::Video,
        b"zip" | b"7z" | b"rar" | b"tar" | b"gz" | b"bz2" | b"xz" | b"zst" | b"cab" | b"iso" | b"tgz" => Kind::Archive,
        b"rs" | b"c" | b"h" | b"cpp" | b"hpp" | b"cc" | b"cs" | b"js" | b"ts" | b"tsx" | b"jsx" | b"py" | b"go"
        | b"java" | b"kt" | b"json" | b"toml" | b"yaml" | b"yml" | b"xml" | b"html" | b"css" | b"scss" | b"sh"
        | b"ps1" | b"lua" | b"sql" | b"rb" | b"php" | b"swift" | b"zig" | b"vue" | b"ini" | b"cfg" | b"lock" => {
            Kind::Code
        }
        b"exe" | b"msi" | b"bat" | b"cmd" | b"com" | b"lnk" | b"dll" | b"sys" => Kind::Exe,
        b"txt" | b"md" | b"pdf" | b"doc" | b"docx" | b"xls" | b"xlsx" | b"ppt" | b"pptx" | b"odt" | b"ods" | b"rtf"
        | b"csv" | b"log" | b"epub" => Kind::Doc,
        _ => Kind::File,
    }
}

/// If `path` is a file, its directory is loaded instead and the file gets selected.
pub fn load(path: &Path, cancel: &AtomicBool) -> Result<Listing, String> {
    let start = Instant::now();
    let wpath = to_utf16_nul(path.as_os_str());
    let attrs = unsafe { GetFileAttributesW(PCWSTR(wpath.as_ptr())) };
    if attrs == INVALID_FILE_ATTRIBUTES {
        return Err(windows::core::Error::from_thread().message());
    }
    let (dir, select) = if attrs & FILE_ATTRIBUTE_DIRECTORY.0 == 0 {
        match (path.parent(), path.file_name()) {
            (Some(p), Some(f)) => (p.to_path_buf(), Some(f.encode_wide().collect())),
            _ => (path.to_path_buf(), None),
        }
    } else {
        (path.to_path_buf(), None)
    };

    let mut pattern = long_path(&dir);
    if pattern.last() != Some(&(b'\\' as u16)) {
        pattern.push(b'\\' as u16);
    }
    pattern.extend_from_slice(&[b'*' as u16, 0]);

    let mut data = WIN32_FIND_DATAW::default();
    let handle = unsafe {
        FindFirstFileExW(
            PCWSTR(pattern.as_ptr()),
            FindExInfoBasic,
            &mut data as *mut _ as *mut _,
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_LARGE_FETCH,
        )
    }
    .map_err(|e| e.message())?;

    let mut entries = Vec::with_capacity(256);
    let (mut dirs, mut files, mut bytes) = (0usize, 0usize, 0u64);
    loop {
        if cancel.load(AtomicOrdering::Relaxed) {
            unsafe {
                let _ = FindClose(handle);
            }
            return Err("cancelled".into());
        }
        let len = data.cFileName.iter().position(|&c| c == 0).unwrap_or(data.cFileName.len());
        let name = &data.cFileName[..len];
        let dot = name == [b'.' as u16] || name == [b'.' as u16, b'.' as u16];
        if !dot {
            let a = data.dwFileAttributes;
            let is_dir = a & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
            let size = ((data.nFileSizeHigh as u64) << 32) | data.nFileSizeLow as u64;
            if is_dir {
                dirs += 1;
            } else {
                files += 1;
                bytes += size;
            }
            entries.push(Entry {
                name: name.into(),
                kind: if is_dir { Kind::Dir } else { kind_from_extension(name) },
                hidden: a & (FILE_ATTRIBUTE_HIDDEN.0 | FILE_ATTRIBUTE_SYSTEM.0) != 0,
                size: if is_dir { 0 } else { size },
                mtime: ((data.ftLastWriteTime.dwHighDateTime as u64) << 32) | data.ftLastWriteTime.dwLowDateTime as u64,
                marked: false,
            });
        }
        if unsafe { FindNextFileW(handle, &mut data) }.is_err() {
            break;
        }
    }
    unsafe {
        let _ = FindClose(handle);
    }

    entries.sort_unstable_by(|a, b| b.is_dir().cmp(&a.is_dir()).then_with(|| natural_cmp(&a.name, &b.name)));
    if dir.parent().is_some() {
        entries.insert(0, Entry::parent());
    }

    Ok(Listing { dir, entries, select, dirs, files, bytes, elapsed: start.elapsed() })
}

/// A running load; `cancel` is set as soon as it is superseded.
pub struct Pending {
    pub id: u64,
    pub path: PathBuf,
    pub select: Option<Box<[u16]>>,
    /// Reload of the same directory: keep the cursor on whatever entry it is on when the
    /// result arrives (the user may have moved it meanwhile).
    pub keep_cursor: bool,
    pub cancel: Arc<AtomicBool>,
    pub started: Instant,
}

pub struct Loaded {
    pub panel: usize,
    pub id: u64,
    pub result: Result<Listing, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn kinds() {
        assert_eq!(kind_from_extension(&w("a.RS")), Kind::Code);
        assert_eq!(kind_from_extension(&w("noext")), Kind::File);
        assert_eq!(kind_from_extension(&w("x.tar.gz")), Kind::Archive);
    }
}
