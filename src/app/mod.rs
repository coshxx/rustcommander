//! Application state: the two panels, the current mode (dialogs, popups),
//! running job and notices. Input handling, layout and drawing live in the
//! sibling modules and in `crate::ui`; they all extend `App` with more methods.

pub(crate) mod dialogs;
pub(crate) mod format;
pub(crate) mod jobs;
mod keyboard;
pub(crate) mod layout;
pub(crate) mod mouse;
pub(crate) mod text_input;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::WM_APP;

use crate::app::dialogs::{Dialog, DrivePopup};
use crate::app::jobs::RunningJob;
use crate::app::mouse::Drag;
use crate::app::text_input::TextInput;
use crate::config::Config;
use crate::files::listing::{Kind, Loaded};
use crate::files::watch::Watcher;
use crate::panel::{Applied, LoadSink, Panel};
use crate::sys::gfx::Gfx;
use crate::sys::win::{self, SendHwnd};
use crate::theme::{DARK, Theme};
use crate::timing::Timing;

pub const LOADING_DELAY_MS: u32 = 150;
pub const WM_APP_JOB: u32 = WM_APP + 2;
pub const WM_APP_DIRCHANGED: u32 = WM_APP + 3;

const TIMER_LOADING: usize = 1;
const TIMER_REFRESH: [usize; 2] = [2, 3];
const TIMER_NOTICE: usize = 4;
pub(crate) const TIMER_JOB: usize = 5;

/// Auto-repeat while a mouse button is held (track paging, drag-marking at the edges).
pub(crate) const TIMER_DRAG: usize = 6;

/// Directory change notifications trigger at most one reload per this interval.
const REFRESH_THROTTLE_MS: u32 = 250;

pub(crate) enum Mode {
    Normal,
    PathEdit { panel: usize, input: TextInput },
    Drives(DrivePopup),
    Dialog(Dialog),
    /// A running job waits for an answer about this existing target.
    Conflict(PathBuf),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    Success,
    Info,
    Error,
}

pub(crate) struct Notice {
    pub(crate) text: String,
    pub(crate) tone: Tone,
}

pub struct App {
    pub hwnd: HWND,
    pub gfx: Gfx,
    pub timing: Timing,
    pub(crate) panels: [Panel; 2],
    pub(crate) active: usize,
    pub(crate) mode: Mode,
    pub(crate) sink: LoadSink,
    rx: Receiver<Loaded>,
    pub(crate) theme: &'static Theme,
    pub(crate) hint_widths: Option<Vec<(f32, f32)>>,
    pub(crate) job: Option<RunningJob>,
    pub(crate) notice: Option<Notice>,
    refresh_armed: [bool; 2],
    pub(crate) drag: Option<Drag>,
    pub(crate) drag_timer: bool,
    /// Panel whose scrollbar is under the mouse.
    pub(crate) hover_bar: Option<usize>,
    /// Last mask used for Num+ / Num−.
    pub(crate) mask: String,
    /// The key that just opened a dialog also produces a WM_CHAR (e.g. Num+ → '+'): drop it.
    pub(crate) eat_char: bool,
    pub(crate) painted: bool,
    pub(crate) painted_content: bool,
}

impl App {
    pub fn new(hwnd: HWND, cfg: &Config, timing: Timing) -> App {
        let (tx, rx) = channel();
        let sink = LoadSink { tx, hwnd: SendHwnd::new(hwnd) };
        // Start the loader threads before graphics init so both run in parallel.
        let mut panels = [Panel::default(), Panel::default()];
        let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
        let defaults = [Some(PathBuf::from(r"C:\")), home];
        for (i, panel) in panels.iter_mut().enumerate() {
            let p = cfg.paths[i].clone().or(defaults[i].clone()).unwrap_or(PathBuf::from(r"C:\"));
            panel.navigate(i, p, None, &sink);
        }
        let mut timing = timing;
        let gfx = Gfx::new().expect("Direct2D/DirectWrite unavailable");
        timing.mark("Direct2D/DirectWrite initialized");
        let app = App {
            hwnd,
            gfx,
            timing,
            panels,
            active: cfg.active.min(1),
            mode: Mode::Normal,
            sink,
            rx,
            theme: &DARK,
            hint_widths: None,
            job: None,
            notice: None,
            refresh_armed: [false; 2],
            drag: None,
            drag_timer: false,
            hover_bar: None,
            mask: cfg.mask.clone().unwrap_or_else(|| "*.*".into()),
            eat_char: false,
            painted: false,
            painted_content: false,
        };
        app.arm_timer();
        app
    }

    pub fn config(&self, hwnd: HWND) -> Config {
        let mut cfg = Config { active: self.active, mask: Some(self.mask.clone()), ..Default::default() };
        for i in 0..2 {
            if !self.panels[i].dir.as_os_str().is_empty() {
                cfg.paths[i] = Some(self.panels[i].dir.clone());
            }
        }
        cfg.window = crate::window::window_placement(hwnd);
        cfg
    }

    pub(crate) fn nav(&mut self, i: usize, path: PathBuf, select: Option<Box<[u16]>>) {
        self.panels[i].navigate(i, path, select, &self.sink);
        win::set_timer(self.hwnd, TIMER_LOADING, LOADING_DELAY_MS + 10);
    }

    pub(crate) fn invalidate(&self) {
        win::invalidate(self.hwnd);
    }

    pub(crate) fn notify(&mut self, text: String, tone: Tone) {
        self.notice = Some(Notice { text, tone });
        win::set_timer(self.hwnd, TIMER_NOTICE, if tone == Tone::Error { 8000 } else { 4000 });
        self.invalidate();
    }

    pub fn on_loaded(&mut self) {
        while let Ok(l) = self.rx.try_recv() {
            let info = match &l.result {
                Ok(li) => format!(
                    "  dir {} – {} entries in {:.2} ms",
                    li.dir.display(),
                    li.entries.len(),
                    li.elapsed.as_secs_f64() * 1000.0
                ),
                Err(e) => format!("  error: {}", e.trim()),
            };
            let p = l.panel;
            match self.panels[p].apply(l.id, l.result) {
                Applied::Stale => continue,
                Applied::Ok { dir_changed } => {
                    if dir_changed || self.panels[p].watcher.is_none() {
                        let dir = &self.panels[p].dir;
                        self.panels[p].watcher = Watcher::new(dir, SendHwnd::new(self.hwnd), WM_APP_DIRCHANGED, p);
                    }
                }
                Applied::Failed { path } => {
                    let dir = self.panels[p].dir.clone();
                    if dir.as_os_str().is_empty() {
                        // Start path broken (e.g. drive gone)? Fall back to C:\.
                        if path != Path::new(r"C:\") {
                            self.nav(p, PathBuf::from(r"C:\"), None);
                        }
                    } else if path == dir {
                        // The shown directory can't be read anymore (deleted?): go up.
                        if let Some(parent) = dir.parent() {
                            self.nav(p, parent.to_path_buf(), None);
                        }
                    }
                }
            }
            self.timing.note(info);
        }
        self.invalidate();
    }

    pub fn on_dir_changed(&mut self, panel: usize) {
        if panel < 2 && !self.refresh_armed[panel] {
            self.refresh_armed[panel] = true;
            win::set_timer(self.hwnd, TIMER_REFRESH[panel], REFRESH_THROTTLE_MS);
        }
    }

    pub fn on_timer(&mut self, id: usize) {
        win::kill_timer(self.hwnd, id);
        match id {
            TIMER_NOTICE => self.notice = None,
            TIMER_DRAG => {
                self.drag_timer = false;
                self.drag_tick();
            }
            _ if TIMER_REFRESH.contains(&id) => {
                let i = if id == TIMER_REFRESH[0] { 0 } else { 1 };
                self.refresh_armed[i] = false;
                if self.panels[i].pending.is_none() {
                    self.panels[i].reload(i, &self.sink);
                }
            }
            _ => {}
        }
        self.invalidate();
    }

    pub(crate) fn open_current(&mut self) {
        let a = self.active;
        let p = &self.panels[a];
        let Some(e) = p.current() else { return };
        match e.kind {
            Kind::Parent => self.panels[a].go_up(a, &self.sink),
            Kind::Dir => {
                let path = p.dir.join(e.os_name());
                self.nav(a, path, None);
            }
            _ => {
                if let Some(path) = p.current_path() {
                    win::shell_open(path);
                }
            }
        }
        self.arm_timer();
    }

    pub(crate) fn arm_timer(&self) {
        if self.panels.iter().any(|p| p.pending.is_some()) {
            win::set_timer(self.hwnd, TIMER_LOADING, LOADING_DELAY_MS + 10);
        }
    }
}
