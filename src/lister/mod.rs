//! The lister: a separate window that shows one file as text, binary or hex (F3, like Total
//! Commander's Lister). Any number of listers can be open; they run on the UI thread next to
//! the main window, each window with its own `Lister`.
//!
//! The file is memory-mapped (`sys::mapped`), so opening is instant whatever the size, and only
//! what is on screen is ever decoded (`text`). Searching runs on a background thread (`search`).

mod draw;
mod input;
pub(crate) mod search;
pub(crate) mod text;
mod window;

use crate::i18n::{t, tf};
use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{WM_APP, WM_CLOSE};

use crate::app::format::utf16;
use crate::app::text_input::TextInput;
use crate::lister::search::{Outcome, Pattern, Running};
use crate::lister::text::{CodePages, Doc, Encoding, Mode};
use crate::sys::gfx::Gfx;
use crate::sys::mapped::MappedFile;
use crate::sys::win::{self, SendHwnd};
use crate::theme::{DARK, Theme};

pub use window::{close_all, open, repaint_all};

/// Posted to a lister window by its search thread (progress and result).
const WM_APP_SEARCH: u32 = WM_APP + 1;

/// What is saved in config.json; shared by all lister windows.
#[derive(Clone)]
pub struct Settings {
    /// Normal window position (left, top, right, bottom) + maximized, of the last closed lister.
    pub window: Option<([i32; 4], bool)>,
    /// Wrap long lines in text mode.
    pub wrap: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { window: None, wrap: true }
    }
}

/// State shared by all lister windows.
struct Shared {
    settings: Settings,
    /// Last search term (preselected in the search dialog).
    term: String,
    match_case: bool,
}

static SHARED: Mutex<Shared> =
    Mutex::new(Shared { settings: Settings { window: None, wrap: true }, term: String::new(), match_case: false });

fn shared() -> MutexGuard<'static, Shared> {
    SHARED.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn settings() -> Settings {
    shared().settings.clone()
}

pub fn set_settings(settings: Settings) {
    shared().settings = settings;
}

pub(crate) fn code_pages() -> &'static CodePages {
    static PAGES: OnceLock<CodePages> = OnceLock::new();
    PAGES.get_or_init(|| CodePages { ansi: win::code_page(false), oem: win::code_page(true) })
}

pub(crate) struct Lister {
    hwnd: HWND,
    gfx: Gfx,
    theme: &'static Theme,
    file: Arc<MappedFile>,
    pages: &'static CodePages,
    mode: Mode,
    enc: Encoding,
    wrap: bool,
    /// Byte offset of the first line on screen.
    top: usize,
    /// Columns scrolled to the right (when lines don't wrap).
    left: usize,
    /// Lines and columns that fit into the window.
    rows: usize,
    cols: usize,
    /// Width of one character of the fixed-width font (DIPs).
    char_w: f32,
    /// Cached `Doc::last_top` (slow inside very long lines); reset whenever the layout changes.
    last_top: Option<usize>,
    /// Byte range of the last search match, highlighted.
    hit: Option<(usize, usize)>,
    /// The search dialog, while open.
    find: Option<TextInput>,
    /// A search ran in this window: from now on F3 finds the next match instead of closing.
    searched: bool,
    running: Option<Running>,
    /// Shown in the status bar until the next key press ("not found" …).
    message: Option<String>,
    /// Dragging the scrollbar thumb: where it was grabbed, relative to the thumb's top.
    drag: Option<f32>,
    /// Mouse wheel movement not yet turned into whole lines.
    wheel: i32,
}

impl Lister {
    fn new(hwnd: HWND, file: MappedFile, dpi: f32) -> Result<Lister, String> {
        let mut gfx = Gfx::new().map_err(|e| e.message())?;
        gfx.set_dpi(dpi);
        let char_w = gfx.measure(&[b'M' as u16; 64], &gfx.fonts.mono) / 64.0;
        let (mode, enc) = text::detect(file.bytes());
        let mut lister = Lister {
            hwnd,
            gfx,
            theme: &DARK,
            file: Arc::new(file),
            pages: code_pages(),
            mode,
            enc,
            wrap: settings().wrap,
            top: 0,
            left: 0,
            rows: 1,
            cols: 80,
            char_w: char_w.max(1.0),
            last_top: None,
            hit: None,
            find: None,
            searched: false,
            running: None,
            message: None,
            drag: None,
            wheel: 0,
        };
        lister.top = lister.doc().base();
        lister.layout();
        Ok(lister)
    }

    /// The file as currently shown.
    fn doc(&self) -> Doc<'_> {
        Doc {
            data: self.file.bytes(),
            mode: self.mode,
            enc: self.enc,
            wrap: self.wrap,
            cols: self.cols,
            pages: self.pages,
        }
    }

    fn invalidate(&self) {
        win::invalidate(self.hwnd);
    }

    fn close(&self) {
        SendHwnd::new(self.hwnd).post(WM_CLOSE);
    }

    /// Remember window position and wrapping for the next lister (and config.json).
    fn save_settings(&self) {
        let window = crate::window::window_placement(self.hwnd);
        set_settings(Settings { window, wrap: self.wrap });
    }

    /// After a resize: lines and columns that fit; keeps the first line in place.
    fn layout(&mut self) {
        let (w, h) = self.gfx.size_dips(self.hwnd);
        let area = draw::text_area(w, h);
        self.rows = ((area.bottom - area.top) / draw::LINE_H).floor().max(1.0) as usize;
        self.cols = ((area.right - area.left) / self.char_w).floor().max(8.0) as usize;
        self.relayout();
    }

    /// The way lines are cut changed: snap `top` to a line start again.
    fn relayout(&mut self) {
        self.last_top = None;
        self.top = self.doc().align(self.top);
        self.clamp();
    }

    fn last_top(&mut self) -> usize {
        if let Some(t) = self.last_top {
            return t;
        }
        let t = self.doc().last_top(self.rows);
        self.last_top = Some(t);
        t
    }

    /// Keep the view inside the file: no scrolling past the last page or the widest line.
    fn clamp(&mut self) {
        let last = self.last_top();
        self.top = self.top.min(last);
        let max_left = match self.doc().content_width() {
            Some(w) => w.saturating_sub(self.cols),
            None => text::MAX_LINE,
        };
        self.left = self.left.min(max_left);
    }

    /// Scroll by `n` lines (negative = up).
    fn scroll(&mut self, n: isize) {
        if n > 0 {
            let last = self.last_top();
            let doc = self.doc();
            let mut t = self.top;
            for _ in 0..n {
                if t >= last {
                    break;
                }
                t = doc.next(t);
            }
            self.top = t.min(last);
        } else {
            self.top = self.doc().back(self.top, n.unsigned_abs());
        }
    }

    fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.left = 0;
        self.relayout();
    }

    fn set_encoding(&mut self, enc: Encoding) {
        self.enc = enc;
        self.relayout();
    }

    fn toggle_wrap(&mut self) {
        self.wrap = !self.wrap;
        self.left = 0;
        self.relayout();
    }

    /// Scroll so that byte `pos` is on screen (a third down from the top), also horizontally.
    fn show(&mut self, pos: usize) {
        let doc = self.doc();
        let line = doc.align(pos);
        let outside = pos < self.top || pos >= doc.view_end(self.top, self.rows);
        let top = if outside { doc.back(line, self.rows / 3) } else { self.top };
        let col = doc.line(line).offs.iter().position(|&o| o != usize::MAX && o >= pos);
        if let Some(col) = col
            && (col < self.left || col >= self.left + self.cols)
        {
            self.left = col.saturating_sub(self.cols / 3);
        }
        self.top = top;
        self.clamp();
    }

    fn open_find(&mut self) {
        self.find = Some(TextInput::selected(utf16(&shared().term)));
    }

    /// Enter in the search dialog.
    fn commit_find(&mut self) {
        let Some(input) = self.find.take() else { return };
        let term = input.string();
        if term.is_empty() {
            return;
        }
        shared().term = term;
        self.hit = None;
        self.search(true);
    }

    /// Find the next (`forward`) or previous match of the last search term.
    fn search(&mut self, forward: bool) {
        let (term, match_case) = {
            let s = shared();
            (s.term.clone(), s.match_case)
        };
        if term.is_empty() {
            self.open_find();
            return;
        }
        if self.running.is_some() {
            return;
        }
        let doc = self.doc();
        let base = doc.base();
        let len = self.file.bytes().len();
        let end = doc.view_end(self.top, self.rows);
        let visible = |s: usize| s >= self.top && s < end;
        // Go on from the match on screen, otherwise from the top (forward) or bottom (back).
        let from = match self.hit {
            Some((s, _)) if visible(s) && forward => Some(s + 1),
            Some((s, _)) if visible(s) => s.checked_sub(1),
            _ if forward => Some(self.top),
            _ => end.checked_sub(1),
        };
        // Binary and hex mode show single bytes, so that is what is searched there.
        let enc = if self.mode == Mode::Text { self.enc } else { self.enc.single_byte() };
        let pattern = Pattern::new(&term, enc, self.pages, !match_case, base);
        self.searched = true;
        match (from, pattern) {
            (Some(from), Some(pattern)) => {
                let notify = (SendHwnd::new(self.hwnd), WM_APP_SEARCH);
                self.running = Some(search::start(self.file.clone(), pattern, from, forward, (base, len), notify));
                self.message = None;
            }
            _ => self.message = Some(tf("lister.not_found", &[("term", &term)])),
        }
    }

    /// `WM_APP_SEARCH`: progress (just repaint) or the result.
    fn on_search_event(&mut self) {
        if let Some(running) = &self.running {
            match running.result.try_recv() {
                Ok(outcome) => {
                    self.running = None;
                    match outcome {
                        Outcome::Found(s, e) => {
                            self.hit = Some((s, e));
                            self.show(s);
                        }
                        Outcome::NotFound => self.message = Some(tf("lister.not_found", &[("term", &shared().term)])),
                        Outcome::Cancelled => self.message = Some(t("lister.search_cancelled").into()),
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.running = None,
            }
        }
        self.invalidate();
    }
}
