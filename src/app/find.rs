//! The search dialog (Alt+F7): name mask, folder and text to look for, and the hits arriving
//! live from `files::find`. The last search is kept; Alt+F7 shows it again.

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::Win32::UI::Input::KeyboardAndMouse::*;

use crate::app::format::{parse_path, utf16};
use crate::app::layout::ROW_H;
use crate::app::text_input::TextInput;
use crate::app::{App, Mode, TIMER_FIND, WM_APP_FIND};
use crate::files::find::{self, Hit, Query, Search};
use crate::files::folder_kind::Action;
use crate::files::names::natural_cmp;
use crate::i18n::tf;
use crate::sys::win::{self, SendHwnd};

/// Translation keys of the input fields' labels, in order.
pub(crate) const LABELS: [&str; 3] = ["find.name", "find.dir", "find.text"];
/// `Find::focus` value of the result list (the inputs are 0..3).
pub(crate) const LIST: usize = 3;
/// Repaint interval while searching (the folder count grows).
const REPAINT_MS: u32 = 100;

pub(crate) struct Find {
    /// Name mask, folder, text.
    pub(crate) inputs: [TextInput; 3],
    /// Input with the keyboard focus, or `LIST`.
    pub(crate) focus: usize,
    pub(crate) hits: Vec<Hit>,
    pub(crate) sel: usize,
    /// First visible hit.
    pub(crate) top: usize,
    /// Folder searched (hits are shown relative to it).
    pub(crate) root: PathBuf,
    pub(crate) running: Option<Search>,
    pub(crate) started: Instant,
    /// Duration of the finished search.
    pub(crate) elapsed: Duration,
    pub(crate) dirs: u64,
    pub(crate) cancelled: bool,
    /// Why the search could not start (bad folder).
    pub(crate) error: Option<String>,
    /// A search has been started at least once (otherwise the status line shows a hint).
    pub(crate) searched: bool,
}

impl Find {
    fn new(dir: &Path) -> Find {
        Find {
            inputs: [
                TextInput::new(Vec::new()),
                TextInput::new(utf16(&dir.display().to_string())),
                TextInput::new(Vec::new()),
            ],
            focus: 0,
            hits: Vec::new(),
            sel: 0,
            top: 0,
            root: dir.to_path_buf(),
            running: None,
            started: Instant::now(),
            elapsed: Duration::ZERO,
            dirs: 0,
            cancelled: false,
            error: None,
            searched: false,
        }
    }

    /// Move the selection by `delta` (clamped) and keep it in view of `rows` rows.
    fn move_sel(&mut self, delta: isize, rows: usize) {
        if self.hits.is_empty() {
            return;
        }
        self.sel = self.sel.saturating_add_signed(delta).min(self.hits.len() - 1);
        self.show_sel(rows);
    }

    fn show_sel(&mut self, rows: usize) {
        let rows = rows.max(1);
        if self.sel < self.top {
            self.top = self.sel;
        } else if self.sel >= self.top + rows {
            self.top = self.sel + 1 - rows;
        }
    }

    /// Take new hits; `true` if the search has ended.
    fn collect(&mut self) -> bool {
        let Some(s) = &self.running else { return false };
        self.hits.append(&mut s.take_hits());
        self.dirs = s.dirs();
        if !s.done() {
            return false;
        }
        self.finish();
        true
    }

    /// Search over (done or cancelled): sort the hits by path, keeping the selected one.
    fn finish(&mut self) {
        if let Some(s) = self.running.take() {
            s.cancel();
            self.hits.append(&mut s.take_hits());
            self.dirs = s.dirs();
        }
        self.elapsed = self.started.elapsed();
        // Keep the hit the user picked while it ran; otherwise start at the first one.
        let selected = self.hits.get(self.sel).filter(|_| self.focus == LIST).map(|h| h.path.clone());
        let mut keyed: Vec<(Vec<u16>, Hit)> =
            self.hits.drain(..).map(|h| (h.path.as_os_str().encode_wide().collect(), h)).collect();
        keyed.sort_by(|a, b| natural_cmp(&a.0, &b.0));
        self.hits = keyed.into_iter().map(|(_, h)| h).collect();
        self.sel = selected.and_then(|p| self.hits.iter().position(|h| h.path == p)).unwrap_or(0);
        self.top = self.top.min(self.sel);
    }
}

impl App {
    /// Alt+F7: show the search dialog (with the last search, if any).
    pub(crate) fn open_find(&mut self) {
        let a = self.active;
        self.panels[a].search.clear();
        match &mut self.find {
            Some(f) => f.focus = if f.hits.is_empty() { 0 } else { LIST },
            None => {
                if !self.check(a, Action::Find) {
                    return;
                }
                // Inside an archive: search the folder holding it.
                let Some(dir) = self.panels[a].folder() else { return };
                self.find = Some(Find::new(dir));
            }
        }
        self.mode = Mode::Find;
    }

    /// Enter in an input: start a new search (a running one is dropped).
    fn start_find(&mut self) {
        let p = &self.panels[self.active];
        let base = p.dir.local().unwrap_or(&p.last_local).to_path_buf();
        let Some(f) = &mut self.find else { return };
        f.running = None;
        f.hits.clear();
        f.sel = 0;
        f.top = 0;
        f.dirs = 0;
        f.cancelled = false;
        f.searched = true;
        let root = parse_path(&f.inputs[1].text, &base);
        let Some(root) = root.filter(|r| r.is_dir()) else {
            f.error = Some(tf("find.dir_not_found", &[("dir", &f.inputs[1].string())]));
            return;
        };
        f.error = None;
        f.root = root.clone();
        f.inputs[1] = TextInput::new(utf16(&root.display().to_string()));
        let query = Query { root, mask: f.inputs[0].string().trim().to_string(), text: f.inputs[2].string() };
        f.running = Some(find::start(query, (SendHwnd::new(self.hwnd), WM_APP_FIND)));
        f.started = Instant::now();
        win::set_timer(self.hwnd, TIMER_FIND, REPAINT_MS);
    }

    /// `WM_APP_FIND`: new hits, or the search has ended.
    pub fn on_find_event(&mut self) {
        if let Some(f) = &mut self.find {
            f.collect();
        }
        self.invalidate();
    }

    /// `TIMER_FIND`: repaint the growing folder count while searching.
    pub(crate) fn find_tick(&mut self) {
        if let Some(f) = &mut self.find
            && !f.collect()
            && f.running.is_some()
        {
            win::set_timer(self.hwnd, TIMER_FIND, REPAINT_MS);
        }
    }

    /// Rows of hits that fit into the dialog.
    fn find_rows(&self) -> usize {
        let (w, h) = self.gfx.size_dips(self.hwnd);
        let list = Self::find_layout(w, h).list;
        ((list.bottom - list.top) / ROW_H).floor().max(1.0) as usize
    }

    /// Go to the selected hit: its folder in the active panel, the cursor on it.
    fn find_goto(&mut self) {
        let Some(path) = self.find.as_ref().and_then(|f| f.hits.get(f.sel)).map(|h| h.path.clone()) else { return };
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else { return };
        self.mode = Mode::Normal;
        let select: Box<[u16]> = name.encode_wide().collect();
        self.navigate(self.active, dir.to_path_buf().into(), Some(select));
    }

    /// F3: view the selected hit (files only).
    fn find_view(&mut self) {
        let hit = self.find.as_ref().and_then(|f| f.hits.get(f.sel)).filter(|h| !h.is_dir);
        if let Some(path) = hit.map(|h| h.path.clone()) {
            self.request_view(path);
        }
    }

    /// Keys while the dialog is open; `true` = handled.
    pub(crate) fn find_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, shift: bool) -> bool {
        let rows = self.find_rows();
        let hwnd = self.hwnd;
        let Some(f) = &mut self.find else { return false };
        let fields = if f.hits.is_empty() { LIST } else { LIST + 1 };
        match vk {
            VK_ESCAPE if f.running.is_some() => {
                f.finish();
                f.cancelled = true;
            }
            VK_ESCAPE => self.mode = Mode::Normal,
            VK_TAB => f.focus = (f.focus + if shift { fields - 1 } else { 1 }) % fields,
            VK_RETURN if f.focus == LIST => self.find_goto(),
            VK_RETURN => self.start_find(),
            VK_F3 if f.focus == LIST => self.find_view(),
            VK_DOWN if f.focus < LIST - 1 || (f.focus == LIST - 1 && !f.hits.is_empty()) => f.focus += 1,
            VK_UP if f.focus > 0 && f.focus < LIST => f.focus -= 1,
            VK_UP if f.focus == LIST && f.sel == 0 => f.focus = LIST - 1,
            _ if f.focus == LIST => match vk {
                VK_UP => f.move_sel(-1, rows),
                VK_DOWN => f.move_sel(1, rows),
                VK_PRIOR => f.move_sel(-(rows as isize), rows),
                VK_NEXT => f.move_sel(rows as isize, rows),
                VK_HOME => f.move_sel(isize::MIN / 2, rows),
                VK_END => f.move_sel(isize::MAX / 2, rows),
                _ => return false,
            },
            _ => return f.inputs[f.focus].key(vk, ctrl, hwnd),
        }
        true
    }

    /// Typed characters go into the focused input.
    pub(crate) fn find_char(&mut self, c: u16) {
        if let Some(f) = &mut self.find
            && let Some(input) = f.inputs.get_mut(f.focus)
        {
            input.insert(c);
        }
    }

    /// Click: focus an input, or select a hit (double click: go there).
    pub(crate) fn find_click(&mut self, x: f32, y: f32, double: bool) {
        let (w, h) = self.gfx.size_dips(self.hwnd);
        let layout = Self::find_layout(w, h);
        let inside = |r: &crate::sys::gfx::Rect| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
        let Some(f) = &mut self.find else { return };
        if let Some(i) = layout.inputs.iter().position(inside) {
            f.focus = i;
        } else if inside(&layout.list) {
            let idx = f.top + ((y - layout.list.top) / ROW_H) as usize;
            if idx < f.hits.len() {
                f.focus = LIST;
                f.sel = idx;
                if double {
                    self.find_goto();
                }
            }
        }
    }

    /// Wheel over the dialog scrolls the hits.
    pub(crate) fn find_wheel(&mut self, rows_delta: isize) {
        let rows = self.find_rows();
        if let Some(f) = &mut self.find {
            let max = f.hits.len().saturating_sub(rows);
            f.top = f.top.saturating_add_signed(rows_delta).min(max);
        }
    }
}
