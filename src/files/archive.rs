//! Archives as read-only directories: `C:\x\fotos.zip\urlaub` is the folder `urlaub` inside
//! `fotos.zip`. The table of contents is read once (`Index`) and cached until the archive
//! file changes.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use crate::files::listing::{Entry, Kind, Listing, kind_from_extension};
use crate::files::names::natural_cmp;
use crate::sys::libarchive::{EntryType, Reader};

/// File name endings opened as archives (Enter goes inside instead of starting a program).
const EXTENSIONS: [&str; 13] = [
    ".zip", ".7z", ".rar", ".tar", ".tgz", ".tar.gz", ".tar.bz2", ".tbz2", ".tar.xz", ".txz", ".tar.zst", ".cab",
    ".iso",
];

pub fn is_archive_name(name: &[u16]) -> bool {
    let name = String::from_utf16_lossy(name).to_lowercase();
    EXTENSIONS.iter().any(|ext| name.len() > ext.len() && name.ends_with(ext))
}

/// The name without its archive extension (`fotos.tar.gz` → `fotos`).
pub fn stem(name: &str) -> &str {
    let lower = name.to_lowercase();
    EXTENSIONS
        .iter()
        .filter(|ext| lower.len() > ext.len() && lower.ends_with(*ext))
        .map(|ext| ext.len())
        .max()
        .and_then(|n| name.get(..name.len() - n))
        .unwrap_or(name)
}

/// Splits a path into the archive file and the path inside it (`\`-separated, `""` for the
/// archive's root). `None` if no part of the path is an archive file.
pub fn split(path: &Path) -> Option<(PathBuf, String)> {
    let mut archive = PathBuf::new();
    let mut components = path.components();
    for c in components.by_ref() {
        archive.push(c);
        if let Component::Normal(name) = c
            && is_archive_name(&name.to_string_lossy().encode_utf16().collect::<Vec<_>>())
            && archive.is_file()
        {
            let inner: Vec<String> = components.map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
            return Some((archive, inner.join("\\")));
        }
    }
    None
}

/// Path of an archive entry as `a\b\c`, or `None` if it would leave the target directory
/// when extracted (`..`, absolute paths, drive letters).
pub fn clean_path(raw: &str) -> Option<String> {
    let mut parts = Vec::new();
    for part in raw.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => return None,
            p if p.contains(':') => return None,
            p => parts.push(p),
        }
    }
    if parts.is_empty() || raw.starts_with(['/', '\\']) { None } else { Some(parts.join("\\")) }
}

/// FILETIME ticks from Unix seconds.
fn filetime(unix: i64) -> u64 {
    ((unix + 11_644_473_600).max(0) as u64) * 10_000_000
}

struct Item {
    name: Box<[u16]>,
    is_dir: bool,
    size: u64,
    mtime: u64,
}

/// Table of contents of one archive.
pub struct Index {
    /// Directory path inside the archive (`""` = root) → its entries.
    dirs: HashMap<String, Vec<Item>>,
    /// Every file with its size (for progress totals).
    files: Vec<(String, u64)>,
}

impl Index {
    fn build(archive: &Path, cancel: &AtomicBool) -> Result<Index, String> {
        let mut reader = Reader::open(archive)?;
        let mut index = Index { dirs: HashMap::from([(String::new(), Vec::new())]), files: Vec::new() };
        // Full path → position in its directory's list, to merge duplicates and implicit dirs.
        let mut known: HashMap<String, usize> = HashMap::new();
        while let Some(header) = reader.next() {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let h = header?;
            if h.kind == EntryType::Other {
                continue;
            }
            let Some(path) = clean_path(&h.path) else { continue };
            let is_dir = h.kind == EntryType::Dir;
            // Make sure every parent directory exists, even if the archive doesn't list it.
            let mut parent = String::new();
            for part in path.split('\\').collect::<Vec<_>>().split_last().map(|(_, p)| p).unwrap_or(&[]) {
                let full = if parent.is_empty() { part.to_string() } else { format!("{parent}\\{part}") };
                index.add(&mut known, &parent, part, &full, true, 0, 0);
                parent = full;
            }
            let name = path.rsplit('\\').next().unwrap_or(&path).to_string();
            index.add(&mut known, &parent, &name, &path, is_dir, h.size, filetime(h.mtime));
            if !is_dir {
                index.files.push((path, h.size));
            }
        }
        Ok(index)
    }

    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        known: &mut HashMap<String, usize>,
        parent: &str,
        name: &str,
        full: &str,
        is_dir: bool,
        size: u64,
        mtime: u64,
    ) {
        if is_dir {
            self.dirs.entry(full.to_string()).or_default();
        }
        let list = self.dirs.entry(parent.to_string()).or_default();
        match known.get(full) {
            // Listed again: an explicit directory entry after its contents, or a newer copy
            // of a file (tar archives can be appended to). Keep the latest data.
            Some(&i) => {
                let item = &mut list[i];
                if mtime != 0 {
                    item.mtime = mtime;
                }
                if !is_dir {
                    item.size = size;
                }
            }
            None => {
                known.insert(full.to_string(), list.len());
                list.push(Item { name: name.encode_utf16().collect(), is_dir, size, mtime });
            }
        }
    }

    /// Exact match first, then ignoring case (paths typed by hand).
    fn dir(&self, inner: &str) -> Option<&Vec<Item>> {
        self.dirs.get(inner).or_else(|| self.dirs.iter().find(|(k, _)| k.eq_ignore_ascii_case(inner)).map(|(_, v)| v))
    }

    /// Number of files and bytes at or below the given inner paths.
    pub fn totals(&self, inner: &[String]) -> (u64, u64) {
        let mut files = 0;
        let mut bytes = 0;
        for (path, size) in &self.files {
            let below = |p: &String| path == p || p.is_empty() || path.starts_with(&format!("{p}\\"));
            if inner.iter().any(below) {
                files += 1;
                bytes += size;
            }
        }
        (files, bytes)
    }
}

struct Cached {
    archive: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
    index: Arc<Index>,
}

/// The table of contents of `archive`, from the cache if the file hasn't changed.
pub fn index(archive: &Path, cancel: &AtomicBool) -> Result<Arc<Index>, String> {
    static CACHE: Mutex<Option<Cached>> = Mutex::new(None);
    let meta = std::fs::metadata(archive).map_err(|e| e.to_string())?;
    let (len, modified) = (meta.len(), meta.modified().ok());
    if let Some(c) = CACHE.lock().unwrap().as_ref()
        && c.archive == archive
        && c.len == len
        && c.modified == modified
    {
        return Ok(c.index.clone());
    }
    // Built outside the lock: both panels may load different archives at the same time.
    let index = Arc::new(Index::build(archive, cancel)?);
    *CACHE.lock().unwrap() = Some(Cached { archive: archive.to_path_buf(), len, modified, index: index.clone() });
    Ok(index)
}

/// Like `listing::load`, for a directory inside an archive. A path to a file inside the
/// archive lists its directory and selects the file.
pub fn load(path: &Path, archive: PathBuf, inner: &str, cancel: &AtomicBool) -> Result<Listing, String> {
    let start = Instant::now();
    let index = index(&archive, cancel)?;
    let (dir, inner, select) = match index.dir(inner) {
        Some(_) => (path.to_path_buf(), inner.to_string(), None),
        None => {
            let (parent, name) = inner.rsplit_once('\\').unwrap_or(("", inner));
            let found = index.dir(parent).is_some_and(|l| l.iter().any(|i| String::from_utf16_lossy(&i.name) == name));
            if !found {
                return Err(crate::i18n::tf("error.no_such_entry", &[("name", &inner)]));
            }
            let dir = path.parent().unwrap_or(path).to_path_buf();
            (dir, parent.to_string(), Some(name.encode_utf16().collect()))
        }
    };
    let items = index.dir(&inner).map(Vec::as_slice).unwrap_or_default();
    let mut entries: Vec<Entry> = items
        .iter()
        .map(|i| Entry {
            name: i.name.clone(),
            kind: if i.is_dir { Kind::Dir } else { kind_from_extension(&i.name) },
            hidden: false,
            size: i.size,
            mtime: i.mtime,
            marked: false,
        })
        .collect();
    entries.sort_unstable_by(|a, b| b.is_dir().cmp(&a.is_dir()).then_with(|| natural_cmp(&a.name, &b.name)));
    let dirs = entries.iter().filter(|e| e.is_dir()).count();
    let bytes = entries.iter().map(|e| e.size).sum();
    let files = entries.len() - dirs;
    // Inside an archive there is always a way up: to the directory holding the archive.
    entries.insert(0, Entry::parent());
    Ok(Listing {
        dir: dir.into(),
        entries,
        select,
        dirs,
        files,
        bytes,
        elapsed: start.elapsed(),
        archive: Some(archive),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn archive_names() {
        assert!(is_archive_name(&w("Fotos.ZIP")));
        assert!(is_archive_name(&w("x.tar.gz")));
        assert!(is_archive_name(&w("a.7z")));
        assert!(!is_archive_name(&w("brief.docx")));
        assert!(!is_archive_name(&w("app.jar")));
        assert!(!is_archive_name(&w(".zip")));
    }

    #[test]
    fn unsafe_paths_are_rejected() {
        assert_eq!(clean_path("a/b/c.txt").as_deref(), Some("a\\b\\c.txt"));
        assert_eq!(clean_path("./a//b/").as_deref(), Some("a\\b"));
        assert_eq!(clean_path("../evil.txt"), None);
        assert_eq!(clean_path("a/../../evil.txt"), None);
        assert_eq!(clean_path("/etc/passwd"), None);
        assert_eq!(clean_path("\\Windows\\x"), None);
        assert_eq!(clean_path("C:/Windows/x"), None);
        assert_eq!(clean_path(""), None);
    }
}
