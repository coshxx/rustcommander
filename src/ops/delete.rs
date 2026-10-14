//! Deleting: to the recycle bin via the shell, or permanently without following links.

use std::path::{Path, PathBuf};

use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::UI::Shell::{
    FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_WANTNUKEWARNING, SHFILEOPSTRUCTW, SHFileOperationW,
};
use windows::core::PCWSTR;

use super::{Cancelled, MAX_DEPTH, Worker, clear_readonly, error_text, is_reparse};
use crate::files::names::to_utf16_nul;

fn count_no_follow(path: &Path, depth: usize, files: &mut u64) {
    let Ok(m) = std::fs::symlink_metadata(path) else { return };
    if m.is_dir() && !is_reparse(&m) {
        if depth < MAX_DEPTH
            && let Ok(rd) = std::fs::read_dir(path)
        {
            for e in rd.flatten() {
                count_no_follow(&e.path(), depth + 1, files);
            }
        }
    } else if !m.is_dir() {
        *files += 1;
    }
}

impl Worker {
    pub(super) fn delete_all(&mut self, sources: &[PathBuf]) -> Result<(), Cancelled> {
        for s in sources {
            count_no_follow(s, 0, &mut self.progress.files_total);
        }
        self.post_progress(true);
        for s in sources {
            self.check()?;
            let errors_before = self.summary.errors.len();
            self.delete_tree(s, 0)?;
            if self.summary.errors.len() == errors_before {
                self.summary.done += 1;
            }
        }
        self.post_progress(true);
        Ok(())
    }

    /// Never follows links: a junction/symlink is removed, not its target's contents.
    fn delete_tree(&mut self, path: &Path, depth: usize) -> Result<(), Cancelled> {
        self.check()?;
        self.progress.current = path.to_path_buf();
        let m = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) => {
                self.error(path, error_text(&e));
                return Ok(());
            }
        };
        if m.is_dir() && !is_reparse(&m) {
            if depth >= MAX_DEPTH {
                self.error(path, "Verzeichnisbaum zu tief");
                return Ok(());
            }
            let entries = match std::fs::read_dir(path) {
                Ok(rd) => rd.flatten().collect::<Vec<_>>(),
                Err(e) => {
                    self.error(path, error_text(&e));
                    return Ok(());
                }
            };
            for e in entries {
                self.delete_tree(&e.path(), depth + 1)?;
            }
        }
        clear_readonly(path);
        let res = if m.is_dir() { std::fs::remove_dir(path) } else { std::fs::remove_file(path) };
        if let Err(e) = res {
            self.error(path, error_text(&e));
        }
        if !m.is_dir() {
            self.progress.files_done += 1;
        }
        self.post_progress(false);
        Ok(())
    }

    pub(super) fn recycle(&mut self, sources: &[PathBuf]) {
        self.progress.files_total = sources.len() as u64;
        self.post_progress(true);
        // Double-null-terminated list of paths.
        let mut from: Vec<u16> = Vec::new();
        for s in sources {
            from.extend(to_utf16_nul(s.as_os_str()));
        }
        from.push(0);
        let mut op = SHFILEOPSTRUCTW {
            wFunc: FO_DELETE,
            pFrom: PCWSTR(from.as_ptr()),
            // Recycle; if that is impossible, the shell asks before deleting for good.
            fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_WANTNUKEWARNING | FOF_NOERRORUI).0 as u16,
            ..Default::default()
        };
        let res = unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            SHFileOperationW(&mut op)
        };
        if op.fAnyOperationsAborted.as_bool() {
            self.summary.cancelled = true;
        } else if res != 0 {
            self.summary.errors.push(format!("Papierkorb: Fehler 0x{res:x}"));
        } else {
            self.summary.done = sources.len() as u64;
        }
        self.progress.files_done = self.progress.files_total;
        self.post_progress(true);
    }
}
