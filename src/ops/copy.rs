//! Copying files and directory trees (`CopyFileExW`, cancellable mid-file).

use std::ffi::c_void;
use std::path::Path;
use std::sync::atomic::Ordering;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    COPYFILE_FLAGS, COPYPROGRESSROUTINE_PROGRESS, CopyFileExW, LPPROGRESS_ROUTINE_CALLBACK_REASON, PROGRESS_CANCEL,
    PROGRESS_CONTINUE,
};
use windows::core::PCWSTR;

use super::{Cancelled, MAX_DEPTH, Worker, clear_readonly, error_text};
use crate::files::names::to_utf16_nul;

struct CopyCtx<'a> {
    worker: &'a mut Worker,
    base: u64,
}

unsafe extern "system" fn copy_progress(
    _total: i64,
    transferred: i64,
    _stream_size: i64,
    _stream_transferred: i64,
    _stream: u32,
    _reason: LPPROGRESS_ROUTINE_CALLBACK_REASON,
    _src: HANDLE,
    _dst: HANDLE,
    data: *const c_void,
) -> COPYPROGRESSROUTINE_PROGRESS {
    // SAFETY: `data` is the `CopyCtx` passed to CopyFileExW, which outlives the call.
    let ctx = unsafe { &mut *(data as *mut CopyCtx) };
    if ctx.worker.cancel.load(Ordering::Relaxed) {
        return PROGRESS_CANCEL;
    }
    ctx.worker.progress.bytes_done = ctx.base + transferred as u64;
    ctx.worker.post_progress(false);
    PROGRESS_CONTINUE
}

impl Worker {
    pub(super) fn copy_tree(&mut self, src: &Path, dst: &Path, depth: usize) -> Result<(), Cancelled> {
        self.check()?;
        let m = match std::fs::metadata(src) {
            Ok(m) => m,
            Err(e) => {
                self.error(src, error_text(&e));
                return Ok(());
            }
        };
        if m.is_dir() {
            if depth >= MAX_DEPTH {
                self.error(src, "Verzeichnisbaum zu tief");
                return Ok(());
            }
            if let Err(e) = std::fs::create_dir_all(dst) {
                self.error(dst, error_text(&e));
                return Ok(());
            }
            let entries = match std::fs::read_dir(src) {
                Ok(rd) => rd.flatten().collect::<Vec<_>>(),
                Err(e) => {
                    self.error(src, error_text(&e));
                    return Ok(());
                }
            };
            for e in entries {
                self.copy_tree(&e.path(), &dst.join(e.file_name()), depth + 1)?;
            }
            Ok(())
        } else {
            self.copy_file(src, dst, m.len()).map(|_| ())
        }
    }

    /// `Ok(true)` only if the file was actually copied.
    pub(super) fn copy_file(&mut self, src: &Path, dst: &Path, len: u64) -> Result<bool, Cancelled> {
        self.progress.current = src.to_path_buf();
        if let Ok(dm) = std::fs::symlink_metadata(dst) {
            if dm.is_dir() {
                self.error(dst, "Ziel ist ein Ordner");
                self.skip_file(len);
                return Ok(false);
            }
            if !self.resolve_conflict(dst)? {
                self.skip_file(len);
                return Ok(false);
            }
            clear_readonly(dst);
        }
        let base = self.progress.bytes_done;
        let (s, d) = (to_utf16_nul(src.as_os_str()), to_utf16_nul(dst.as_os_str()));
        let mut ctx = CopyCtx { worker: self, base };
        let res = unsafe {
            CopyFileExW(
                PCWSTR(s.as_ptr()),
                PCWSTR(d.as_ptr()),
                Some(copy_progress),
                Some(&mut ctx as *mut CopyCtx as *const c_void),
                None,
                COPYFILE_FLAGS(0),
            )
        };
        self.check()?;
        let ok = match res {
            Ok(()) => true,
            Err(e) => {
                self.error(src, e.message().trim());
                false
            }
        };
        self.progress.bytes_done = base + len;
        self.progress.files_done += 1;
        self.post_progress(false);
        Ok(ok)
    }

    fn skip_file(&mut self, len: u64) {
        self.progress.bytes_done += len;
        self.progress.files_done += 1;
    }
}
