//! Folder sizes (Space, Alt+Shift+Enter). All requests share one pool of worker threads and
//! one stack of directories still to read, so a wide tree keeps every thread busy and several
//! folders can be counted at once. Directory links (junctions, symlinks) are not followed:
//! they would count data twice or loop.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_FILES};
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FIND_FIRST_EX_LARGE_FETCH, FindClose, FindExInfoBasic,
    FindExSearchNameMatch, FindFirstFileExW, FindNextFileW, WIN32_FIND_DATAW,
};
use windows::core::PCWSTR;

use crate::files::archive;
use crate::files::listing::extended_path;
use crate::sys::win::SendHwnd;

// Reparse tags of directory links (`winnt.h`). Other reparse points – OneDrive placeholders,
// deduplicated files – are ordinary folders as far as the size goes.
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;

/// Size of one folder, counted in the background; the numbers grow while it runs.
pub struct DirSize {
    bytes: AtomicU64,
    /// Directories queued or being read; 0 = done.
    pending: AtomicUsize,
    /// Something could not be read (access denied …): the real size is larger.
    incomplete: AtomicBool,
    cancel: AtomicBool,
    /// Posted once when counting has finished.
    notify: (SendHwnd, u32),
}

impl DirSize {
    fn new(notify: (SendHwnd, u32)) -> Arc<DirSize> {
        Arc::new(DirSize {
            bytes: AtomicU64::new(0),
            pending: AtomicUsize::new(1),
            incomplete: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
            notify,
        })
    }

    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    pub fn done(&self) -> bool {
        self.pending.load(Ordering::Acquire) == 0
    }

    pub fn incomplete(&self) -> bool {
        self.incomplete.load(Ordering::Relaxed)
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// One directory less to read; the last one reports the end.
    fn finish_one(&self) {
        if self.pending.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.notify.0.post(self.notify.1);
        }
    }
}

/// A directory to read: its path in `\\?\` form (no MAX_PATH limit), without terminator.
struct Task {
    size: Arc<DirSize>,
    path: Vec<u16>,
}

struct Pool {
    stack: Mutex<Vec<Task>>,
    ready: Condvar,
}

impl Pool {
    fn lock(&self) -> MutexGuard<'_, Vec<Task>> {
        self.stack.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn push(&self, tasks: impl IntoIterator<Item = Task>) {
        let mut stack = self.lock();
        let before = stack.len();
        stack.extend(tasks);
        let added = stack.len() - before;
        drop(stack);
        if added == 1 {
            self.ready.notify_one();
        } else if added > 1 {
            self.ready.notify_all();
        }
    }
}

/// The worker threads are started on first use and then wait for work.
fn pool() -> &'static Pool {
    static POOL: OnceLock<&'static Pool> = OnceLock::new();
    POOL.get_or_init(|| {
        let pool: &'static Pool = Box::leak(Box::new(Pool { stack: Mutex::new(Vec::new()), ready: Condvar::new() }));
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 16);
        for _ in 0..threads {
            std::thread::spawn(move || worker(pool));
        }
        pool
    })
}

fn worker(pool: &'static Pool) {
    loop {
        let task = {
            let mut stack = pool.lock();
            loop {
                if let Some(t) = stack.pop() {
                    break t;
                }
                stack = pool.ready.wait(stack).unwrap_or_else(|e| e.into_inner());
            }
        };
        if !task.size.cancelled() {
            let subdirs = read_dir(&task.size, &task.path);
            if !subdirs.is_empty() {
                // Counted before this directory is finished, so `pending` can't reach 0 early.
                task.size.pending.fetch_add(subdirs.len(), Ordering::AcqRel);
                pool.push(subdirs.into_iter().map(|path| Task { size: task.size.clone(), path }));
            }
        }
        task.size.finish_one();
    }
}

/// Add up the files of one directory; returns its subdirectories.
fn read_dir(size: &DirSize, dir: &[u16]) -> Vec<Vec<u16>> {
    let mut subdirs = Vec::new();
    let mut bytes = 0u64;
    let mut n = 0u32;
    let complete = scan(dir, |name, data| {
        if is_dir(data) {
            if !is_dir_link(data) {
                subdirs.push(child(dir, name));
            }
        } else {
            bytes += file_size(data);
        }
        n += 1;
        // Huge directories: show progress and react to cancelling now and then.
        if n.is_multiple_of(4096) {
            size.bytes.fetch_add(std::mem::take(&mut bytes), Ordering::Relaxed);
            return !size.cancelled();
        }
        true
    });
    if !complete {
        size.incomplete.store(true, Ordering::Relaxed);
    }
    size.bytes.fetch_add(bytes, Ordering::Relaxed);
    subdirs
}

/// Read the directory `dir` (a `\\?\` path without terminator) entry by entry, without `.` and
/// `..`, the fastest way Windows offers. `f` gets name and find data and returns `false` to stop.
/// Returns `false` if the directory could not be read (completely); an empty one is fine.
pub(crate) fn scan(dir: &[u16], mut f: impl FnMut(&[u16], &WIN32_FIND_DATAW) -> bool) -> bool {
    let mut pattern = dir.to_vec();
    if pattern.last() != Some(&(b'\\' as u16)) {
        pattern.push(b'\\' as u16);
    }
    pattern.extend_from_slice(&[b'*' as u16, 0]);
    let mut data = WIN32_FIND_DATAW::default();
    let handle = unsafe {
        FindFirstFileExW(
            PCWSTR(pattern.as_ptr()),
            FindExInfoBasic,
            &mut data as *mut _ as *mut _,
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };
    let handle = match handle {
        Ok(h) => h,
        Err(e) => return e.code() == ERROR_FILE_NOT_FOUND.to_hresult(),
    };
    let mut complete = true;
    loop {
        let len = data.cFileName.iter().position(|&c| c == 0).unwrap_or(data.cFileName.len());
        let name = &data.cFileName[..len];
        let dots = name == [b'.' as u16] || name == [b'.' as u16, b'.' as u16];
        if !dots && !f(name, &data) {
            break;
        }
        if let Err(e) = unsafe { FindNextFileW(handle, &mut data) } {
            complete = e.code() == ERROR_NO_MORE_FILES.to_hresult();
            break;
        }
    }
    unsafe {
        let _ = FindClose(handle);
    }
    complete
}

pub(crate) fn is_dir(data: &WIN32_FIND_DATAW) -> bool {
    data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0
}

/// Junction or directory symlink: not followed (would count twice or loop).
pub(crate) fn is_dir_link(data: &WIN32_FIND_DATAW) -> bool {
    data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
        && matches!(data.dwReserved0, IO_REPARSE_TAG_MOUNT_POINT | IO_REPARSE_TAG_SYMLINK)
}

pub(crate) fn file_size(data: &WIN32_FIND_DATAW) -> u64 {
    ((data.nFileSizeHigh as u64) << 32) | data.nFileSizeLow as u64
}

/// `dir\name`.
pub(crate) fn child(dir: &[u16], name: &[u16]) -> Vec<u16> {
    let mut path = Vec::with_capacity(dir.len() + 1 + name.len());
    path.extend_from_slice(dir);
    if path.last() != Some(&(b'\\' as u16)) {
        path.push(b'\\' as u16);
    }
    path.extend_from_slice(name);
    path
}

/// Start counting the folder `dir`; `notify` is posted when done.
pub fn start(dir: &Path, notify: (SendHwnd, u32)) -> Arc<DirSize> {
    let size = DirSize::new(notify);
    pool().push([Task { size: size.clone(), path: extended_path(dir) }]);
    size
}

/// The size of the folder `inner` (`a\b`) inside `archive`, from its table of contents. Runs
/// on a thread of its own, as the table may have to be read first.
pub fn start_in_archive(archive: PathBuf, inner: String, notify: (SendHwnd, u32)) -> Arc<DirSize> {
    let size = DirSize::new(notify);
    let s = size.clone();
    std::thread::spawn(move || {
        match archive::index(&archive, &s.cancel) {
            Ok(index) => {
                let (_, bytes) = index.totals(&[inner]);
                s.bytes.store(bytes, Ordering::Relaxed);
            }
            Err(_) => s.incomplete.store(true, Ordering::Relaxed),
        }
        s.finish_one();
    });
    size
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait(size: &DirSize) {
        let start = Instant::now();
        while !size.done() {
            assert!(start.elapsed() < Duration::from_secs(10), "counting did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn counts_nested_folders() {
        let root = std::env::temp_dir().join(format!("rc_dirsize_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for i in 0..20 {
            let d = root.join(format!("d{i}")).join("x").join("y");
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("f.bin"), vec![0u8; 1000 + i]).unwrap();
        }
        std::fs::write(root.join("top.txt"), b"hello").unwrap();
        let size = start(&root, (SendHwnd::new(Default::default()), 0));
        wait(&size);
        let expected: u64 = (0..20).map(|i| 1000 + i).sum::<u64>() + 5;
        assert_eq!(size.bytes(), expected);
        assert!(!size.incomplete());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn missing_folder_is_incomplete() {
        let size = start(Path::new(r"C:\does\not\exist\rc_dirsize"), (SendHwnd::new(Default::default()), 0));
        wait(&size);
        assert_eq!(size.bytes(), 0);
    }
}
