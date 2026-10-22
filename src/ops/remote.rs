//! Operations involving an SFTP server: copying to it (upload) and from it (download), and
//! moving, renaming, deleting and creating folders on it. The connection comes from
//! `sftp::session`, shared with the panels.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{Cancelled, MAX_DEPTH, Worker, clear_readonly, error_text, scan};
use crate::files::location::Location;
use crate::i18n::t;
use crate::sftp::{self, RemotePath, Session};

/// Seconds since 1970 (SFTP v3 keeps 32 bits).
fn unix_seconds(t: SystemTime) -> u32 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()).min(u32::MAX as u64) as u32
}

/// `sftp://host/path` as a `PathBuf`, for progress and the conflict question (display only).
fn shown(p: &RemotePath) -> PathBuf {
    PathBuf::from(p.to_string())
}

impl Worker {
    fn remote_error(&mut self, path: &RemotePath, msg: impl std::fmt::Display) {
        self.summary.errors.push(format!("{path}: {msg}"));
    }

    /// The connection to `host`; on failure the error is recorded and `None` returned.
    fn session(&mut self, host: &str) -> Option<Arc<Session>> {
        match sftp::session(host) {
            Ok(s) => Some(s),
            Err(e) => {
                self.summary.errors.push(format!("{host}: {e}"));
                None
            }
        }
    }

    /// Count files and bytes below a remote path (following links, like the copy does).
    fn remote_scan(&self, s: &Session, path: &RemotePath, depth: usize, files: &mut u64, bytes: &mut u64) {
        let Ok(a) = s.stat(&path.path) else { return };
        if !a.is_dir() {
            *files += 1;
            *bytes += a.size.unwrap_or(0);
        } else if depth < MAX_DEPTH
            && let Ok(names) = s.read_dir(&path.path, &|| self.cancel.load(Ordering::Relaxed))
        {
            for n in names {
                self.remote_scan(s, &path.join(&n.name), depth + 1, files, bytes);
            }
        }
    }

    // ---------- upload ----------

    /// Copy local `sources` to the server. `into` (or several sources, or an existing folder at
    /// `dest`): into `dest`; otherwise exactly to `dest` (a new name).
    pub(super) fn upload(&mut self, sources: &[PathBuf], dest: &RemotePath, into: bool) -> Result<(), Cancelled> {
        let Some(s) = self.session(&dest.host) else { return Ok(()) };
        let dest_dir = matches!(s.stat(&dest.path), Ok(a) if a.is_dir());
        let into = into || sources.len() > 1 || dest_dir;
        if into && !dest_dir && !self.remote_mkdirs(&s, dest) {
            return Ok(());
        }
        for src in sources {
            scan(src, 0, &mut self.progress.files_total, &mut self.progress.bytes_total);
        }
        self.post_progress(true);
        for src in sources {
            self.check()?;
            let Some(name) = src.file_name() else {
                self.error(src, t("error.invalid_source"));
                continue;
            };
            let target = if into { dest.join(&name.to_string_lossy()) } else { dest.clone() };
            let errors_before = self.summary.errors.len();
            self.upload_tree(&s, src, &target, 0)?;
            if self.summary.errors.len() == errors_before {
                self.summary.done += 1;
                self.summary.select.get_or_insert(Location::Remote(target));
            }
        }
        self.post_progress(true);
        Ok(())
    }

    fn upload_tree(&mut self, s: &Session, src: &Path, dst: &RemotePath, depth: usize) -> Result<(), Cancelled> {
        self.check()?;
        let m = match std::fs::metadata(src) {
            Ok(m) => m,
            Err(e) => {
                self.error(src, error_text(&e));
                return Ok(());
            }
        };
        if !m.is_dir() {
            return self.upload_file(s, src, dst, &m).map(|_| ());
        }
        if depth >= MAX_DEPTH {
            self.error(src, t("error.too_deep"));
            return Ok(());
        }
        match s.lstat(&dst.path) {
            Ok(Some(a)) if a.is_dir() => {}
            Ok(Some(_)) => {
                self.remote_error(dst, t("error.type_mismatch"));
                return Ok(());
            }
            Ok(None) => {
                if let Err(e) = s.mkdir(&dst.path) {
                    self.remote_error(dst, e);
                    return Ok(());
                }
            }
            Err(e) => {
                self.remote_error(dst, e);
                return Ok(());
            }
        }
        let entries = match std::fs::read_dir(src) {
            Ok(rd) => rd.flatten().collect::<Vec<_>>(),
            Err(e) => {
                self.error(src, error_text(&e));
                return Ok(());
            }
        };
        for e in entries {
            self.upload_tree(s, &e.path(), &dst.join(&e.file_name().to_string_lossy()), depth + 1)?;
        }
        Ok(())
    }

    /// `Ok(true)` only if the file was uploaded completely; a partial file is removed again.
    fn upload_file(
        &mut self,
        s: &Session,
        src: &Path,
        dst: &RemotePath,
        m: &std::fs::Metadata,
    ) -> Result<bool, Cancelled> {
        self.progress.current = src.to_path_buf();
        let len = m.len();
        match s.lstat(&dst.path) {
            Ok(Some(a)) if a.is_dir() => {
                self.remote_error(dst, t("error.target_is_folder"));
                self.skip_file(len);
                return Ok(false);
            }
            Ok(Some(_)) if !self.resolve_conflict(&shown(dst))? => {
                self.skip_file(len);
                return Ok(false);
            }
            Ok(_) => {}
            Err(e) => {
                self.remote_error(dst, e);
                self.skip_file(len);
                return Ok(false);
            }
        }
        let mut file = match File::open(src) {
            Ok(f) => f,
            Err(e) => {
                self.error(src, error_text(&e));
                self.skip_file(len);
                return Ok(false);
            }
        };
        let base = self.progress.bytes_done;
        let cancel = self.cancel.clone();
        let mut read_error = None;
        let result = s.upload(&dst.path, &mut |buf| {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            match file.read(buf) {
                Ok(n) => {
                    self.progress.bytes_done += n as u64;
                    self.post_progress(false);
                    Some(n)
                }
                Err(e) => {
                    read_error = Some(e);
                    None
                }
            }
        });
        let complete = match result {
            Ok(true) => true,
            Ok(false) => {
                if let Some(e) = read_error {
                    self.error(src, error_text(&e));
                }
                false
            }
            Err(e) => {
                self.remote_error(dst, e);
                false
            }
        };
        if complete {
            if let Ok(mtime) = m.modified() {
                let secs = unix_seconds(mtime);
                let _ = s.set_times(&dst.path, secs, secs);
            }
        } else {
            let _ = s.remove(&dst.path);
        }
        self.progress.bytes_done = base + len;
        self.progress.files_done += 1;
        self.post_progress(false);
        self.check()?;
        Ok(complete)
    }

    // ---------- download ----------

    /// Copy `sources` (all on one server) to the local `dest`, with the same "into" rules as a
    /// local copy.
    pub(super) fn download(&mut self, sources: &[RemotePath], dest: &Path) -> Result<(), Cancelled> {
        let Some(host) = sources.first().map(|s| s.host.clone()) else { return Ok(()) };
        let Some(s) = self.session(&host) else { return Ok(()) };
        let ends_with_sep = dest.as_os_str().to_string_lossy().ends_with(['\\', '/']);
        let into = sources.len() > 1 || ends_with_sep || dest.is_dir();
        if into
            && !dest.exists()
            && let Err(e) = std::fs::create_dir_all(dest)
        {
            self.error(dest, error_text(&e));
            return Ok(());
        }
        for src in sources {
            let (mut f, mut b) = (0, 0);
            self.remote_scan(&s, src, 0, &mut f, &mut b);
            self.progress.files_total += f;
            self.progress.bytes_total += b;
        }
        self.post_progress(true);
        for src in sources {
            self.check()?;
            let target = if into { dest.join(src.name()) } else { dest.to_path_buf() };
            let errors_before = self.summary.errors.len();
            self.download_tree(&s, src, &target, 0)?;
            if self.summary.errors.len() == errors_before {
                self.summary.done += 1;
                self.summary.select.get_or_insert(Location::Local(target));
            }
        }
        self.post_progress(true);
        Ok(())
    }

    fn download_tree(&mut self, s: &Session, src: &RemotePath, dst: &Path, depth: usize) -> Result<(), Cancelled> {
        self.check()?;
        let a = match s.stat(&src.path) {
            Ok(a) => a,
            Err(e) => {
                self.remote_error(src, e);
                return Ok(());
            }
        };
        if !a.is_dir() {
            return self.download_file(s, src, dst, a.size.unwrap_or(0), a.mtime).map(|_| ());
        }
        if depth >= MAX_DEPTH {
            self.remote_error(src, t("error.too_deep"));
            return Ok(());
        }
        if let Err(e) = std::fs::create_dir_all(dst) {
            self.error(dst, error_text(&e));
            return Ok(());
        }
        let names = match s.read_dir(&src.path, &|| self.cancel.load(Ordering::Relaxed)) {
            Ok(n) => n,
            Err(e) => {
                self.check()?;
                self.remote_error(src, e);
                return Ok(());
            }
        };
        for n in names {
            self.download_tree(s, &src.join(&n.name), &dst.join(&n.name), depth + 1)?;
        }
        Ok(())
    }

    /// `Ok(true)` only if the file was downloaded completely; a partial file is removed again.
    fn download_file(
        &mut self,
        s: &Session,
        src: &RemotePath,
        dst: &Path,
        len: u64,
        mtime: Option<u32>,
    ) -> Result<bool, Cancelled> {
        self.progress.current = shown(src);
        if let Ok(dm) = std::fs::symlink_metadata(dst) {
            if dm.is_dir() {
                self.error(dst, t("error.target_is_folder"));
                self.skip_file(len);
                return Ok(false);
            }
            if !self.resolve_conflict(dst)? {
                self.skip_file(len);
                return Ok(false);
            }
            clear_readonly(dst);
        }
        let mut file = match File::create(dst) {
            Ok(f) => f,
            Err(e) => {
                self.error(dst, error_text(&e));
                self.skip_file(len);
                return Ok(false);
            }
        };
        let base = self.progress.bytes_done;
        let cancel = self.cancel.clone();
        let mut write_error = None;
        let result = s.download(&src.path, &mut |data| {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            if let Err(e) = file.write_all(data) {
                write_error = Some(e);
                return false;
            }
            self.progress.bytes_done += data.len() as u64;
            self.post_progress(false);
            true
        });
        let complete = match result {
            Ok(true) => true,
            Ok(false) => {
                if let Some(e) = write_error {
                    self.error(dst, error_text(&e));
                }
                false
            }
            Err(e) => {
                self.remote_error(src, e);
                false
            }
        };
        if complete {
            if let Some(m) = mtime {
                let _ = file.set_modified(UNIX_EPOCH + Duration::from_secs(m as u64));
            }
            drop(file);
        } else {
            drop(file);
            let _ = std::fs::remove_file(dst);
        }
        self.progress.bytes_done = base + len;
        self.progress.files_done += 1;
        self.post_progress(false);
        self.check()?;
        Ok(complete)
    }

    // ---------- on the server ----------

    /// Create `path` and every missing folder above it; `false` (error recorded) on failure.
    fn remote_mkdirs(&mut self, s: &Session, path: &RemotePath) -> bool {
        let mut missing = Vec::new();
        let mut p = Some(path.clone());
        while let Some(cur) = p {
            match s.lstat(&cur.path) {
                Ok(Some(a)) if a.is_dir() => break,
                Ok(Some(_)) => {
                    self.remote_error(&cur, t("error.type_mismatch"));
                    return false;
                }
                Ok(None) => {
                    p = cur.parent();
                    missing.push(cur);
                }
                Err(e) => {
                    self.remote_error(&cur, e);
                    return false;
                }
            }
        }
        for dir in missing.iter().rev() {
            if let Err(e) = s.mkdir(&dir.path) {
                self.remote_error(dir, e);
                return false;
            }
        }
        true
    }

    /// F7 on a server: `a/b` creates nested folders.
    pub(super) fn remote_mkdir(&mut self, path: &RemotePath) {
        let Some(s) = self.session(&path.host) else { return };
        if self.remote_mkdirs(&s, path) {
            self.summary.done = 1;
            self.summary.select = Some(Location::Remote(path.clone()));
        }
    }

    pub(super) fn remote_rename(&mut self, from: &RemotePath, to: &str) {
        let Some(s) = self.session(&from.host) else { return };
        let Some(target) = from.parent().map(|p| p.join(to)) else {
            self.remote_error(from, t("error.invalid_source"));
            return;
        };
        match s.lstat(&target.path) {
            Ok(Some(_)) => self.remote_error(&target, t("error.exists")),
            Ok(None) => match s.rename(&from.path, &target.path) {
                Ok(()) => {
                    self.summary.done = 1;
                    self.summary.select = Some(Location::Remote(target));
                }
                Err(e) => self.remote_error(from, e),
            },
            Err(e) => self.remote_error(&target, e),
        }
    }

    /// F6 on one server: renames. An existing file at the target is replaced after asking; an
    /// existing folder is an error (no merging on the server).
    pub(super) fn remote_move(
        &mut self,
        sources: &[RemotePath],
        dest: &RemotePath,
        into: bool,
    ) -> Result<(), Cancelled> {
        let Some(s) = self.session(&dest.host) else { return Ok(()) };
        let dest_dir = matches!(s.stat(&dest.path), Ok(a) if a.is_dir());
        let into = into || sources.len() > 1 || dest_dir;
        if into && !dest_dir && !self.remote_mkdirs(&s, dest) {
            return Ok(());
        }
        self.progress.files_total = sources.len() as u64;
        self.post_progress(true);
        for src in sources {
            self.check()?;
            self.progress.current = shown(src);
            let target = if into { dest.join(src.name()) } else { dest.clone() };
            if target == *src {
                self.remote_error(src, t("error.same_path"));
            } else if target.starts_with(src) {
                self.remote_error(src, t("error.into_itself"));
            } else {
                match s.lstat(&target.path) {
                    Ok(None) => self.remote_rename_to(&s, src, &target),
                    Ok(Some(a)) if a.is_dir() => self.remote_error(&target, t("error.folder_exists")),
                    Ok(Some(_)) => {
                        if self.resolve_conflict(&shown(&target))? {
                            match s.remove(&target.path) {
                                Ok(()) => self.remote_rename_to(&s, src, &target),
                                Err(e) => self.remote_error(&target, e),
                            }
                        }
                    }
                    Err(e) => self.remote_error(&target, e),
                }
            }
            self.progress.files_done += 1;
            self.post_progress(false);
        }
        self.post_progress(true);
        Ok(())
    }

    fn remote_rename_to(&mut self, s: &Session, src: &RemotePath, target: &RemotePath) {
        match s.rename(&src.path, &target.path) {
            Ok(()) => {
                self.summary.done += 1;
                self.summary.select.get_or_insert(Location::Remote(target.clone()));
            }
            Err(e) => self.remote_error(src, e),
        }
    }

    /// Delete for good (a server has no recycle bin). Links are removed, never followed.
    pub(super) fn remote_delete(&mut self, sources: &[RemotePath]) -> Result<(), Cancelled> {
        let Some(host) = sources.first().map(|s| s.host.clone()) else { return Ok(()) };
        let Some(s) = self.session(&host) else { return Ok(()) };
        self.progress.files_total = sources.len() as u64;
        self.post_progress(true);
        for src in sources {
            self.check()?;
            let errors_before = self.summary.errors.len();
            self.remote_delete_tree(&s, src, 0)?;
            if self.summary.errors.len() == errors_before {
                self.summary.done += 1;
            }
            self.progress.files_done += 1;
            self.post_progress(false);
        }
        self.post_progress(true);
        Ok(())
    }

    fn remote_delete_tree(&mut self, s: &Session, path: &RemotePath, depth: usize) -> Result<(), Cancelled> {
        self.check()?;
        self.progress.current = shown(path);
        self.post_progress(false);
        let a = match s.lstat(&path.path) {
            Ok(Some(a)) => a,
            Ok(None) => return Ok(()),
            Err(e) => {
                self.remote_error(path, e);
                return Ok(());
            }
        };
        if !a.is_dir() {
            if let Err(e) = s.remove(&path.path) {
                self.remote_error(path, e);
            }
            return Ok(());
        }
        if depth >= MAX_DEPTH {
            self.remote_error(path, t("error.too_deep"));
            return Ok(());
        }
        let names = match s.read_dir(&path.path, &|| self.cancel.load(Ordering::Relaxed)) {
            Ok(n) => n,
            Err(e) => {
                self.check()?;
                self.remote_error(path, e);
                return Ok(());
            }
        };
        for n in names {
            self.remote_delete_tree(s, &path.join(&n.name), depth + 1)?;
        }
        if let Err(e) = s.rmdir(&path.path) {
            self.remote_error(path, e);
        }
        Ok(())
    }
}
