//! State of one file list: directory, entries, cursor, scroll position, quick search.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::time::Instant;

use crate::files::archive;
use crate::files::dirsize::{self, DirSize};
use crate::files::listing::{self, Entry, Kind, Listing, Loaded, Pending};
use crate::files::names::{starts_with_ignore_case, to_utf16_nul};
use crate::files::watch::Watcher;
use crate::sys::win::SendHwnd;

pub const WM_APP_LOADED: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;
/// Posted when a folder size has been counted.
pub const WM_APP_SIZE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 5;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Everything a loader thread needs to report its result back.
#[derive(Clone)]
pub struct LoadSink {
    pub tx: Sender<Loaded>,
    pub hwnd: SendHwnd,
}

#[derive(Default)]
pub struct Panel {
    pub dir: PathBuf,
    pub entries: Vec<Entry>,
    pub cursor: usize,
    pub top: usize,
    /// Set during layout.
    pub rows_visible: usize,
    pub pending: Option<Pending>,
    pub search: Vec<u16>,
    pub error: Option<String>,
    pub dirs: usize,
    pub files: usize,
    pub bytes: u64,
    /// Watches `dir` (or the directory holding the archive) for changes.
    pub watcher: Option<Watcher>,
    /// The archive file if `dir` is inside one; such panels are read-only.
    pub archive: Option<PathBuf>,
    /// Folder sizes counted with Space / Alt+Shift+Enter, by folder name. Kept across automatic
    /// refreshes, dropped when leaving the directory or reloading by hand.
    pub sizes: HashMap<Box<[u16]>, Arc<DirSize>>,
}

/// Outcome of `Panel::apply`.
pub enum Applied {
    /// Result of a superseded load; ignored.
    Stale,
    Ok {
        dir_changed: bool,
    },
    Failed {
        path: PathBuf,
    },
}

impl Panel {
    /// Load `path` in the background; `select` = entry to put the cursor on.
    pub fn navigate(&mut self, index: usize, path: PathBuf, select: Option<Box<[u16]>>, sink: &LoadSink) {
        if path != self.dir {
            self.clear_sizes();
        }
        self.start_load(index, path, select, false, sink);
    }

    fn start_load(
        &mut self,
        index: usize,
        path: PathBuf,
        select: Option<Box<[u16]>>,
        keep_cursor: bool,
        sink: &LoadSink,
    ) {
        if let Some(p) = self.pending.take() {
            p.cancel.store(true, Ordering::Relaxed);
        }
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.pending = Some(Pending {
            id,
            path: path.clone(),
            select,
            keep_cursor,
            cancel: cancel.clone(),
            started: Instant::now(),
        });
        self.search.clear();
        let sink = sink.clone();
        std::thread::spawn(move || {
            let result = listing::load(&path, &cancel);
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            if sink.tx.send(Loaded { panel: index, id, result }).is_ok() {
                sink.hwnd.post(WM_APP_LOADED);
            }
        });
    }

    /// Reload the current directory, keeping cursor, scroll position and marks.
    pub fn reload(&mut self, index: usize, sink: &LoadSink) {
        if self.dir.as_os_str().is_empty() {
            return;
        }
        let search = std::mem::take(&mut self.search);
        self.start_load(index, self.dir.clone(), None, true, sink);
        self.search = search;
    }

    /// Parent directory, with the cursor on the directory we came from.
    pub fn go_up(&mut self, index: usize, sink: &LoadSink) {
        if let (Some(parent), Some(name)) = (self.dir.parent(), self.dir.file_name()) {
            let sel = to_utf16_nul(name);
            let sel = sel[..sel.len() - 1].into();
            self.navigate(index, parent.to_path_buf(), Some(sel), sink);
        }
    }

    /// Root of the current drive.
    pub fn go_root(&mut self, index: usize, sink: &LoadSink) {
        if let Some(root) = self.dir.ancestors().last()
            && root != self.dir
        {
            self.navigate(index, root.to_path_buf(), None, sink);
        }
    }

    /// Apply a load result if it is still the current one.
    pub fn apply(&mut self, id: u64, result: Result<Listing, String>) -> Applied {
        let Some(p) = self.pending.take_if(|p| p.id == id) else {
            return Applied::Stale;
        };
        match result {
            Ok(l) => {
                let same_dir = l.dir == self.dir;
                let keep = same_dir && p.keep_cursor;
                let select = l.select.or(if keep { self.current().map(|e| e.name.clone()) } else { p.select });
                let marked: HashSet<Box<[u16]>> = if same_dir {
                    self.entries.iter().filter(|e| e.marked).map(|e| e.name.clone()).collect()
                } else {
                    HashSet::new()
                };
                let old_cursor = self.cursor;
                self.dir = l.dir;
                self.entries = l.entries;
                self.dirs = l.dirs;
                self.files = l.files;
                self.bytes = l.bytes;
                self.archive = l.archive;
                self.error = None;
                if same_dir {
                    let names: HashSet<&[u16]> = self.entries.iter().filter(|e| e.is_dir()).map(|e| &*e.name).collect();
                    self.sizes.retain(|name, size| {
                        let keep = names.contains(&**name);
                        if !keep {
                            size.cancel();
                        }
                        keep
                    });
                } else {
                    self.clear_sizes();
                }
                if !marked.is_empty() {
                    for e in &mut self.entries {
                        e.marked = marked.contains(&e.name);
                    }
                }
                // Selected entry gone (e.g. deleted)? Stay at the same position.
                let idx = select.and_then(|s| self.entries.iter().position(|e| *e.name == *s)).unwrap_or(if same_dir {
                    old_cursor
                } else {
                    0
                });
                if !same_dir {
                    self.top = 0;
                }
                if keep {
                    // Auto/manual refresh: the view stays where the user scrolled it.
                    self.cursor = idx.min(self.entries.len().saturating_sub(1));
                    self.clamp_top();
                } else {
                    self.set_cursor(idx);
                }
                Applied::Ok { dir_changed: !same_dir }
            }
            Err(e) => {
                self.error = Some(format!("{}: {}", p.path.display(), e.trim()));
                Applied::Failed { path: p.path }
            }
        }
    }

    // ---------- marks ----------

    /// Toggle the mark of the entry under the cursor (`..` is never marked).
    pub fn toggle_mark(&mut self) {
        if let Some(e) = self.entries.get_mut(self.cursor)
            && e.kind != Kind::Parent
        {
            e.marked = !e.marked;
        }
    }

    /// Mark everything; if everything is marked already, unmark everything.
    pub fn mark_all(&mut self) {
        let all = self.entries.iter().filter(|e| e.kind != Kind::Parent).all(|e| e.marked);
        for e in self.entries.iter_mut().filter(|e| e.kind != Kind::Parent) {
            e.marked = !all;
        }
    }

    /// Set the mark of one entry (`..` is never marked).
    pub fn set_mark(&mut self, i: usize, marked: bool) {
        if let Some(e) = self.entries.get_mut(i)
            && e.kind != Kind::Parent
        {
            e.marked = marked;
        }
    }

    /// Set the mark of every entry `pick` accepts (never `..`); returns how many matched.
    pub fn mark_where(&mut self, marked: bool, pick: impl Fn(&Entry) -> bool) -> usize {
        let mut n = 0;
        for e in self.entries.iter_mut().filter(|e| e.kind != Kind::Parent) {
            if pick(e) {
                e.marked = marked;
                n += 1;
            }
        }
        n
    }

    /// Invert the marks of all files; directories keep theirs.
    pub fn invert_file_marks(&mut self) {
        for e in self.entries.iter_mut().filter(|e| !e.is_dir()) {
            e.marked = !e.marked;
        }
    }

    pub fn clear_marks(&mut self) {
        for e in &mut self.entries {
            e.marked = false;
        }
    }

    /// Number of marked entries and their total size (folders only once counted).
    pub fn marked(&self) -> (usize, u64) {
        let size = |e: &Entry| self.size_of(e).map_or(e.size, DirSize::bytes);
        self.entries.iter().filter(|e| e.marked).fold((0, 0), |(n, b), e| (n + 1, b + size(e)))
    }

    // ---------- folder sizes ----------

    /// The counted (or being counted) size of a folder entry.
    pub fn size_of(&self, e: &Entry) -> Option<&DirSize> {
        if e.kind != Kind::Dir {
            return None;
        }
        self.sizes.get(&e.name).map(|s| &**s)
    }

    /// Start counting the size of folder entry `i`, unless known already. `true` if started.
    pub fn count_size(&mut self, i: usize, sink: &LoadSink) -> bool {
        let Some(e) = self.entries.get(i).filter(|e| e.kind == Kind::Dir) else { return false };
        if self.sizes.contains_key(&e.name) {
            return false;
        }
        let path = self.dir.join(e.os_name());
        let notify = (sink.hwnd, WM_APP_SIZE);
        let size = match self.archive.as_ref().and_then(|_| archive::split(&path)) {
            Some((file, inner)) => dirsize::start_in_archive(file, inner, notify),
            None => dirsize::start(&path, notify),
        };
        self.sizes.insert(e.name.clone(), size);
        true
    }

    /// Start counting every folder not counted yet.
    pub fn count_all_sizes(&mut self, sink: &LoadSink) {
        for i in 0..self.entries.len() {
            self.count_size(i, sink);
        }
    }

    pub fn sizes_running(&self) -> bool {
        self.sizes.values().any(|s| !s.done())
    }

    /// Stop the counts still running and forget them; `true` if there were any.
    pub fn cancel_sizes(&mut self) -> bool {
        let before = self.sizes.len();
        self.sizes.retain(|_, s| {
            let done = s.done();
            if !done {
                s.cancel();
            }
            done
        });
        self.sizes.len() != before
    }

    pub fn clear_sizes(&mut self) {
        for s in self.sizes.values() {
            s.cancel();
        }
        self.sizes.clear();
    }

    /// What an operation acts on: the marked entries, or else the entry under the cursor.
    pub fn targets(&self) -> Vec<PathBuf> {
        let marked: Vec<PathBuf> =
            self.entries.iter().filter(|e| e.marked).map(|e| self.dir.join(e.os_name())).collect();
        if !marked.is_empty() {
            return marked;
        }
        match self.current() {
            Some(e) if e.kind != Kind::Parent => vec![self.dir.join(e.os_name())],
            _ => Vec::new(),
        }
    }

    /// Entry under the cursor.
    pub fn current(&self) -> Option<&Entry> {
        self.entries.get(self.cursor)
    }

    /// Put the cursor on entry `i` (clamped) and scroll it into view.
    pub fn set_cursor(&mut self, i: usize) {
        self.cursor = i.min(self.entries.len().saturating_sub(1));
        self.ensure_visible();
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let i = (self.cursor as isize + delta).max(0) as usize;
        self.set_cursor(i);
    }

    /// Rows per page (PgUp/PgDn).
    pub fn page(&self) -> isize {
        self.rows_visible.saturating_sub(1).max(1) as isize
    }

    /// Scroll so the cursor is visible.
    pub fn ensure_visible(&mut self) {
        let rows = self.rows_visible.max(1);
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + rows {
            self.top = self.cursor + 1 - rows;
        }
        self.clamp_top();
    }

    /// Scroll the view by `delta` rows; the cursor stays.
    pub fn scroll(&mut self, delta: isize) {
        self.top = (self.top as isize + delta).max(0) as usize;
        self.clamp_top();
    }

    /// Scroll to an absolute position without touching the cursor.
    pub fn scroll_to(&mut self, top: usize) {
        self.top = top;
        self.clamp_top();
    }

    /// Largest possible first visible row.
    pub fn max_top(&self) -> usize {
        self.entries.len().saturating_sub(self.rows_visible.max(1))
    }

    pub fn clamp_top(&mut self) {
        self.top = self.top.min(self.max_top());
    }

    /// Quick search: append a character and jump to the first matching entry.
    /// If nothing matches, the character is dropped.
    pub fn search_push(&mut self, c: u16) {
        self.search.push(c);
        if !self.search_jump() {
            self.search.pop();
        }
    }

    /// Quick search: remove the last character.
    pub fn search_pop(&mut self) {
        self.search.pop();
        if !self.search.is_empty() {
            self.search_jump();
        }
    }

    fn search_jump(&mut self) -> bool {
        let hit =
            self.entries.iter().position(|e| e.kind != Kind::Parent && starts_with_ignore_case(&e.name, &self.search));
        if let Some(i) = hit {
            self.set_cursor(i);
        }
        hit.is_some()
    }

    /// How long the current load has been running, if any.
    pub fn loading_for_ms(&self) -> Option<u128> {
        self.pending.as_ref().map(|p| p.started.elapsed().as_millis())
    }

    /// The path to show in the path bar: the one being loaded, else the current one.
    pub fn shown_path(&self) -> &Path {
        match &self.pending {
            Some(p) if p.started.elapsed().as_millis() >= crate::app::LOADING_DELAY_MS as u128 => &p.path,
            _ => &self.dir,
        }
    }
}
