//! Packing files and folders into a new zip or 7z archive (`Job::Pack`) and unpacking whole
//! archives (`Job::Unpack`).

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use super::{Cancelled, MAX_DEPTH, Worker, error_text, scan};
use crate::files::archive::{self, clean_path};
use crate::i18n::t;
use crate::sys::libarchive::{EntryType, Reader, Writer};

pub use crate::sys::libarchive::Format;

/// Compression level offered in the pack dialog.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Level {
    Store,
    Fast,
    #[default]
    Normal,
    Max,
}

impl Level {
    pub const ALL: [Level; 4] = [Level::Store, Level::Fast, Level::Normal, Level::Max];

    /// Name in config.json; the label is `pack.<id>`.
    pub fn id(self) -> &'static str {
        match self {
            Level::Store => "store",
            Level::Fast => "fast",
            Level::Normal => "normal",
            Level::Max => "max",
        }
    }

    pub fn from_id(id: &str) -> Option<Level> {
        Level::ALL.into_iter().find(|l| l.id() == id)
    }

    /// libarchive's compression level (0 = store).
    fn value(self) -> u8 {
        match self {
            Level::Store => 0,
            Level::Fast => 1,
            Level::Normal => 6,
            Level::Max => 9,
        }
    }
}

/// Seconds since 1970 of a file's last change (0 if unknown).
fn mtime(m: &std::fs::Metadata) -> i64 {
    m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64)
}

/// Why packing ended early.
enum Stop {
    Cancelled,
    /// A fatal error: the archive is broken.
    Broken(String),
}

impl From<Cancelled> for Stop {
    fn from(_: Cancelled) -> Stop {
        Stop::Cancelled
    }
}

impl Worker {
    /// Pack `sources` (each at the archive's top level, folders with everything below them) into
    /// `dest`. An existing `dest` is replaced after asking. A failed or cancelled archive is
    /// deleted.
    pub(super) fn pack(
        &mut self,
        sources: &[PathBuf],
        dest: &Path,
        format: Format,
        level: Level,
    ) -> Result<(), Cancelled> {
        if let Ok(m) = std::fs::symlink_metadata(dest) {
            if m.is_dir() {
                self.error(dest, t("error.target_is_folder"));
                return Ok(());
            }
            if !self.resolve_conflict(dest)? {
                return Ok(());
            }
            super::clear_readonly(dest);
        }
        if let Some(parent) = dest.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            self.error(dest, error_text(&e));
            return Ok(());
        }
        for s in sources {
            scan(s, 0, &mut self.progress.files_total, &mut self.progress.bytes_total);
        }
        self.post_progress(true);
        let mut writer = match Writer::create(dest, format, level.value()) {
            Ok(w) => w,
            Err(e) => {
                self.error(dest, e);
                let _ = std::fs::remove_file(dest);
                return Ok(());
            }
        };
        let mut buf = vec![0u8; 256 * 1024];
        let mut stop = None;
        for src in sources {
            let Some(name) = src.file_name() else {
                self.error(src, t("error.invalid_source"));
                continue;
            };
            let errors_before = self.summary.errors.len();
            match self.pack_tree(&mut writer, src, &name.to_string_lossy(), dest, 0, &mut buf) {
                Ok(()) => {}
                Err(s) => {
                    stop = Some(s);
                    break;
                }
            }
            if self.summary.errors.len() == errors_before {
                self.summary.done += 1;
            }
        }
        if stop.is_none() {
            self.progress.current = dest.to_path_buf();
            self.post_progress(true);
            if let Err(e) = writer.finish() {
                stop = Some(Stop::Broken(e));
            }
        } else {
            drop(writer);
        }
        match stop {
            None => {
                self.summary.select = Some(dest.to_path_buf());
                self.post_progress(true);
                Ok(())
            }
            Some(s) => {
                let _ = std::fs::remove_file(dest);
                self.summary.done = 0;
                match s {
                    Stop::Cancelled => Err(Cancelled),
                    Stop::Broken(e) => {
                        self.error(dest, e);
                        Ok(())
                    }
                }
            }
        }
    }

    /// Add `src` as `inner` (`/`-separated) to the archive; folders recursively. The archive
    /// itself (`dest`) is left out when it lies inside a packed folder.
    fn pack_tree(
        &mut self,
        w: &mut Writer,
        src: &Path,
        inner: &str,
        dest: &Path,
        depth: usize,
        buf: &mut [u8],
    ) -> Result<(), Stop> {
        self.check()?;
        let m = match std::fs::metadata(src) {
            Ok(m) => m,
            Err(e) => {
                self.error(src, error_text(&e));
                return Ok(());
            }
        };
        if !m.is_dir() {
            if src == dest {
                return Ok(());
            }
            return self.pack_file(w, src, inner, &m, buf);
        }
        if depth >= MAX_DEPTH {
            self.error(src, t("error.too_deep"));
            return Ok(());
        }
        w.add(&format!("{inner}/"), true, 0, mtime(&m)).map_err(Stop::Broken)?;
        let entries = match std::fs::read_dir(src) {
            Ok(rd) => rd.flatten().collect::<Vec<_>>(),
            Err(e) => {
                self.error(src, error_text(&e));
                return Ok(());
            }
        };
        for e in entries {
            let child = format!("{inner}/{}", e.file_name().to_string_lossy());
            self.pack_tree(w, &e.path(), &child, dest, depth + 1, buf)?;
        }
        Ok(())
    }

    /// One file: a file that can't be opened is reported and left out; a read error in the
    /// middle breaks the archive.
    fn pack_file(
        &mut self,
        w: &mut Writer,
        src: &Path,
        inner: &str,
        m: &std::fs::Metadata,
        buf: &mut [u8],
    ) -> Result<(), Stop> {
        self.progress.current = src.to_path_buf();
        let len = m.len();
        let mut file = match File::open(src) {
            Ok(f) => f,
            Err(e) => {
                self.error(src, error_text(&e));
                self.skip_file(len);
                return Ok(());
            }
        };
        w.add(inner, false, len, mtime(m)).map_err(Stop::Broken)?;
        let mut left = len;
        while left > 0 {
            self.check()?;
            let want = buf.len().min(left as usize);
            let n = match file.read(&mut buf[..want]) {
                Ok(0) => return Err(Stop::Broken(format!("{}: {}", src.display(), t("error.file_changed")))),
                Ok(n) => n,
                Err(e) => return Err(Stop::Broken(format!("{}: {}", src.display(), error_text(&e)))),
            };
            w.write(&buf[..n]).map_err(Stop::Broken)?;
            left -= n as u64;
            self.progress.bytes_done += n as u64;
            self.post_progress(false);
        }
        self.progress.files_done += 1;
        Ok(())
    }

    /// Unpack each archive completely into `dest`, or with `subdir` into `dest\<archive name>`.
    pub(super) fn unpack(&mut self, archives: &[PathBuf], dest: &Path, subdir: bool) -> Result<(), Cancelled> {
        for a in archives {
            if let Ok(index) = archive::index(a, &self.cancel) {
                let (files, bytes) = index.totals(&[String::new()]);
                self.progress.files_total += files;
                self.progress.bytes_total += bytes;
            }
        }
        self.post_progress(true);
        let mut buf = vec![0u8; 256 * 1024];
        let mut targets = Vec::new();
        for a in archives {
            self.check()?;
            let target = match (subdir, a.file_name()) {
                (true, Some(name)) => dest.join(archive::stem(&name.to_string_lossy())),
                _ => dest.to_path_buf(),
            };
            let errors_before = self.summary.errors.len();
            self.unpack_one(a, &target, &mut buf)?;
            if self.summary.errors.len() == errors_before {
                self.summary.done += 1;
                targets.push(target);
            }
        }
        // One archive into its own folder: the cursor goes onto that folder.
        if subdir && let [t] = targets.as_slice() {
            self.summary.select = Some(t.clone());
        }
        self.post_progress(true);
        Ok(())
    }

    fn unpack_one(&mut self, archive_path: &Path, target: &Path, buf: &mut [u8]) -> Result<(), Cancelled> {
        if let Err(e) = std::fs::create_dir_all(target) {
            self.error(target, error_text(&e));
            return Ok(());
        }
        let mut reader = match Reader::open(archive_path) {
            Ok(r) => r,
            Err(e) => {
                self.error(archive_path, e);
                return Ok(());
            }
        };
        while let Some(header) = reader.next() {
            self.check()?;
            let h = match header {
                Ok(h) => h,
                Err(e) => {
                    self.error(archive_path, e);
                    break;
                }
            };
            let Some(path) = clean_path(&h.path) else { continue };
            let out = target.join(path);
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
                EntryType::File => self.extract_file(&mut reader, &out, h.size, h.mtime, buf)?,
            }
        }
        Ok(())
    }
}
