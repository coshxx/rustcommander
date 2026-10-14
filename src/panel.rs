//! Zustand einer Dateiliste: Ordner, Einträge, Cursor, Scrollposition, Schnellsuche.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::time::Instant;

use crate::fsx::{self, Entry, Listing, Loaded, Pending};
use crate::win::SendHwnd;

pub const WM_APP_LOADED: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Alles, was ein Lade-Thread braucht, um sein Ergebnis zurückzumelden.
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
    /// Wird beim Layout gesetzt.
    pub rows_visible: usize,
    pub pending: Option<Pending>,
    pub search: Vec<u16>,
    pub error: Option<String>,
    pub dirs: usize,
    pub files: usize,
    pub bytes: u64,
}

impl Panel {
    pub fn navigate(&mut self, index: usize, path: PathBuf, select: Option<Box<[u16]>>, sink: &LoadSink) {
        if let Some(p) = self.pending.take() {
            p.cancel.store(true, Ordering::Relaxed);
        }
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.pending = Some(Pending {
            id,
            path: path.clone(),
            select,
            cancel: cancel.clone(),
            started: Instant::now(),
        });
        self.search.clear();
        let sink = sink.clone();
        std::thread::spawn(move || {
            let result = fsx::load(&path, &cancel);
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            if sink.tx.send(Loaded { panel: index, id, result }).is_ok() {
                sink.hwnd.post(WM_APP_LOADED);
            }
        });
    }

    pub fn reload(&mut self, index: usize, sink: &LoadSink) {
        let sel = self.current().map(|e| e.name.clone());
        self.navigate(index, self.dir.clone(), sel, sink);
    }

    pub fn go_up(&mut self, index: usize, sink: &LoadSink) {
        if let (Some(parent), Some(name)) = (self.dir.parent(), self.dir.file_name()) {
            let sel = fsx::to_wide_z(name);
            let sel = sel[..sel.len() - 1].into();
            self.navigate(index, parent.to_path_buf(), Some(sel), sink);
        }
    }

    pub fn go_root(&mut self, index: usize, sink: &LoadSink) {
        if let Some(root) = self.dir.ancestors().last() {
            if root != self.dir {
                self.navigate(index, root.to_path_buf(), None, sink);
            }
        }
    }

    /// Ergebnis übernehmen, falls es noch das aktuelle ist. `true` = übernommen.
    pub fn apply(&mut self, id: u64, result: Result<Listing, String>) -> bool {
        let Some(p) = self.pending.take_if(|p| p.id == id) else {
            return false;
        };
        match result {
            Ok(l) => {
                let same_dir = l.dir == self.dir;
                let select = l.select.or(p.select);
                self.dir = l.dir;
                self.entries = l.entries;
                self.dirs = l.dirs;
                self.files = l.files;
                self.bytes = l.bytes;
                self.error = None;
                let idx = select
                    .and_then(|s| self.entries.iter().position(|e| *e.name == *s))
                    .unwrap_or(0);
                if !same_dir {
                    self.top = 0;
                }
                self.set_cursor(idx);
            }
            Err(e) => {
                self.error = Some(format!("{}: {}", p.path.display(), e.trim()));
            }
        }
        true
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

    fn clamp_top(&mut self) {
        let max_top = self.entries.len().saturating_sub(self.rows_visible.max(1));
        self.top = self.top.min(max_top);
    }

    /// Schnellsuche: Zeichen anhängen und zum ersten passenden Eintrag springen.
    /// Gibt es keinen Treffer, wird das Zeichen verworfen.
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
        let hit = self
            .entries
            .iter()
            .position(|e| e.kind != fsx::Kind::Parent && fsx::starts_with_ci(&e.name, &self.search));
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
