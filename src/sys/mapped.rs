//! A whole file mapped into memory, read-only. Opening is instant whatever the size; pages
//! are read from disk only when touched. Used by the lister.

use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, PAGE_READONLY, UnmapViewOfFile,
};

/// FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE: viewing never blocks other programs.
const SHARE_ALL: u32 = 7;

pub struct MappedFile {
    _file: File,
    map: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    len: usize,
}

// SAFETY: the view is read-only and stays mapped until `drop`; reading it from several threads
// is fine.
unsafe impl Send for MappedFile {}
unsafe impl Sync for MappedFile {}

impl MappedFile {
    pub fn open(path: &Path) -> std::io::Result<MappedFile> {
        let file = OpenOptions::new().read(true).share_mode(SHARE_ALL).open(path)?;
        let meta = file.metadata()?;
        if meta.is_dir() {
            return Err(std::io::Error::other("ist ein Ordner"));
        }
        let len = usize::try_from(meta.len()).map_err(std::io::Error::other)?;
        let mut mapped = MappedFile { _file: file, map: HANDLE::default(), view: Default::default(), len: 0 };
        // An empty file can't be mapped – and needn't be.
        if len == 0 {
            return Ok(mapped);
        }
        unsafe {
            let handle = HANDLE(mapped._file.as_raw_handle());
            mapped.map = CreateFileMappingW(handle, None, PAGE_READONLY, 0, 0, None)?;
            mapped.view = MapViewOfFile(mapped.map, FILE_MAP_READ, 0, 0, 0);
        }
        if mapped.view.Value.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        mapped.len = len;
        Ok(mapped)
    }

    /// The file's contents. Windows refuses to shorten a file while it is mapped, so the slice
    /// stays valid; another program may still change bytes in it.
    pub fn bytes(&self) -> &[u8] {
        if self.len == 0 {
            return &[];
        }
        // SAFETY: `view` maps exactly `len` readable bytes until `drop`.
        unsafe { std::slice::from_raw_parts(self.view.Value as *const u8, self.len) }
    }
}

impl Drop for MappedFile {
    fn drop(&mut self) {
        unsafe {
            if !self.view.Value.is_null() {
                let _ = UnmapViewOfFile(self.view);
            }
            if !self.map.is_invalid() {
                let _ = CloseHandle(self.map);
            }
        }
    }
}
