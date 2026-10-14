//! File operations on a worker thread: copy, move, delete (permanent or to the recycle bin),
//! rename, create directory, extract from an archive. Reports progress, asks the UI about
//! conflicts, can be cancelled.

mod copy;
mod delete;
mod extract;
mod moving;
#[cfg(test)]
mod tests;

use std::ffi::OsString;
use std::os::windows::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use crate::sys::win::SendHwnd;

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const MAX_DEPTH: usize = 128;

pub enum Job {
    Copy {
        sources: Vec<PathBuf>,
        dest: PathBuf,
    },
    Move {
        sources: Vec<PathBuf>,
        dest: PathBuf,
    },
    Delete {
        sources: Vec<PathBuf>,
        recycle: bool,
    },
    Rename {
        from: PathBuf,
        to: OsString,
    },
    MkDir {
        path: PathBuf,
    },
    /// Copy out of an archive; `sources` are paths inside it (`C:\x\a.zip\dir\file`).
    Extract {
        sources: Vec<PathBuf>,
        dest: PathBuf,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Answer {
    Overwrite,
    Skip,
    OverwriteAll,
    SkipAll,
    Cancel,
}

#[derive(Clone, Default, Debug)]
pub struct Progress {
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub current: PathBuf,
}

#[derive(Default, Debug)]
pub struct Summary {
    /// Top-level items or files handled successfully.
    pub done: u64,
    pub errors: Vec<String>,
    pub cancelled: bool,
    /// Path the UI should put the cursor on (new name after rename/mkdir).
    pub select: Option<PathBuf>,
}

pub enum Event {
    Progress(Progress),
    /// Target exists; the worker waits for an `Answer`.
    Conflict(PathBuf),
    Done(Summary),
}

/// UI side of a running job.
pub struct Handle {
    cancel: Arc<AtomicBool>,
    pub events: Receiver<Event>,
    answers: Sender<Answer>,
}

impl Handle {
    /// Stop the job as soon as possible (also mid-file).
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.answers.send(Answer::Cancel);
    }

    /// Answer the pending `Event::Conflict`.
    pub fn answer(&self, a: Answer) {
        if a == Answer::Cancel {
            self.cancel();
        } else {
            let _ = self.answers.send(a);
        }
    }
}

/// Starts `job` on a worker thread. Every event is followed by posting `msg` to `notify`.
pub fn start(job: Job, notify: Option<(SendHwnd, u32)>) -> Handle {
    let cancel = Arc::new(AtomicBool::new(false));
    let (ev_tx, ev_rx) = channel();
    let (ans_tx, ans_rx) = channel();
    let mut w = Worker {
        cancel: cancel.clone(),
        events: ev_tx,
        answers: ans_rx,
        notify,
        progress: Progress::default(),
        last_post: Instant::now() - Duration::from_secs(1),
        policy: None,
        summary: Summary::default(),
    };
    std::thread::spawn(move || {
        w.run(job);
        let summary = std::mem::take(&mut w.summary);
        w.send(Event::Done(summary));
    });
    Handle { cancel, events: ev_rx, answers: ans_tx }
}

struct Cancelled;

struct Worker {
    cancel: Arc<AtomicBool>,
    events: Sender<Event>,
    answers: Receiver<Answer>,
    notify: Option<(SendHwnd, u32)>,
    progress: Progress,
    last_post: Instant,
    /// Set by "overwrite all" (`true`) or "skip all" (`false`).
    policy: Option<bool>,
    summary: Summary,
}

/// Junction or symbolic link: never followed when deleting or moving across volumes.
fn is_reparse(m: &std::fs::Metadata) -> bool {
    m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// Both paths on the same drive letter / volume (a move is then a cheap rename).
fn same_volume(a: &Path, b: &Path) -> bool {
    let root = |p: &Path| match p.components().next() {
        Some(Component::Prefix(px)) => Some(px.as_os_str().to_ascii_lowercase()),
        _ => None,
    };
    root(a).is_some() && root(a) == root(b)
}

/// OS error message without the trailing "(os error N)".
pub(crate) fn error_text(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(code) => windows::core::Error::from_hresult(windows::core::HRESULT::from_win32(code as u32))
            .message()
            .trim()
            .to_string(),
        None => e.to_string(),
    }
}

/// Count files and bytes below `path` (following directory links, like the copy does).
fn scan(path: &Path, depth: usize, files: &mut u64, bytes: &mut u64) {
    let Ok(m) = std::fs::metadata(path) else { return };
    if m.is_dir() {
        if depth >= MAX_DEPTH {
            return;
        }
        if let Ok(rd) = std::fs::read_dir(path) {
            for e in rd.flatten() {
                scan(&e.path(), depth + 1, files, bytes);
            }
        }
    } else {
        *files += 1;
        *bytes += m.len();
    }
}

/// Remove the read-only attribute so the file can be overwritten or deleted.
fn clear_readonly(path: &Path) {
    if let Ok(m) = std::fs::symlink_metadata(path) {
        let mut p = m.permissions();
        if p.readonly() {
            #[allow(clippy::permissions_set_readonly_false)]
            p.set_readonly(false);
            let _ = std::fs::set_permissions(path, p);
        }
    }
}

/// Rename that only changes the case (`a.txt` → `A.txt`).
fn same_name_ignoring_case(a: &Path, b: &Path) -> bool {
    a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
}

impl Worker {
    /// Send an event to the UI and wake it up.
    fn send(&self, ev: Event) {
        if self.events.send(ev).is_ok()
            && let Some((hwnd, msg)) = self.notify
        {
            hwnd.post(msg);
        }
    }

    /// Report progress, at most every few milliseconds unless `force`.
    fn post_progress(&mut self, force: bool) {
        if force || self.last_post.elapsed() >= Duration::from_millis(40) {
            self.last_post = Instant::now();
            self.send(Event::Progress(self.progress.clone()));
        }
    }

    /// `Err(Cancelled)` once the UI asked to cancel.
    fn check(&self) -> Result<(), Cancelled> {
        if self.cancel.load(Ordering::Relaxed) { Err(Cancelled) } else { Ok(()) }
    }

    /// Remember a failure for the summary and carry on with the next item.
    fn error(&mut self, path: &Path, msg: impl AsRef<str>) {
        self.summary.errors.push(format!("{}: {}", path.display(), msg.as_ref()));
    }

    /// `Ok(true)` = overwrite, `Ok(false)` = skip.
    fn resolve_conflict(&mut self, target: &Path) -> Result<bool, Cancelled> {
        if let Some(p) = self.policy {
            return Ok(p);
        }
        self.send(Event::Conflict(target.to_path_buf()));
        match self.answers.recv().unwrap_or(Answer::Cancel) {
            Answer::Overwrite => Ok(true),
            Answer::Skip => Ok(false),
            Answer::OverwriteAll => {
                self.policy = Some(true);
                Ok(true)
            }
            Answer::SkipAll => {
                self.policy = Some(false);
                Ok(false)
            }
            Answer::Cancel => {
                self.cancel.store(true, Ordering::Relaxed);
                Err(Cancelled)
            }
        }
    }

    /// Execute the whole job and send `Event::Done` with the summary.
    fn run(&mut self, job: Job) {
        let result = match job {
            Job::Copy { sources, dest } => self.transfer(&sources, &dest, false),
            Job::Move { sources, dest } => self.transfer(&sources, &dest, true),
            Job::Delete { sources, recycle: true } => {
                self.recycle(&sources);
                Ok(())
            }
            Job::Delete { sources, recycle: false } => self.delete_all(&sources),
            Job::Rename { from, to } => {
                let target = from.with_file_name(&to);
                if target.exists() && !same_name_ignoring_case(&from, &target) {
                    self.error(&target, "existiert bereits");
                } else {
                    match std::fs::rename(&from, &target) {
                        Ok(()) => {
                            self.summary.done = 1;
                            self.summary.select = Some(target);
                        }
                        Err(e) => self.error(&from, error_text(&e)),
                    }
                }
                Ok(())
            }
            Job::Extract { sources, dest } => self.extract(&sources, &dest),
            Job::MkDir { path } => {
                match std::fs::create_dir_all(&path) {
                    Ok(()) => {
                        self.summary.done = 1;
                        self.summary.select = Some(path);
                    }
                    Err(e) => self.error(&path, error_text(&e)),
                }
                Ok(())
            }
        };
        if result.is_err() || self.cancel.load(Ordering::Relaxed) {
            self.summary.cancelled = true;
        }
    }

    // ---------- copy / move ----------

    /// Where `src` ends up: into `dest` if it is a directory (or several sources),
    /// otherwise exactly at `dest` (copy/move with a new name).
    fn target_for(src: &Path, dest: &Path, into: bool) -> Option<PathBuf> {
        if into || dest.is_dir() { src.file_name().map(|n| dest.join(n)) } else { Some(dest.to_path_buf()) }
    }

    /// Copy or move `sources` to `dest`. A trailing `\` or several sources mean "into `dest`".
    fn transfer(&mut self, sources: &[PathBuf], dest: &Path, is_move: bool) -> Result<(), Cancelled> {
        // Several sources, or a destination ending in `\`, always mean "into this directory".
        let ends_with_sep = dest.as_os_str().to_string_lossy().ends_with(['\\', '/']);
        let many = sources.len() > 1 || ends_with_sep;
        if many
            && !dest.exists()
            && let Err(e) = std::fs::create_dir_all(dest)
        {
            self.error(dest, error_text(&e));
            return Ok(());
        }
        let fast_move = is_move && sources.iter().all(|s| same_volume(s, dest));
        if fast_move {
            self.progress.files_total = sources.len() as u64;
        } else {
            for s in sources {
                scan(s, 0, &mut self.progress.files_total, &mut self.progress.bytes_total);
            }
        }
        self.post_progress(true);

        for src in sources {
            self.check()?;
            let Some(target) = Self::target_for(src, dest, many) else {
                self.error(src, "ungültiger Quellpfad");
                continue;
            };
            if src == &target {
                self.error(src, "Quelle und Ziel sind identisch");
                continue;
            }
            if target.starts_with(src) {
                self.error(src, "Ziel liegt innerhalb der Quelle");
                continue;
            }
            let errors_before = self.summary.errors.len();
            if is_move {
                self.move_item(src, &target, fast_move)?;
            } else {
                self.copy_tree(src, &target, 0)?;
            }
            if self.summary.errors.len() == errors_before {
                self.summary.done += 1;
            }
        }
        self.post_progress(true);
        Ok(())
    }
}
