//! Reading and writing archives with libarchive. Windows 11 ships it as `archiveint.dll` (Explorer uses it
//! for zip/7z/rar), so no extra library is needed. It is loaded from System32 on first use;
//! on older Windows it is missing and archives can't be opened.

use crate::i18n::t;
use std::ffi::{CStr, CString, c_char, c_void};
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
    write_new: unsafe extern "C" fn() -> Archive,
    write_set_format_zip: unsafe extern "C" fn(Archive) -> i32,
    write_set_format_7zip: unsafe extern "C" fn(Archive) -> i32,
    write_set_options: unsafe extern "C" fn(Archive, *const c_char) -> i32,
    write_open_filename_w: unsafe extern "C" fn(Archive, *const u16) -> i32,
    write_header: unsafe extern "C" fn(Archive, ArchiveEntry) -> i32,
    write_data: unsafe extern "C" fn(Archive, *const c_void, usize) -> isize,
    write_close: unsafe extern "C" fn(Archive) -> i32,
    write_fail: unsafe extern "C" fn(Archive) -> i32,
    write_free: unsafe extern "C" fn(Archive) -> i32,
    entry_new: unsafe extern "C" fn() -> ArchiveEntry,
    entry_free: unsafe extern "C" fn(ArchiveEntry),
    entry_clear: unsafe extern "C" fn(ArchiveEntry) -> ArchiveEntry,
    entry_copy_pathname_w: unsafe extern "C" fn(ArchiveEntry, *const u16),
    entry_set_filetype: unsafe extern "C" fn(ArchiveEntry, u32),
    entry_set_perm: unsafe extern "C" fn(ArchiveEntry, u16),
    entry_set_size: unsafe extern "C" fn(ArchiveEntry, i64),
    entry_set_mtime: unsafe extern "C" fn(ArchiveEntry, i64, i32),
}

// The transmute targets are the `Api` field types; spelling them out again adds nothing.
#[allow(clippy::missing_transmute_annotations)]
fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| unsafe {
        let lib = LoadLibraryExW(w!("archiveint.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
        // SAFETY: each symbol is a libarchive function with exactly the declared signature
        // (libarchive's public C API; `wchar_t` is UTF-16, `time_t` and `la_int64_t` are 64 bit,
        // `long` is 32 bit and `mode_t` 16 bit on Windows).
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
            write_new: sym!(s!("archive_write_new")),
            write_set_format_zip: sym!(s!("archive_write_set_format_zip")),
            write_set_format_7zip: sym!(s!("archive_write_set_format_7zip")),
            write_set_options: sym!(s!("archive_write_set_options")),
            write_open_filename_w: sym!(s!("archive_write_open_filename_w")),
            write_header: sym!(s!("archive_write_header")),
            write_data: sym!(s!("archive_write_data")),
            write_close: sym!(s!("archive_write_close")),
            write_fail: sym!(s!("archive_write_fail")),
            write_free: sym!(s!("archive_write_free")),
            entry_new: sym!(s!("archive_entry_new")),
            entry_free: sym!(s!("archive_entry_free")),
            entry_clear: sym!(s!("archive_entry_clear")),
            entry_copy_pathname_w: sym!(s!("archive_entry_copy_pathname_w")),
            entry_set_filetype: sym!(s!("archive_entry_set_filetype")),
            entry_set_perm: sym!(s!("archive_entry_set_perm")),
            entry_set_size: sym!(s!("archive_entry_set_size")),
            entry_set_mtime: sym!(s!("archive_entry_set_mtime")),
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
        let api = api().ok_or(t("error.needs_windows_11"))?;
        let archive = unsafe { (api.read_new)() };
        if archive.is_null() {
            return Err(t("error.out_of_memory").into());
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
        error_of(self.api, self.archive)
    }
}

/// libarchive's message for the last error on `archive`.
fn error_of(api: &Api, archive: Archive) -> String {
    let msg = unsafe { (api.error_string)(archive) };
    let text = if msg.is_null() {
        t("error.unknown").to_string()
    } else {
        unsafe { CStr::from_ptr(msg) }.to_string_lossy().into_owned()
    };
    if text.to_ascii_lowercase().contains("encrypt") { t("error.encrypted_archive").into() } else { text }
}

/// Archive formats that can be written.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Format {
    #[default]
    Zip,
    SevenZip,
}

impl Format {
    pub const ALL: [Format; 2] = [Format::Zip, Format::SevenZip];

    /// Name in the UI and in config.json; also the file extension.
    pub fn id(self) -> &'static str {
        match self {
            Format::Zip => "zip",
            Format::SevenZip => "7z",
        }
    }

    pub fn from_id(id: &str) -> Option<Format> {
        Format::ALL.into_iter().find(|f| f.id().eq_ignore_ascii_case(id))
    }
}

/// Writes a new archive entry by entry: `add` a header, then `write` the file's data.
pub struct Writer {
    api: &'static Api,
    archive: Archive,
    entry: ArchiveEntry,
    /// Something went wrong (or the writer is dropped unfinished): `Drop` must not complete the
    /// archive.
    failed: bool,
}

impl Drop for Writer {
    fn drop(&mut self) {
        unsafe {
            if self.failed {
                (self.api.write_fail)(self.archive);
            }
            (self.api.write_free)(self.archive);
            if !self.entry.is_null() {
                (self.api.entry_free)(self.entry);
            }
        }
    }
}

impl Writer {
    /// Create `path` (replacing it) as `format` with compression `level` 0 (store) … 9.
    pub fn create(path: &Path, format: Format, level: u8) -> Result<Writer, String> {
        let api = api().ok_or(t("error.needs_windows_11"))?;
        let archive = unsafe { (api.write_new)() };
        if archive.is_null() {
            return Err(t("error.out_of_memory").into());
        }
        let entry = unsafe { (api.entry_new)() };
        let mut w = Writer { api, archive, entry, failed: true };
        if entry.is_null() {
            return Err(t("error.out_of_memory").into());
        }
        let options = match (format, level) {
            (Format::Zip, 0) => "zip:compression=store,zip:hdrcharset=UTF-8".to_string(),
            (Format::Zip, l) => format!("zip:compression=deflate,zip:compression-level={l},zip:hdrcharset=UTF-8"),
            (Format::SevenZip, 0) => "7zip:compression=store".to_string(),
            (Format::SevenZip, l) => format!("7zip:compression=lzma2,7zip:compression-level={l}"),
        };
        let options = CString::new(options).expect("no NUL in the options");
        let name = to_utf16_nul(path.as_os_str());
        let ok = |r: i32| r == ARCHIVE_OK || r == ARCHIVE_WARN;
        unsafe {
            let r = match format {
                Format::Zip => (api.write_set_format_zip)(archive),
                Format::SevenZip => (api.write_set_format_7zip)(archive),
            };
            if !ok(r) || !ok((api.write_set_options)(archive, options.as_ptr())) {
                return Err(w.error());
            }
            if !ok((api.write_open_filename_w)(archive, name.as_ptr())) {
                return Err(w.error());
            }
        }
        w.failed = false;
        Ok(w)
    }

    /// Start the next entry: `path` inside the archive (`/`-separated), a folder or a file whose
    /// `size` bytes follow with `write`. `mtime` in seconds since 1970.
    pub fn add(&mut self, path: &str, dir: bool, size: u64, mtime: i64) -> Result<(), String> {
        let (api, e) = (self.api, self.entry);
        let name: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let r = unsafe {
            (api.entry_clear)(e);
            (api.entry_copy_pathname_w)(e, name.as_ptr());
            (api.entry_set_filetype)(e, if dir { AE_IFDIR } else { AE_IFREG } as u32);
            (api.entry_set_perm)(e, if dir { 0o755 } else { 0o644 });
            (api.entry_set_size)(e, if dir { 0 } else { size as i64 });
            (api.entry_set_mtime)(e, mtime, 0);
            (api.write_header)(self.archive, e)
        };
        if r == ARCHIVE_OK || r == ARCHIVE_WARN { Ok(()) } else { Err(self.fail()) }
    }

    /// Data of the current file entry.
    pub fn write(&mut self, mut data: &[u8]) -> Result<(), String> {
        while !data.is_empty() {
            let n = unsafe { (self.api.write_data)(self.archive, data.as_ptr().cast(), data.len()) };
            if n <= 0 {
                return Err(self.fail());
            }
            data = &data[(n as usize).min(data.len())..];
        }
        Ok(())
    }

    /// Complete the archive (7z writes most of it only now).
    pub fn finish(mut self) -> Result<(), String> {
        let r = unsafe { (self.api.write_close)(self.archive) };
        if r == ARCHIVE_OK || r == ARCHIVE_WARN { Ok(()) } else { Err(self.fail()) }
    }

    /// The last error; from now on the archive is not completed on drop.
    fn fail(&mut self) -> String {
        self.failed = true;
        self.error()
    }

    fn error(&self) -> String {
        error_of(self.api, self.archive)
    }
}
