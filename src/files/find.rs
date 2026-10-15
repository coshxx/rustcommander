//! File search (Alt+F7): walks a directory tree on several threads that share one stack of
//! directories, matches names against a mask and, optionally, file contents against a text.
//! Hits are sent in batches; `notify` is posted at most once until the UI has taken them.
//! Directory links are not followed (like folder sizes), archives are not looked into.

use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW;

use crate::files::dirsize::{child, file_size, is_dir, is_dir_link, scan};
use crate::files::listing::extended_path;
use crate::files::mask::Mask;
use crate::lister::search::{Outcome, Pattern};
use crate::lister::text::Encoding;
use crate::sys::mapped::MappedFile;
use crate::sys::win::SendHwnd;

pub struct Hit {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    /// FILETIME (100 ns ticks since 1601, UTC).
    pub mtime: u64,
}

pub struct Query {
    pub root: PathBuf,
    /// File mask syntax (`*.rs;*.toml | target\`); empty = everything.
    pub mask: String,
    /// Text the file must contain (case-insensitive); empty = no content check.
    pub text: String,
}

/// A search running in the background; dropping it cancels it.
pub struct Search {
    shared: Arc<Shared>,
    rx: Receiver<Vec<Hit>>,
}

struct Shared {
    mask: Mask,
    /// The text in UTF-8, the ANSI code page and UTF-16 LE; empty = names only.
    patterns: Vec<Pattern>,
    stack: Mutex<Vec<Vec<u16>>>,
    ready: Condvar,
    /// Directories queued or being read; 0 = finished.
    pending: AtomicUsize,
    dirs: AtomicU64,
    cancel: AtomicBool,
    /// `notify` was posted and not yet answered by `take_hits`.
    posted: AtomicBool,
    notify: (SendHwnd, u32),
}

impl Search {
    /// New hits since the last call.
    pub fn take_hits(&self) -> Vec<Hit> {
        self.shared.posted.store(false, Ordering::Release);
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(mut batch) => out.append(&mut batch),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return out,
            }
        }
    }

    pub fn done(&self) -> bool {
        self.shared.pending.load(Ordering::Acquire) == 0
    }

    /// Directories read so far.
    pub fn dirs(&self) -> u64 {
        self.shared.dirs.load(Ordering::Relaxed)
    }

    pub fn cancel(&self) {
        self.shared.cancel.store(true, Ordering::Relaxed);
        self.shared.ready.notify_all();
    }
}

impl Drop for Search {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Vec<Vec<u16>>> {
        self.stack.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn post(&self) {
        if !self.posted.swap(true, Ordering::AcqRel) {
            self.notify.0.post(self.notify.1);
        }
    }
}

/// Start searching; `notify` is posted when there are new hits and when the search ends.
pub fn start(query: Query, notify: (SendHwnd, u32)) -> Search {
    let pages = crate::lister::code_pages();
    let patterns = if query.text.is_empty() {
        Vec::new()
    } else {
        [Encoding::Utf8, Encoding::Ansi, Encoding::Utf16Le]
            .into_iter()
            .filter_map(|enc| Pattern::new(&query.text, enc, pages, true, 0))
            .collect()
    };
    let shared = Arc::new(Shared {
        mask: Mask::parse(&query.mask),
        patterns,
        stack: Mutex::new(vec![extended_path(&query.root)]),
        ready: Condvar::new(),
        pending: AtomicUsize::new(1),
        dirs: AtomicU64::new(0),
        cancel: AtomicBool::new(false),
        posted: AtomicBool::new(false),
        notify,
    });
    let (tx, rx) = channel();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 16);
    for _ in 0..threads {
        let (shared, tx) = (shared.clone(), tx.clone());
        std::thread::spawn(move || worker(&shared, &tx));
    }
    Search { shared, rx }
}

fn worker(shared: &Shared, tx: &Sender<Vec<Hit>>) {
    loop {
        let dir = {
            let mut stack = shared.lock();
            loop {
                if shared.cancelled() || shared.pending.load(Ordering::Acquire) == 0 {
                    return;
                }
                if let Some(d) = stack.pop() {
                    break d;
                }
                // With a timeout: the last directory may end without anyone notifying.
                stack = shared
                    .ready
                    .wait_timeout(stack, Duration::from_millis(50))
                    .map_or_else(|e| e.into_inner().0, |r| r.0);
            }
        };
        let (hits, subdirs) = search_dir(shared, &dir);
        shared.dirs.fetch_add(1, Ordering::Relaxed);
        if !subdirs.is_empty() {
            // Counted before this directory is finished, so `pending` can't reach 0 early.
            shared.pending.fetch_add(subdirs.len(), Ordering::AcqRel);
            shared.lock().extend(subdirs);
            shared.ready.notify_all();
        }
        let mut news = !hits.is_empty();
        if news {
            let _ = tx.send(hits);
        }
        if shared.pending.fetch_sub(1, Ordering::AcqRel) == 1 {
            shared.ready.notify_all();
            news = true;
        }
        if news {
            shared.post();
        }
    }
}

/// Hits in `dir` and the subdirectories to search next.
fn search_dir(shared: &Shared, dir: &[u16]) -> (Vec<Hit>, Vec<Vec<u16>>) {
    let mut hits = Vec::new();
    let mut subdirs = Vec::new();
    scan(dir, |name, data| {
        let folder = is_dir(data);
        if folder && !is_dir_link(data) && !shared.mask.excludes_dir(name) {
            subdirs.push(child(dir, name));
        }
        if shared.mask.finds(name, folder) && (shared.patterns.is_empty() || (!folder && contains(shared, dir, name))) {
            hits.push(hit(dir, name, data));
        }
        !shared.cancelled()
    });
    (hits, subdirs)
}

fn hit(dir: &[u16], name: &[u16], data: &WIN32_FIND_DATAW) -> Hit {
    let full = child(dir, name);
    // Shown and handed on without the `\\?\` prefix.
    let plain = full.strip_prefix(&[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16][..]).unwrap_or(&full);
    let t = data.ftLastWriteTime;
    Hit {
        path: PathBuf::from(std::ffi::OsString::from_wide(plain)),
        is_dir: is_dir(data),
        size: file_size(data),
        mtime: ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64,
    }
}

/// Does the file contain the text (in any of the encodings)? Unreadable files don't.
fn contains(shared: &Shared, dir: &[u16], name: &[u16]) -> bool {
    let path = PathBuf::from(std::ffi::OsString::from_wide(&child(dir, name)));
    let Ok(file) = MappedFile::open(&path) else { return false };
    let data = file.bytes();
    let mut go_on = |_| !shared.cancelled();
    shared.patterns.iter().any(|p| matches!(p.find(data, 0, true, (0, data.len()), &mut go_on), Outcome::Found(..)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn run(query: Query) -> Vec<String> {
        let search = start(query, (SendHwnd::new(Default::default()), 0));
        let begin = Instant::now();
        let mut hits = Vec::new();
        while !search.done() {
            assert!(begin.elapsed() < Duration::from_secs(10), "search did not finish");
            hits.append(&mut search.take_hits());
            std::thread::sleep(Duration::from_millis(5));
        }
        hits.append(&mut search.take_hits());
        let mut names: Vec<String> =
            hits.iter().map(|h| h.path.file_name().unwrap().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn names_and_contents() {
        let root = std::env::temp_dir().join(format!("rc_find_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["a", "a/b", "target/x", "Fotos"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("a/b/notiz.txt"), "Grüße aus München").unwrap();
        let utf16: Vec<u8> = "MÜNCHEN".encode_utf16().flat_map(u16::to_le_bytes).collect();
        std::fs::write(root.join("a/wide.txt"), utf16).unwrap();
        std::fs::write(root.join("a/other.txt"), "nichts").unwrap();
        std::fs::write(root.join("target/x/notiz.txt"), "München").unwrap();
        std::fs::write(root.join("Fotos/urlaub.jpg"), "").unwrap();
        let q = |mask: &str, text: &str| Query { root: root.clone(), mask: mask.into(), text: text.into() };

        assert_eq!(run(q("*.txt | target\\", "")), ["notiz.txt", "other.txt", "wide.txt"]);
        assert_eq!(run(q("foto", "")), ["Fotos"]);
        // Content: case-insensitive with umlauts, UTF-8 and UTF-16; folders never match.
        assert_eq!(run(q("", "münchen")), ["notiz.txt", "notiz.txt", "wide.txt"]);
        assert_eq!(run(q("| target\\", "münchen")), ["notiz.txt", "wide.txt"]);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
