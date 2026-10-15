//! Application state: the two panels, the current mode (dialogs, popups),
//! running job and notices. Input handling, layout and drawing live in the
//! sibling modules and in `crate::ui`; they all extend `App` with more methods.

pub(crate) mod command_line;
pub(crate) mod dialogs;
pub(crate) mod find;
pub(crate) mod format;
pub(crate) mod jobs;
mod keyboard;
pub(crate) mod layout;
pub(crate) mod mouse;
pub(crate) mod options;
pub(crate) mod rename;
mod sizes;
pub(crate) mod text_input;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::WM_APP;

use crate::app::dialogs::{Dialog, DrivePopup};
use crate::app::jobs::{AfterJob, RunningJob};
use crate::app::layout::ROW_H;
use crate::app::mouse::Drag;
use crate::app::rename::{InlineRename, PendingRename};
use crate::app::text_input::TextInput;
use crate::config::{Config, Options};
use crate::files::archive::is_archive_name;
use crate::files::listing::{Kind, Loaded};
use crate::files::sort::SortKey;
use crate::files::watch::Watcher;
use crate::i18n::{t, tf};
use crate::panel::{Applied, LoadSink, Panel};
use crate::sys::gfx::Gfx;
use crate::sys::win::{self, SendHwnd};
use crate::theme::{DARK, Theme};
use crate::timing::Timing;

/// Directories that take longer than this show "loading" in the path bar.
pub const LOADING_DELAY_MS: u32 = 150;
/// Posted by the file operation worker when it has news (progress, conflict, done).
pub const WM_APP_JOB: u32 = WM_APP + 2;
/// Posted by a directory watcher; `wParam` = panel index.
pub const WM_APP_DIRCHANGED: u32 = WM_APP + 3;
/// Posted to itself: open the files in `App::view_requests` in lister windows.
pub const WM_APP_VIEW: u32 = WM_APP + 4;
/// Posted to itself: show the shell context menu of `App::context_request`.
pub const WM_APP_CONTEXT: u32 = WM_APP + 6;
/// Posted by the file search when there are new hits or it has ended.
pub const WM_APP_FIND: u32 = WM_APP + 7;

// Timer IDs (`WM_TIMER`).
const TIMER_LOADING: usize = 1;
const TIMER_REFRESH: [usize; 2] = [2, 3];
const TIMER_NOTICE: usize = 4;
pub(crate) const TIMER_JOB: usize = 5;
/// Auto-repeat while a mouse button is held (track paging, drag-marking at the edges).
pub(crate) const TIMER_DRAG: usize = 6;
/// Repaint while folder sizes are being counted.
const TIMER_SIZES: usize = 7;
/// Right button held this long on a row: shell context menu instead of marking.
pub(crate) const TIMER_LONG_PRESS: usize = 8;
/// Repaint while a file search runs.
pub(crate) const TIMER_FIND: usize = 9;
/// A slow second click on the name under the cursor starts renaming after the double-click time.
pub(crate) const TIMER_RENAME: usize = 10;

/// Directory change notifications trigger at most one reload per this interval.
const REFRESH_THROTTLE_MS: u32 = 250;

/// Who gets keyboard and mouse input right now.
pub(crate) enum Mode {
    /// The file lists.
    Normal,
    /// Editing the path bar of `panel` (Ctrl+L or click on the path).
    PathEdit { panel: usize, input: TextInput },
    /// Drive popup (Alt+F1 / Alt+F2).
    Drives(DrivePopup),
    /// One of the small dialogs (copy, delete, mark by mask, …).
    Dialog(Dialog),
    /// A running job waits for an answer about this existing target.
    Conflict(PathBuf),
    /// The options dialog (Ctrl+,); `sel` = highlighted row, `popup` = open language list with
    /// its highlighted entry.
    Options { sel: usize, popup: Option<usize> },
    /// The search dialog (Alt+F7); its state is `App::find`.
    Find,
    /// Renaming an entry in place (slow second click on its name).
    Rename(InlineRename),
}

/// Color and icon of a notice.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    Success,
    Info,
    Error,
}

/// Shell context menu to show (by `window.rs`, outside of `AppCell::with`).
pub(crate) struct ContextRequest {
    /// Items of one directory.
    pub(crate) paths: Vec<PathBuf>,
    /// Where to open it, in DIPs relative to the window.
    pub(crate) at: (f32, f32),
}

pub(crate) struct Notice {
    pub(crate) text: String,
    pub(crate) tone: Tone,
}

pub struct App {
    pub hwnd: HWND,
    pub gfx: Gfx,
    pub timing: Timing,
    /// Left and right file list.
    pub(crate) panels: [Panel; 2],
    /// Index of the panel with the keyboard focus.
    pub(crate) active: usize,
    /// What keys and clicks currently go to: the file lists, a dialog, a popup …
    pub(crate) mode: Mode,
    /// Handed to directory loader threads so they can report back.
    pub(crate) sink: LoadSink,
    /// Results from the directory loader threads (see `LoadSink`).
    loaded_rx: Receiver<Loaded>,
    pub(crate) theme: &'static Theme,
    /// Measured once: widths of the key and label of each key bar hint.
    pub(crate) hint_widths: Option<Vec<(f32, f32)>>,
    /// The file operation in progress (at most one at a time).
    pub(crate) job: Option<RunningJob>,
    /// Result message shown for a few seconds after a job.
    pub(crate) notice: Option<Notice>,
    /// Per panel: a reload after a change notification is already scheduled.
    refresh_armed: [bool; 2],
    /// Mouse interaction while a button is held.
    pub(crate) drag: Option<Drag>,
    /// `TIMER_DRAG` is running (auto-repeat while dragging).
    pub(crate) drag_timer_armed: bool,
    /// Panel whose scrollbar is under the mouse.
    pub(crate) hover_bar: Option<usize>,
    /// Column header under the mouse (panel, column).
    pub(crate) hover_header: Option<(usize, SortKey)>,
    /// Mouse over the gear button in the key bar.
    pub(crate) hover_gear: bool,
    /// A click that may start an inline rename (see `rename.rs`).
    pub(crate) pending_rename: Option<PendingRename>,
    /// Time, panel and row of the last left click on a row (double clicks are told apart from
    /// slow second clicks by this, see `rename.rs`).
    pub(crate) last_click: Option<(std::time::Instant, usize, usize)>,
    /// The command line's text (option `command_line`).
    pub(crate) cmdline: TextInput,
    /// Commands run from it, newest first (`history.json`).
    pub(crate) history: Vec<String>,
    /// Entry of `history` shown while browsing with Ctrl+↑/↓.
    pub(crate) history_pos: Option<usize>,
    /// Last mask used for Num+ / Num−.
    pub(crate) mask: String,
    /// Program for F4 (from config.json; Notepad by default).
    pub(crate) editor: String,
    /// Switched in the options dialog; saved right away.
    pub(crate) options: Options,
    /// The key that just opened a dialog also produces a WM_CHAR (e.g. Num+ → '+'): drop it.
    pub(crate) swallow_next_char: bool,
    /// Files to open in a lister (F3); opened by `window.rs` on `WM_APP_VIEW`.
    pub(crate) view_requests: Vec<PathBuf>,
    /// Shell context menu to open; shown by `window.rs` on `WM_APP_CONTEXT`.
    pub(crate) context_request: Option<ContextRequest>,
    /// The last file search (kept when the dialog is closed).
    pub(crate) find: Option<find::Find>,
    /// For `--timing`: the first frame / the first frame with both panels loaded was drawn.
    pub(crate) painted: bool,
    pub(crate) painted_content: bool,
}

impl App {
    /// Starts loading both panels right away, before graphics are initialized.
    pub fn new(hwnd: HWND, cfg: &Config, timing: Timing) -> App {
        let (tx, rx) = channel();
        let sink = LoadSink { tx, hwnd: SendHwnd::new(hwnd) };
        // Start the loader threads before graphics init so both run in parallel.
        let mut panels = [Panel::default(), Panel::default()];
        let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
        let defaults = [Some(PathBuf::from(r"C:\")), home];
        for (i, panel) in panels.iter_mut().enumerate() {
            let p = cfg.paths[i].clone().or(defaults[i].clone()).unwrap_or(PathBuf::from(r"C:\"));
            panel.sort = cfg.sorts[i];
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
            loaded_rx: rx,
            theme: &DARK,
            hint_widths: None,
            job: None,
            notice: None,
            refresh_armed: [false; 2],
            drag: None,
            drag_timer_armed: false,
            hover_bar: None,
            hover_header: None,
            hover_gear: false,
            pending_rename: None,
            last_click: None,
            cmdline: TextInput::new(Vec::new()),
            history: crate::config::history::load(),
            history_pos: None,
            mask: cfg.mask.clone().unwrap_or_else(|| "*.*".into()),
            editor: cfg.editor.clone().unwrap_or_else(|| crate::config::DEFAULT_EDITOR.into()),
            options: cfg.options,
            swallow_next_char: false,
            view_requests: Vec::new(),
            context_request: None,
            find: None,
            painted: false,
            painted_content: false,
        };
        app.arm_loading_indicator();
        app
    }

    /// Current state to save (on exit and when an option changes).
    pub fn config(&self, hwnd: HWND) -> Config {
        let mut cfg = Config {
            active: self.active,
            mask: Some(self.mask.clone()),
            editor: Some(self.editor.clone()),
            lister: crate::lister::settings(),
            options: self.options,
            sorts: [self.panels[0].sort, self.panels[1].sort],
            ..Default::default()
        };
        for i in 0..2 {
            if !self.panels[i].dir.as_os_str().is_empty() {
                cfg.paths[i] = Some(self.panels[i].dir.clone());
            }
        }
        cfg.window = crate::window::window_placement(hwnd);
        cfg
    }

    /// Load `path` into panel `i` and show "loading" if it takes long.
    pub(crate) fn navigate(&mut self, i: usize, path: PathBuf, select: Option<Box<[u16]>>) {
        self.panels[i].navigate(i, path, select, &self.sink);
        win::set_timer(self.hwnd, TIMER_LOADING, LOADING_DELAY_MS + 10);
    }

    /// F2 / Ctrl+R: reload the active panel; counted folder sizes are dropped.
    pub(crate) fn reload_by_hand(&mut self) {
        let a = self.active;
        self.panels[a].clear_sizes();
        self.panels[a].reload(a, &self.sink);
    }

    /// Request a repaint.
    pub(crate) fn invalidate(&self) {
        win::invalidate(self.hwnd);
    }

    /// Show a notice card for a few seconds.
    pub(crate) fn notify(&mut self, text: String, tone: Tone) {
        self.notice = Some(Notice { text, tone });
        win::set_timer(self.hwnd, TIMER_NOTICE, if tone == Tone::Error { 8000 } else { 4000 });
        self.invalidate();
    }

    /// `WM_APP_LOADED`: take over finished directory loads.
    pub fn on_loaded(&mut self) {
        while let Ok(l) = self.loaded_rx.try_recv() {
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
            match self.panels[p].apply(l.id, l.result, self.options.sort_dirs) {
                Applied::Stale => continue,
                Applied::Ok { dir_changed } => {
                    if dir_changed || self.panels[p].watcher.is_none() {
                        // Inside an archive, watch the directory holding it: a changed archive
                        // file shows up there.
                        let panel = &self.panels[p];
                        let dir = panel.archive.as_deref().and_then(Path::parent).unwrap_or(&panel.dir);
                        self.panels[p].watcher = Watcher::new(dir, SendHwnd::new(self.hwnd), WM_APP_DIRCHANGED, p);
                    }
                    self.follow_rename(p);
                }
                Applied::Failed { path } => {
                    let dir = self.panels[p].dir.clone();
                    if dir.as_os_str().is_empty() {
                        // Start path broken (e.g. drive gone)? Fall back to C:\.
                        if path != Path::new(r"C:\") {
                            self.navigate(p, PathBuf::from(r"C:\"), None);
                        }
                    } else if path == dir {
                        // The shown directory can't be read anymore (deleted?): go up.
                        if let Some(parent) = dir.parent() {
                            self.navigate(p, parent.to_path_buf(), None);
                        }
                    }
                }
            }
            self.timing.note(info);
        }
        self.invalidate();
    }

    /// `WM_APP_DIRCHANGED`: schedule a throttled reload of the panel.
    pub fn on_dir_changed(&mut self, panel: usize) {
        if panel < 2 && !self.refresh_armed[panel] {
            self.refresh_armed[panel] = true;
            win::set_timer(self.hwnd, TIMER_REFRESH[panel], REFRESH_THROTTLE_MS);
        }
    }

    /// `WM_TIMER`: all one-shot timers end up here.
    pub fn on_timer(&mut self, id: usize) {
        win::kill_timer(self.hwnd, id);
        match id {
            TIMER_NOTICE => self.notice = None,
            TIMER_DRAG => {
                self.drag_timer_armed = false;
                self.drag_tick();
            }
            TIMER_SIZES => self.arm_sizes_timer(),
            TIMER_LONG_PRESS => self.long_press(),
            TIMER_FIND => self.find_tick(),
            TIMER_RENAME => self.rename_tick(),
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

    /// Enter: into the directory or archive under the cursor, otherwise open the file with its
    /// default program (from inside an archive: extract it first). `shell` (Shift+Enter) opens
    /// archives with their default program as well.
    pub(crate) fn open_current(&mut self, shell: bool) {
        let a = self.active;
        let p = &self.panels[a];
        let Some(e) = p.current() else { return };
        let (kind, in_archive, is_archive) = (e.kind, p.archive.is_some(), is_archive_name(&e.name));
        let path = p.dir.join(e.os_name());
        match kind {
            Kind::Parent => self.panels[a].go_up(a, &self.sink),
            Kind::Dir => self.navigate(a, path, None),
            _ if in_archive => self.extract_then(a, path, AfterJob::Open),
            _ if is_archive && !shell => self.navigate(a, path, None),
            _ => win::shell_open(path),
        }
        self.arm_loading_indicator();
    }

    /// F3: show the file under the cursor in a lister window (from an archive: extract first).
    pub(crate) fn view_current(&mut self) {
        let a = self.active;
        let p = &self.panels[a];
        let Some(e) = p.current().filter(|e| e.kind != Kind::Parent && !e.is_dir()) else { return };
        let path = p.dir.join(e.os_name());
        if p.archive.is_some() {
            self.extract_then(a, path, AfterJob::View);
        } else {
            self.request_view(path);
        }
    }

    /// F4: open the file under the cursor in the editor (not inside archives: they are
    /// read-only, changes would be lost).
    pub(crate) fn edit_current(&mut self) {
        let p = &self.panels[self.active];
        let Some(e) = p.current().filter(|e| !e.is_dir()) else { return };
        if p.archive.is_some() {
            self.notify(t("notice.read_only").into(), Tone::Info);
            return;
        }
        let path = p.dir.join(e.os_name());
        self.edit(&path);
    }

    /// Start the editor on `path`.
    pub(crate) fn edit(&mut self, path: &Path) {
        let mut cmd = std::process::Command::new(&self.editor);
        cmd.arg(path);
        if let Some(dir) = path.parent() {
            cmd.current_dir(dir);
        }
        if let Err(e) = cmd.spawn() {
            self.notify(tf("notice.editor_failed", &[("editor", &self.editor), ("error", &e)]), Tone::Error);
        }
    }

    /// Open `path` in a lister. The window is created on `WM_APP_VIEW`, outside of this
    /// handler: creating a window sends messages to the main window synchronously.
    pub(crate) fn request_view(&mut self, path: PathBuf) {
        self.view_requests.push(path);
        SendHwnd::new(self.hwnd).post(WM_APP_VIEW);
    }

    /// Shell context menu for the marked entries, else the one under the cursor (on `..`: the
    /// directory itself). `at` = mouse position; `None` = below the cursor row (keyboard).
    pub(crate) fn request_context_menu(&mut self, at: Option<(f32, f32)>) {
        let a = self.active;
        let p = &mut self.panels[a];
        if p.archive.is_some() {
            self.notify(t("notice.no_context_menu").into(), Tone::Info);
            return;
        }
        let mut paths = p.targets();
        if paths.is_empty() && !p.dir.as_os_str().is_empty() {
            paths.push(p.dir.clone());
        }
        if paths.is_empty() {
            return;
        }
        p.search.clear();
        p.ensure_visible();
        let at = at.unwrap_or_else(|| {
            let (w, h) = self.gfx.size_dips(self.hwnd);
            let p = &self.panels[a];
            let row = (p.cursor - p.top) as f32;
            (self.columns(a, w).name_x + 16.0, self.list_area(h).0 + (row + 1.0) * ROW_H)
        });
        self.context_request = Some(ContextRequest { paths, at });
        SendHwnd::new(self.hwnd).post(WM_APP_CONTEXT);
    }

    /// Menu key or Shift+F10 (`WM_CONTEXTMENU` from the keyboard).
    pub fn on_context_key(&mut self) {
        if matches!(self.mode, Mode::Normal) {
            self.request_context_menu(None);
        }
    }

    /// Ctrl+PgDn: like Enter, but only for directories and archives.
    pub(crate) fn enter_current(&mut self) {
        let p = &self.panels[self.active];
        let enterable = p.current().is_some_and(|e| {
            e.kind == Kind::Dir || (e.kind != Kind::Parent && p.archive.is_none() && is_archive_name(&e.name))
        });
        if enterable {
            self.open_current(false);
        }
    }

    /// If a panel is loading, repaint shortly after `LOADING_DELAY_MS` to show "loading".
    pub(crate) fn arm_loading_indicator(&self) {
        if self.panels.iter().any(|p| p.pending.is_some()) {
            win::set_timer(self.hwnd, TIMER_LOADING, LOADING_DELAY_MS + 10);
        }
    }
}
