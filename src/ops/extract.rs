//! Extracting from an archive (`Job::Extract`): one pass over the archive, writing every entry
//! at or below one of the selected items. Entry paths that would leave the destination
//! (`..`, absolute paths) are never written.

use crate::i18n::t;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use super::{Cancelled, Worker, clear_readonly, error_text};
use crate::files::archive::{self, clean_path};
use crate::sys::libarchive::{EntryType, Reader};

/// A selected item: its path inside the archive and where it is extracted to.
struct Wanted {
    inner: String,
    target: PathBuf,
    found: bool,
    failed: bool,
}

impl Wanted {
    /// `path` (inside the archive) is this item or lies below it; returns the rest of the path.
    fn rest<'a>(&self, path: &'a str) -> Option<&'a str> {
        let head = path.get(..self.inner.len())?;
        let rest = &path[self.inner.len()..];
        (head.eq_ignore_ascii_case(&self.inner) && (rest.is_empty() || rest.starts_with('\\')))
            .then(|| rest.trim_start_matches('\\'))
    }
}

impl Worker {
    /// Extract `sources` (paths inside one archive, as shown in the panel) to `dest`, which is
    /// treated like the destination of a copy.
    pub(super) fn extract(&mut self, sources: &[PathBuf], dest: &Path) -> Result<(), Cancelled> {
        let Some((archive_path, _)) = sources.first().and_then(|s| archive::split(s)) else {
            self.error(dest, t("error.not_in_archive"));
            return Ok(());
        };
        let ends_with_sep = dest.as_os_str().to_string_lossy().ends_with(['\\', '/']);
        let many = sources.len() > 1 || ends_with_sep;
        if many
            && !dest.exists()
            && let Err(e) = std::fs::create_dir_all(dest)
        {
            self.error(dest, error_text(&e));
            return Ok(());
        }
        let mut wanted = Vec::new();
        for s in sources {
            match (archive::split(s), Self::target_for(s, dest, many)) {
                (Some((a, inner)), Some(target)) if a == archive_path && !inner.is_empty() => {
                    wanted.push(Wanted { inner, target, found: false, failed: false });
                }
                _ => self.error(s, t("error.other_archive")),
            }
        }

        // Progress totals from the (usually cached) table of contents.
        if let Ok(index) = archive::index(&archive_path, &self.cancel) {
            let inner: Vec<String> = wanted.iter().map(|w| w.inner.clone()).collect();
            (self.progress.files_total, self.progress.bytes_total) = index.totals(&inner);
        }
        self.post_progress(true);

        let mut reader = match Reader::open(&archive_path) {
            Ok(r) => r,
            Err(e) => {
                self.error(&archive_path, e);
                return Ok(());
            }
        };
        let mut buf = vec![0u8; 256 * 1024];
        while let Some(header) = reader.next() {
            self.check()?;
            let h = match header {
                Ok(h) => h,
                Err(e) => {
                    self.error(&archive_path, e);
                    break;
                }
            };
            let Some(path) = clean_path(&h.path) else { continue };
            let Some((i, rest)) = wanted.iter().enumerate().find_map(|(i, w)| w.rest(&path).map(|r| (i, r))) else {
                continue;
            };
            let out = if rest.is_empty() { wanted[i].target.clone() } else { wanted[i].target.join(rest) };
            wanted[i].found = true;
            let errors_before = self.summary.errors.len();
            match h.kind {
                EntryType::Dir => {
                    if let Err(e) = std::fs::create_dir_all(&out) {
                        self.error(&out, error_text(&e));
                    }
                }
                EntryType::Other => {}
                EntryType::File if h.encrypted => {
                    self.error(&out, t("error.encrypted_entry"));
                    self.skip_file(h.size);
                }
                EntryType::File => self.extract_file(&mut reader, &out, h.size, h.mtime, &mut buf)?,
            }
            if self.summary.errors.len() != errors_before {
                wanted[i].failed = true;
            }
        }

        for w in &wanted {
            if !w.found {
                self.error(&w.target, t("error.missing_in_archive"));
            } else if !w.failed {
                self.summary.done += 1;
            }
        }
        if let [w] = wanted.as_slice()
            && w.found
            && !w.failed
        {
            self.summary.select = Some(w.target.clone());
        }
        self.post_progress(true);
        Ok(())
    }

    /// Write the current entry's data to `out` (asks first if `out` exists). Failures are
    /// reported and leave no partial file behind.
    fn extract_file(
        &mut self,
        reader: &mut Reader,
        out: &Path,
        size: u64,
        mtime: i64,
        buf: &mut [u8],
    ) -> Result<(), Cancelled> {
        self.progress.current = out.to_path_buf();
        if let Some(parent) = out.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            self.error(out, error_text(&e));
            self.skip_file(size);
            return Ok(());
        }
        if let Ok(meta) = std::fs::symlink_metadata(out) {
            if meta.is_dir() {
                self.error(out, t("error.folder_exists"));
                self.skip_file(size);
                return Ok(());
            }
            if !self.resolve_conflict(out)? {
                self.skip_file(size);
                return Ok(());
            }
            clear_readonly(out);
        }
        let mut file = match File::create(out) {
            Ok(f) => f,
            Err(e) => {
                self.error(out, error_text(&e));
                self.skip_file(size);
                return Ok(());
            }
        };
        let mut written = 0u64;
        let failure = loop {
            if self.check().is_err() {
                drop(file);
                let _ = std::fs::remove_file(out);
                return Err(Cancelled);
            }
            match reader.read(buf) {
                Ok(0) => break None,
                Ok(n) => {
                    if let Err(e) = file.write_all(&buf[..n]) {
                        break Some(error_text(&e));
                    }
                    written += n as u64;
                    self.progress.bytes_done += n as u64;
                    self.post_progress(false);
                }
                Err(e) => break Some(e),
            }
        };
        if let Some(msg) = failure {
            drop(file);
            let _ = std::fs::remove_file(out);
            self.error(out, msg);
            self.progress.bytes_done += size.saturating_sub(written);
        } else if mtime > 0 {
            let _ = file.set_modified(UNIX_EPOCH + Duration::from_secs(mtime as u64));
        }
        self.progress.files_done += 1;
        Ok(())
    }
}
