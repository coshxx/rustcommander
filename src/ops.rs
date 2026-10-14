//! File operations on a worker thread: copy, move, delete (permanent or to the recycle bin),
//! rename, create directory. Reports progress, asks the UI about conflicts, can be cancelled.

use std::ffi::{OsString, c_void};
use std::os::windows::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{ERROR_NOT_SAME_DEVICE, HANDLE};
use windows::Win32::Storage::FileSystem::{
    COPYFILE_FLAGS, COPYPROGRESSROUTINE_PROGRESS, CopyFileExW, LPPROGRESS_ROUTINE_CALLBACK_REASON,
    MOVE_FILE_FLAGS, MOVEFILE_REPLACE_EXISTING, MoveFileExW, PROGRESS_CANCEL, PROGRESS_CONTINUE,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::UI::Shell::{
    FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_WANTNUKEWARNING,
    SHFILEOPSTRUCTW, SHFileOperationW,
};
use windows::core::PCWSTR;

use crate::fsx::to_wide_z;
use crate::win::SendHwnd;

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const MAX_DEPTH: usize = 128;

pub enum Job {
    Copy { sources: Vec<PathBuf>, dest: PathBuf },
    Move { sources: Vec<PathBuf>, dest: PathBuf },
    Delete { sources: Vec<PathBuf>, recycle: bool },
    Rename { from: PathBuf, to: OsString },
    MkDir { path: PathBuf },
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
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.answers.send(Answer::Cancel);
    }

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

fn is_reparse(m: &std::fs::Metadata) -> bool {
    m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

fn same_volume(a: &Path, b: &Path) -> bool {
    let root = |p: &Path| match p.components().next() {
        Some(Component::Prefix(px)) => Some(px.as_os_str().to_ascii_lowercase()),
        _ => None,
    };
    root(a).is_some() && root(a) == root(b)
}

fn err_text(e: &std::io::Error) -> String {
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
    fn send(&self, ev: Event) {
        if self.events.send(ev).is_ok() {
            if let Some((hwnd, msg)) = self.notify {
                hwnd.post(msg);
            }
        }
    }

    fn post_progress(&mut self, force: bool) {
        if force || self.last_post.elapsed() >= Duration::from_millis(40) {
            self.last_post = Instant::now();
            self.send(Event::Progress(self.progress.clone()));
        }
    }

    fn check(&self) -> Result<(), Cancelled> {
        if self.cancel.load(Ordering::Relaxed) { Err(Cancelled) } else { Ok(()) }
    }

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
                        Err(e) => self.error(&from, err_text(&e)),
                    }
                }
                Ok(())
            }
            Job::MkDir { path } => {
                match std::fs::create_dir_all(&path) {
                    Ok(()) => {
                        self.summary.done = 1;
                        self.summary.select = Some(path);
                    }
                    Err(e) => self.error(&path, err_text(&e)),
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
        if into || dest.is_dir() {
            src.file_name().map(|n| dest.join(n))
        } else {
            Some(dest.to_path_buf())
        }
    }

    fn transfer(&mut self, sources: &[PathBuf], dest: &Path, is_move: bool) -> Result<(), Cancelled> {
        // Several sources, or a destination ending in `\`, always mean "into this directory".
        let ends_with_sep = dest.as_os_str().to_string_lossy().ends_with(['\\', '/']);
        let many = sources.len() > 1 || ends_with_sep;
        if many && !dest.exists() {
            if let Err(e) = std::fs::create_dir_all(dest) {
                self.error(dest, err_text(&e));
                return Ok(());
            }
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

    fn copy_tree(&mut self, src: &Path, dst: &Path, depth: usize) -> Result<(), Cancelled> {
        self.check()?;
        let m = match std::fs::metadata(src) {
            Ok(m) => m,
            Err(e) => {
                self.error(src, err_text(&e));
                return Ok(());
            }
        };
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
                self.copy_tree(&e.path(), &dst.join(e.file_name()), depth + 1)?;
            }
            Ok(())
        } else {
            self.copy_file(src, dst, m.len()).map(|_| ())
        }
    }

    /// `Ok(true)` only if the file was actually copied.
    fn copy_file(&mut self, src: &Path, dst: &Path, len: u64) -> Result<bool, Cancelled> {
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
        let (s, d) = (to_wide_z(src.as_os_str()), to_wide_z(dst.as_os_str()));
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

    fn move_item(&mut self, src: &Path, dst: &Path, fast: bool) -> Result<(), Cancelled> {
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

    fn delete_all(&mut self, sources: &[PathBuf]) -> Result<(), Cancelled> {
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
                self.error(path, err_text(&e));
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
                    self.error(path, err_text(&e));
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
            self.error(path, err_text(&e));
        }
        if !m.is_dir() {
            self.progress.files_done += 1;
        }
        self.post_progress(false);
        Ok(())
    }

    fn recycle(&mut self, sources: &[PathBuf]) {
        self.progress.files_total = sources.len() as u64;
        self.post_progress(true);
        // Double-null-terminated list of paths.
        let mut from: Vec<u16> = Vec::new();
        for s in sources {
            from.extend(to_wide_z(s.as_os_str()));
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

fn count_no_follow(path: &Path, depth: usize, files: &mut u64) {
    let Ok(m) = std::fs::symlink_metadata(path) else { return };
    if m.is_dir() && !is_reparse(&m) {
        if depth < MAX_DEPTH {
            if let Ok(rd) = std::fs::read_dir(path) {
                for e in rd.flatten() {
                    count_no_follow(&e.path(), depth + 1, files);
                }
            }
        }
    } else if !m.is_dir() {
        *files += 1;
    }
}

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

fn same_name_ignoring_case(a: &Path, b: &Path) -> bool {
    a.as_os_str().to_ascii_lowercase() == b.as_os_str().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Fresh scratch directory under the system temp dir; never touches anything else.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let p = std::env::temp_dir().join(format!("rc-ops-test-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Scratch(p)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write(p: &Path, content: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    /// Runs a job to completion, answering conflicts with `answer`.
    fn run(job: Job, answer: Answer) -> (Summary, u32) {
        let h = start(job, None);
        let mut conflicts = 0;
        loop {
            match h.events.recv().unwrap() {
                Event::Progress(_) => {}
                Event::Conflict(_) => {
                    conflicts += 1;
                    h.answer(answer);
                }
                Event::Done(s) => return (s, conflicts),
            }
        }
    }

    #[test]
    fn copy_tree_into_dir() {
        let t = Scratch::new("copy");
        write(&t.0.join("src/a.txt"), "A");
        write(&t.0.join("src/sub/b.txt"), "BB");
        fs::create_dir_all(t.0.join("dst")).unwrap();
        let (s, _) = run(Job::Copy { sources: vec![t.0.join("src")], dest: t.0.join("dst") }, Answer::Skip);
        assert!(s.errors.is_empty(), "{:?}", s.errors);
        assert_eq!(fs::read_to_string(t.0.join("dst/src/sub/b.txt")).unwrap(), "BB");
        assert!(t.0.join("src/a.txt").exists());
    }

    #[test]
    fn trailing_separator_means_directory() {
        let t = Scratch::new("trailing");
        write(&t.0.join("a.txt"), "A");
        let mut dest = t.0.join("neu").into_os_string();
        dest.push("\\");
        let (s, _) = run(Job::Copy { sources: vec![t.0.join("a.txt")], dest: dest.into() }, Answer::Skip);
        assert!(s.errors.is_empty(), "{:?}", s.errors);
        assert!(t.0.join("neu/a.txt").is_file());
    }

    #[test]
    fn copy_single_file_with_new_name() {
        let t = Scratch::new("copyname");
        write(&t.0.join("a.txt"), "A");
        let (s, _) = run(Job::Copy { sources: vec![t.0.join("a.txt")], dest: t.0.join("b.txt") }, Answer::Skip);
        assert!(s.errors.is_empty());
        assert_eq!(fs::read_to_string(t.0.join("b.txt")).unwrap(), "A");
    }

    #[test]
    fn conflicts_skip_and_overwrite() {
        let t = Scratch::new("conflict");
        write(&t.0.join("src/a.txt"), "new");
        write(&t.0.join("dst/a.txt"), "old");
        let job = || Job::Copy { sources: vec![t.0.join("src/a.txt")], dest: t.0.join("dst") };
        let (_, n) = run(job(), Answer::Skip);
        assert_eq!(n, 1);
        assert_eq!(fs::read_to_string(t.0.join("dst/a.txt")).unwrap(), "old");
        let (_, _) = run(job(), Answer::Overwrite);
        assert_eq!(fs::read_to_string(t.0.join("dst/a.txt")).unwrap(), "new");
    }

    #[test]
    fn cancel_on_conflict_stops() {
        let t = Scratch::new("cancel");
        write(&t.0.join("src/a.txt"), "1");
        write(&t.0.join("src/b.txt"), "2");
        write(&t.0.join("dst/a.txt"), "x");
        write(&t.0.join("dst/b.txt"), "y");
        let (s, n) = run(
            Job::Copy { sources: vec![t.0.join("src/a.txt"), t.0.join("src/b.txt")], dest: t.0.join("dst") },
            Answer::Cancel,
        );
        assert!(s.cancelled);
        assert_eq!(n, 1);
        assert_eq!(fs::read_to_string(t.0.join("dst/b.txt")).unwrap(), "y");
    }

    #[test]
    fn cancel_while_copying_many_files() {
        let t = Scratch::new("cancelmany");
        for i in 0..3000 {
            write(&t.0.join(format!("src/f{i}.txt")), "x");
        }
        let h = start(Job::Copy { sources: vec![t.0.join("src")], dest: t.0.join("dst") }, None);
        let summary = loop {
            match h.events.recv().unwrap() {
                Event::Progress(p) if p.files_done > 0 => h.cancel(),
                Event::Done(s) => break s,
                _ => {}
            }
        };
        assert!(summary.cancelled);
        let copied = fs::read_dir(t.0.join("dst/src")).map(|rd| rd.count()).unwrap_or(0);
        assert!(copied < 3000, "copied {copied} files despite cancel");
    }

    #[test]
    fn copy_into_itself_is_refused() {
        let t = Scratch::new("self");
        write(&t.0.join("src/a.txt"), "A");
        let (s, _) = run(Job::Copy { sources: vec![t.0.join("src")], dest: t.0.join("src") }, Answer::Skip);
        assert_eq!(s.errors.len(), 1);
        assert!(!t.0.join("src/src").exists());
    }

    #[test]
    fn move_same_volume_and_merge() {
        let t = Scratch::new("move");
        write(&t.0.join("src/d/a.txt"), "A");
        write(&t.0.join("dst/d/b.txt"), "B");
        let (s, _) = run(Job::Move { sources: vec![t.0.join("src/d")], dest: t.0.join("dst") }, Answer::Skip);
        assert!(s.errors.is_empty(), "{:?}", s.errors);
        assert!(t.0.join("dst/d/a.txt").exists());
        assert!(t.0.join("dst/d/b.txt").exists());
        assert!(!t.0.join("src/d").exists());
    }

    #[test]
    fn move_skip_keeps_source() {
        let t = Scratch::new("moveskip");
        write(&t.0.join("src/a.txt"), "new");
        write(&t.0.join("dst/a.txt"), "old");
        let (_, n) = run(Job::Move { sources: vec![t.0.join("src/a.txt")], dest: t.0.join("dst") }, Answer::Skip);
        assert_eq!(n, 1);
        assert!(t.0.join("src/a.txt").exists());
        assert_eq!(fs::read_to_string(t.0.join("dst/a.txt")).unwrap(), "old");
    }

    #[test]
    fn delete_permanent_tree_with_readonly() {
        let t = Scratch::new("delete");
        write(&t.0.join("d/sub/a.txt"), "A");
        let ro = t.0.join("d/ro.txt");
        write(&ro, "R");
        let mut p = fs::metadata(&ro).unwrap().permissions();
        p.set_readonly(true);
        fs::set_permissions(&ro, p).unwrap();
        let (s, _) = run(Job::Delete { sources: vec![t.0.join("d")], recycle: false }, Answer::Skip);
        assert!(s.errors.is_empty(), "{:?}", s.errors);
        assert!(!t.0.join("d").exists());
    }

    #[test]
    fn delete_does_not_follow_junctions() {
        let t = Scratch::new("junction");
        write(&t.0.join("keep/important.txt"), "!");
        fs::create_dir_all(t.0.join("d")).unwrap();
        let link = t.0.join("d/link");
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(t.0.join("keep"))
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok {
            return; // mklink unavailable; nothing to test
        }
        let (s, _) = run(Job::Delete { sources: vec![t.0.join("d")], recycle: false }, Answer::Skip);
        assert!(s.errors.is_empty(), "{:?}", s.errors);
        assert!(!t.0.join("d").exists());
        assert!(t.0.join("keep/important.txt").exists());
    }

    #[test]
    fn rename_and_mkdir() {
        let t = Scratch::new("rename");
        write(&t.0.join("a.txt"), "A");
        let (s, _) = run(Job::Rename { from: t.0.join("a.txt"), to: "b.txt".into() }, Answer::Skip);
        assert_eq!(s.select.as_deref(), Some(t.0.join("b.txt").as_path()));
        let (s, _) = run(Job::MkDir { path: t.0.join("neu") }, Answer::Skip);
        assert!(s.errors.is_empty());
        assert!(t.0.join("neu").is_dir());
        let (s, _) = run(Job::Rename { from: t.0.join("b.txt"), to: "neu".into() }, Answer::Skip);
        assert_eq!(s.errors.len(), 1);
    }
}
