//! Reading archives with libarchive. Windows 11 ships it as `archiveint.dll` (Explorer uses it
//! for zip/7z/rar), so no extra library is needed. It is loaded from System32 on first use;
//! on older Windows it is missing and archives can't be opened.

use std::ffi::{CStr, c_char, c_void};
use std::path::Path;
use std::sync::OnceLock;

use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows::core::{s, w};

use crate::files::names::to_utf16_nul;

type Archive = *mut c_void;
type ArchiveEntry = *mut c_void;

// Return codes of libarchive.
const ARCHIVE_OK: i32 = 0;
const ARCHIVE_EOF: i32 = 1;
const ARCHIVE_WARN: i32 = -20;

// File types (`archive_entry_filetype`).
const AE_IFMT: u16 = 0o170000;
const AE_IFREG: u16 = 0o100000;
const AE_IFDIR: u16 = 0o040000;

/// The libarchive functions we use.
struct Api {
    read_new: unsafe extern "C" fn() -> Archive,
    read_support_filter_all: unsafe extern "C" fn(Archive) -> i32,
    read_support_format_all: unsafe extern "C" fn(Archive) -> i32,
    read_open_filename_w: unsafe extern "C" fn(Archive, *const u16, usize) -> i32,
    read_next_header: unsafe extern "C" fn(Archive, *mut ArchiveEntry) -> i32,
    read_data: unsafe extern "C" fn(Archive, *mut c_void, usize) -> isize,
    read_free: unsafe extern "C" fn(Archive) -> i32,
    error_string: unsafe extern "C" fn(Archive) -> *const c_char,
    entry_pathname_w: unsafe extern "C" fn(ArchiveEntry) -> *const u16,
    entry_pathname_utf8: unsafe extern "C" fn(ArchiveEntry) -> *const c_char,
    entry_size: unsafe extern "C" fn(ArchiveEntry) -> i64,
    entry_mtime: unsafe extern "C" fn(ArchiveEntry) -> i64,
    entry_filetype: unsafe extern "C" fn(ArchiveEntry) -> u16,
    entry_is_encrypted: unsafe extern "C" fn(ArchiveEntry) -> i32,
}

// The transmute targets are the `Api` field types; spelling them out again adds nothing.
#[allow(clippy::missing_transmute_annotations)]
fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| unsafe {
        let lib = LoadLibraryExW(w!("archiveint.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
        // SAFETY: each symbol is a libarchive function with exactly the declared signature
        // (libarchive's public C API; `wchar_t` is UTF-16, `time_t` and `la_int64_t` are 64 bit).
        macro_rules! sym {
            ($name:expr) => {
                std::mem::transmute(GetProcAddress(lib, $name)?)
            };
        }
        Some(Api {
            read_new: sym!(s!("archive_read_new")),
            read_support_filter_all: sym!(s!("archive_read_support_filter_all")),
            read_support_format_all: sym!(s!("archive_read_support_format_all")),
            read_open_filename_w: sym!(s!("archive_read_open_filename_w")),
            read_next_header: sym!(s!("archive_read_next_header")),
            read_data: sym!(s!("archive_read_data")),
            read_free: sym!(s!("archive_read_free")),
            error_string: sym!(s!("archive_error_string")),
            entry_pathname_w: sym!(s!("archive_entry_pathname_w")),
            entry_pathname_utf8: sym!(s!("archive_entry_pathname_utf8")),
            entry_size: sym!(s!("archive_entry_size")),
            entry_mtime: sym!(s!("archive_entry_mtime")),
            entry_filetype: sym!(s!("archive_entry_filetype")),
            entry_is_encrypted: sym!(s!("archive_entry_is_encrypted")),
        })
    })
    .as_ref()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EntryType {
    File,
    Dir,
    /// Links, devices …: not extracted.
    Other,
}

/// One entry of the archive's table of contents.
pub struct Header {
    /// Path inside the archive as stored (usually `/`-separated).
    pub path: String,
    pub kind: EntryType,
    pub size: u64,
    /// Seconds since 1970 (UTC).
    pub mtime: i64,
    pub encrypted: bool,
}

/// Reads an archive front to back: `next` for each entry, `read` for its data.
pub struct Reader {
    api: &'static Api,
    archive: Archive,
    entry: ArchiveEntry,
}

impl Drop for Reader {
    fn drop(&mut self) {
        unsafe { (self.api.read_free)(self.archive) };
    }
}

impl Reader {
    pub fn open(path: &Path) -> Result<Reader, String> {
        let api = api().ok_or("Archive öffnen braucht Windows 11 (archiveint.dll fehlt)")?;
        let archive = unsafe { (api.read_new)() };
        if archive.is_null() {
            return Err("libarchive: kein Speicher".into());
        }
        let reader = Reader { api, archive, entry: std::ptr::null_mut() };
        let name = to_utf16_nul(path.as_os_str());
        let r = unsafe {
            (api.read_support_filter_all)(archive);
            (api.read_support_format_all)(archive);
            (api.read_open_filename_w)(archive, name.as_ptr(), 64 * 1024)
        };
        if r != ARCHIVE_OK && r != ARCHIVE_WARN {
            return Err(reader.error());
        }
        Ok(reader)
    }

    /// The next entry; `None` at the end. After an error the reader is unusable.
    pub fn next(&mut self) -> Option<Result<Header, String>> {
        let r = unsafe { (self.api.read_next_header)(self.archive, &mut self.entry) };
        match r {
            ARCHIVE_EOF => None,
            ARCHIVE_OK | ARCHIVE_WARN => Some(Ok(self.header())),
            _ => Some(Err(self.error())),
        }
    }

    /// Data of the current entry; `Ok(0)` at its end.
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let n = unsafe { (self.api.read_data)(self.archive, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 { Err(self.error()) } else { Ok(n as usize) }
    }

    fn header(&self) -> Header {
        let (api, e) = (self.api, self.entry);
        unsafe {
            let wide = (api.entry_pathname_w)(e);
            let path = if !wide.is_null() {
                let len = (0..).take_while(|&i| *wide.add(i) != 0).count();
                String::from_utf16_lossy(std::slice::from_raw_parts(wide, len))
            } else {
                let utf8 = (api.entry_pathname_utf8)(e);
                if utf8.is_null() { String::new() } else { CStr::from_ptr(utf8).to_string_lossy().into_owned() }
            };
            let kind = match (api.entry_filetype)(e) & AE_IFMT {
                AE_IFDIR => EntryType::Dir,
                AE_IFREG => EntryType::File,
                _ if path.ends_with('/') => EntryType::Dir,
                _ => EntryType::Other,
            };
            Header {
                path,
                kind,
                size: (api.entry_size)(e).max(0) as u64,
                mtime: (api.entry_mtime)(e),
                encrypted: (api.entry_is_encrypted)(e) != 0,
            }
        }
    }

    fn error(&self) -> String {
        let msg = unsafe { (self.api.error_string)(self.archive) };
        let text = if msg.is_null() {
            "unbekannter Fehler".to_string()
        } else {
            unsafe { CStr::from_ptr(msg) }.to_string_lossy().into_owned()
        };
        if text.to_ascii_lowercase().contains("encrypt") {
            "Verschlüsseltes Archiv – Passwörter werden noch nicht unterstützt".into()
        } else {
            text
        }
    }
}
