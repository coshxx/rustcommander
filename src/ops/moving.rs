//! Moving: a rename on the same volume, copy + delete across volumes; merges into
//! existing directories.

use std::path::Path;

use windows::Win32::Foundation::ERROR_NOT_SAME_DEVICE;
use windows::Win32::Storage::FileSystem::{MOVE_FILE_FLAGS, MOVEFILE_REPLACE_EXISTING, MoveFileExW};
use windows::core::PCWSTR;

use crate::files::names::to_wide_z;
use super::{Cancelled, MAX_DEPTH, Worker, clear_readonly, err_text, is_reparse, scan};

impl Worker {
    pub(super) fn move_item(&mut self, src: &Path, dst: &Path, fast: bool) -> Result<(), Cancelled> {
        self.check()?;
        self.progress.current = src.to_path_buf();
        if let Ok(dm) = std::fs::symlink_metadata(dst) {
            let src_dir = std::fs::symlink_metadata(src).map(|m| m.is_dir()).unwrap_or(false);
            if src_dir && dm.is_dir() {
                // Merge directories: move the children, then remove the source if empty.
                let entries = match std::fs::read_dir(src) {
                    Ok(rd) => rd.flatten().collect::<Vec<_>>(),
                    Err(e) => {
                        self.error(src, err_text(&e));
                        return Ok(());
                    }
                };
                for e in entries {
                    self.move_item(&e.path(), &dst.join(e.file_name()), fast)?;
                }
                let _ = std::fs::remove_dir(src);
                return Ok(());
            }
            if dm.is_dir() != src_dir {
                self.error(dst, "Ziel existiert mit anderem Typ (Datei/Ordner)");
                return Ok(());
            }
            if !self.resolve_conflict(dst)? {
                if fast {
                    self.progress.files_done += 1;
                } else {
                    let (mut f, mut b) = (0, 0);
                    scan(src, 0, &mut f, &mut b);
                    self.progress.files_done += f;
                    self.progress.bytes_done += b;
                }
                return Ok(());
            }
            clear_readonly(dst);
            return self.rename_or_cross(src, dst, MOVEFILE_REPLACE_EXISTING, fast);
        }
        self.rename_or_cross(src, dst, MOVE_FILE_FLAGS(0), fast)
    }

    fn rename_or_cross(&mut self, src: &Path, dst: &Path, flags: MOVE_FILE_FLAGS, fast: bool) -> Result<(), Cancelled> {
        let (s, d) = (to_wide_z(src.as_os_str()), to_wide_z(dst.as_os_str()));
        match unsafe { MoveFileExW(PCWSTR(s.as_ptr()), PCWSTR(d.as_ptr()), flags) } {
            Ok(()) => {
                if fast {
                    self.progress.files_done += 1;
                } else {
                    let (mut f, mut b) = (0, 0);
                    scan(dst, 0, &mut f, &mut b);
                    self.progress.files_done += f;
                    self.progress.bytes_done += b;
                }
                self.post_progress(false);
                Ok(())
            }
            Err(e) if e.code() == ERROR_NOT_SAME_DEVICE.to_hresult() => self.move_cross(src, dst, 0),
            Err(e) => {
                self.error(src, e.message().trim());
                Ok(())
            }
        }
    }

    /// Move across volumes: copy each file, delete it only after a successful copy.
    fn move_cross(&mut self, src: &Path, dst: &Path, depth: usize) -> Result<(), Cancelled> {
        self.check()?;
        let m = match std::fs::symlink_metadata(src) {
            Ok(m) => m,
            Err(e) => {
                self.error(src, err_text(&e));
                return Ok(());
            }
        };
        if is_reparse(&m) {
            self.error(src, "Verknüpfungen können nicht laufwerksübergreifend verschoben werden");
            return Ok(());
        }
        if m.is_dir() {
            if depth >= MAX_DEPTH {
                self.error(src, "Verzeichnisbaum zu tief");
                return Ok(());
            }
            if let Err(e) = std::fs::create_dir_all(dst) {
                self.error(dst, err_text(&e));
                return Ok(());
            }
            let entries = match std::fs::read_dir(src) {
                Ok(rd) => rd.flatten().collect::<Vec<_>>(),
                Err(e) => {
                    self.error(src, err_text(&e));
                    return Ok(());
                }
            };
            for e in entries {
                self.move_cross(&e.path(), &dst.join(e.file_name()), depth + 1)?;
            }
            // Fails (and keeps the directory) if something was skipped or failed.
            let _ = std::fs::remove_dir(src);
            return Ok(());
        }
        let copied = self.copy_file(src, dst, m.len())?;
        if copied && std::fs::metadata(dst).map(|d| d.len() == m.len()).unwrap_or(false) {
            clear_readonly(src);
            if let Err(e) = std::fs::remove_file(src) {
                self.error(src, err_text(&e));
            }
        }
        Ok(())
    }

    // ---------- delete ----------
}
