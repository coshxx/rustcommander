//! State of one file list: directory, entries, cursor, scroll position, quick search.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::time::Instant;

use crate::files::listing::{self, Entry, Kind, Listing, Loaded, Pending};
use crate::files::names::{starts_with_ignore_case, to_utf16_nul};
use crate::files::watch::Watcher;
use crate::sys::win::SendHwnd;

pub const WM_APP_LOADED: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;

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
    /// Watches `dir` for changes; replaced whenever `dir` changes.
    pub watcher: Option<Watcher>,
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
    pub fn navigate(&mut self, index: usize, path: PathBuf, select: Option<Box<[u16]>>, sink: &LoadSink) {
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

    pub fn go_up(&mut self, index: usize, sink: &LoadSink) {
        if let (Some(parent), Some(name)) = (self.dir.parent(), self.dir.file_name()) {
            let sel = to_utf16_nul(name);
            let sel = sel[..sel.len() - 1].into();
            self.navigate(index, parent.to_path_buf(), Some(sel), sink);
        }
    }

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
                self.error = None;
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

    /// Number of marked entries and the total size of the marked files.
    pub fn marked(&self) -> (usize, u64) {
        self.entries.iter().filter(|e| e.marked).fold((0, 0), |(n, b), e| (n + 1, b + e.size))
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

    pub fn current(&self) -> Option<&Entry> {
        self.entries.get(self.cursor)
    }

    pub fn current_path(&self) -> Option<PathBuf> {
        self.current().map(|e| self.dir.join(e.os_name()))
    }

    pub fn set_cursor(&mut self, i: usize) {
        self.cursor = i.min(self.entries.len().saturating_sub(1));
        self.ensure_visible();
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let i = (self.cursor as isize + delta).max(0) as usize;
        self.set_cursor(i);
    }

    pub fn page(&self) -> isize {
        self.rows_visible.saturating_sub(1).max(1) as isize
    }

    pub fn ensure_visible(&mut self) {
        let rows = self.rows_visible.max(1);
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + rows {
            self.top = self.cursor + 1 - rows;
        }
        self.clamp_top();
    }

    pub fn scroll(&mut self, delta: isize) {
        self.top = (self.top as isize + delta).max(0) as usize;
        self.clamp_top();
    }

    /// Scroll to an absolute position without touching the cursor.
    pub fn scroll_to(&mut self, top: usize) {
        self.top = top;
        self.clamp_top();
    }

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

    pub fn loading_for_ms(&self) -> Option<u128> {
        self.pending.as_ref().map(|p| p.started.elapsed().as_millis())
    }

    pub fn shown_path(&self) -> &Path {
        match &self.pending {
            Some(p) if p.started.elapsed().as_millis() >= crate::app::LOADING_DELAY_MS as u128 => &p.path,
            _ => &self.dir,
        }
    }
}
