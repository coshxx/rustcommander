//! Application state, input handling and drawing.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::Instant;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::{WM_APP, WM_CLOSE};

use crate::config::Config;
use crate::fsx::{self, Kind, Loaded};
use crate::gfx::{Canvas, Gfx, Rect, rect};
use crate::mask::Mask;
use crate::ops::{self, Answer, Job};
use crate::panel::{Applied, LoadSink, Panel};
use crate::theme::{Color, DARK, Theme};
use crate::timing::Timing;
use crate::watch::Watcher;
use crate::win::{self, Drive, SendHwnd};

pub const LOADING_DELAY_MS: u32 = 150;
pub const WM_APP_JOB: u32 = WM_APP + 2;
pub const WM_APP_DIRCHANGED: u32 = WM_APP + 3;

const TIMER_LOADING: usize = 1;
const TIMER_REFRESH: [usize; 2] = [2, 3];
const TIMER_NOTICE: usize = 4;
const TIMER_JOB: usize = 5;
/// Auto-repeat while a mouse button is held (track paging, drag-marking at the edges).
const TIMER_DRAG: usize = 6;
const DRAG_REPEAT_MS: u32 = 50;
/// Directory change notifications trigger at most one reload per this interval.
const REFRESH_THROTTLE_MS: u32 = 250;
/// Short jobs (rename, mkdir) finish before their progress card would appear.
const JOB_CARD_DELAY_MS: u32 = 200;

// Layout in DIPs.
const PATH_H: f32 = 38.0;
const COLS_H: f32 = 24.0;
const ROW_H: f32 = 24.0;
const STATUS_H: f32 = 26.0;
const KEYBAR_H: f32 = 30.0;
const SIZE_W: f32 = 84.0;
const DATE_W: f32 = 118.0;
const PAD: f32 = 12.0;
const ICON_W: f32 = 24.0;
const CHIP_W: f32 = 40.0;
const POPUP_W: f32 = 230.0;
const CARD_W: f32 = 400.0;
/// Width of the scrollbar's mouse zone at the panel's right edge.
const SCROLL_ZONE_W: f32 = 14.0;
const THUMB_MIN_H: f32 = 24.0;

const HINTS: [(&str, &str); 10] = [
    ("Tab", "Seite"),
    ("Einfg", "Markieren"),
    ("Num+", "Auswahl"),
    ("F5", "Kopieren"),
    ("F6", "Verschieben"),
    ("⇧F6", "Umbenennen"),
    ("F7", "Ordner"),
    ("F8", "Löschen"),
    ("Alt+F1/F2", "Laufwerk"),
    ("Strg+L", "Pfad"),
];

/// Single-line text input (path bar and dialogs).
struct TextInput {
    text: Vec<u16>,
    caret: usize,
    /// Whole text selected: typing replaces it.
    all: bool,
}

impl TextInput {
    fn new(text: Vec<u16>) -> TextInput {
        let caret = text.len();
        TextInput { text, caret, all: false }
    }

    fn selected(text: Vec<u16>) -> TextInput {
        TextInput { all: !text.is_empty(), ..TextInput::new(text) }
    }

    /// Editing keys; `true` = handled.
    fn key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, hwnd: HWND) -> bool {
        let all = std::mem::take(&mut self.all);
        match vk {
            VK_LEFT if all => self.caret = 0,
            VK_RIGHT if all => self.caret = self.text.len(),
            VK_BACK | VK_DELETE if all => {
                self.text.clear();
                self.caret = 0;
            }
            VK_LEFT => self.caret = self.caret.saturating_sub(1),
            VK_RIGHT => self.caret = (self.caret + 1).min(self.text.len()),
            VK_HOME => self.caret = 0,
            VK_END => self.caret = self.text.len(),
            VK_BACK => {
                if self.caret > 0 {
                    self.caret -= 1;
                    self.text.remove(self.caret);
                }
            }
            VK_DELETE => {
                if self.caret < self.text.len() {
                    self.text.remove(self.caret);
                }
            }
            VK_V if ctrl => {
                if all {
                    self.text.clear();
                    self.caret = 0;
                }
                if let Some(clip) = win::clipboard_text(hwnd) {
                    let clip: Vec<u16> = clip.into_iter().filter(|&c| c >= 0x20).collect();
                    let n = clip.len();
                    self.text.splice(self.caret..self.caret, clip);
                    self.caret += n;
                }
            }
            VK_A if ctrl => self.all = !self.text.is_empty(),
            _ => {
                self.all = all;
                return false;
            }
        }
        true
    }

    fn insert(&mut self, c: u16) {
        if std::mem::take(&mut self.all) {
            self.text.clear();
            self.caret = 0;
        }
        self.text.insert(self.caret, c);
        self.caret += 1;
    }

    fn string(&self) -> String {
        String::from_utf16_lossy(&self.text)
    }
}

struct DrivePopup {
    panel: usize,
    drives: Vec<Drive>,
    sel: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DialogKind {
    Copy,
    Move,
    Delete { recycle: bool },
    Rename,
    MkDir,
    /// Mark (`add`) or unmark entries by file mask.
    Select { add: bool },
}

impl DialogKind {
    fn title(self) -> &'static str {
        match self {
            DialogKind::Copy => "Kopieren",
            DialogKind::Move => "Verschieben",
            DialogKind::Delete { recycle: true } => "In den Papierkorb",
            DialogKind::Delete { recycle: false } => "Endgültig löschen",
            DialogKind::Rename => "Umbenennen",
            DialogKind::MkDir => "Neuer Ordner",
            DialogKind::Select { add: true } => "Markieren",
            DialogKind::Select { add: false } => "Markierung aufheben",
        }
    }
}

struct Dialog {
    kind: DialogKind,
    panel: usize,
    sources: Vec<PathBuf>,
    message: String,
    input: Option<TextInput>,
}

enum Mode {
    Normal,
    PathEdit { panel: usize, input: TextInput },
    Drives(DrivePopup),
    Dialog(Dialog),
    /// A running job waits for an answer about this existing target.
    Conflict(PathBuf),
}

enum Hit {
    Drive(usize),
    Path(usize),
    Row(usize, usize),
    Scrollbar(usize),
    Panel(usize),
    Popup(usize),
    Nothing,
}

/// Mouse interaction while a button is held (the window has the mouse capture).
#[derive(Clone, Copy)]
enum Drag {
    /// Dragging the scrollbar thumb; `grab` = mouse offset from the thumb's top.
    Thumb { panel: usize, grab: f32 },
    /// Left button held on the track: page towards the mouse, auto-repeating.
    Page { panel: usize, y: f32 },
    /// Right button held: every row the mouse passes gets `marked`.
    Mark { panel: usize, marked: bool, last: usize, y: f32 },
}

struct ScrollBar {
    zone: Rect,
    thumb_y: f32,
    thumb_h: f32,
}

struct RunningJob {
    handle: ops::Handle,
    title: &'static str,
    progress: ops::Progress,
    source_panel: usize,
    started: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tone {
    Success,
    Info,
    Error,
}

struct Notice {
    text: String,
    tone: Tone,
}

pub struct App {
    pub hwnd: HWND,
    pub gfx: Gfx,
    pub timing: Timing,
    panels: [Panel; 2],
    active: usize,
    mode: Mode,
    sink: LoadSink,
    rx: Receiver<Loaded>,
    theme: &'static Theme,
    hint_widths: Option<Vec<(f32, f32)>>,
    job: Option<RunningJob>,
    notice: Option<Notice>,
    refresh_armed: [bool; 2],
    drag: Option<Drag>,
    drag_timer: bool,
    /// Panel whose scrollbar is under the mouse.
    hover_bar: Option<usize>,
    /// Last mask used for Num+ / Num−.
    mask: String,
    /// The key that just opened a dialog also produces a WM_CHAR (e.g. Num+ → '+'): drop it.
    eat_char: bool,
    painted: bool,
    painted_content: bool,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn fmt_size(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if b < 1024 {
        return format!("{b} B");
    }
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    let s = if v < 10.0 { format!("{v:.1}") } else { format!("{v:.0}") };
    format!("{} {}", s.replace('.', ","), UNITS[u])
}

fn fmt_date(t: u64) -> String {
    if t == 0 {
        return String::new();
    }
    match win::local_time(t) {
        Some(lt) => format!("{:02}.{:02}.{:04}  {:02}:{:02}", lt.wDay, lt.wMonth, lt.wYear, lt.wHour, lt.wMinute),
        None => String::new(),
    }
}

fn icon(kind: Kind, t: &Theme) -> (u16, Color) {
    match kind {
        Kind::Parent => (0xE74A, t.icon_parent),
        Kind::Dir => (0xE8D5, t.icon_folder),
        Kind::File => (0xE7C3, t.icon_file),
        Kind::Image => (0xEB9F, t.icon_image),
        Kind::Audio => (0xE8D6, t.icon_audio),
        Kind::Video => (0xE714, t.icon_video),
        Kind::Archive => (0xF012, t.icon_archive),
        Kind::Code => (0xE943, t.icon_code),
        Kind::Exe => (0xECAA, t.icon_exe),
        Kind::Doc => (0xE8A5, t.icon_doc),
    }
}

fn drive_info(kind: u32) -> (u16, &'static str) {
    match kind {
        2 => (0xE88E, "Wechseldatenträger"),
        4 => (0xE8CE, "Netzwerk"),
        5 => (0xE958, "CD/DVD"),
        6 => (0xEDA2, "RAM-Disk"),
        _ => (0xEDA2, "Lokaler Datenträger"),
    }
}

/// „name“ for a single item, otherwise "N Elemente".
fn describe(sources: &[PathBuf]) -> String {
    match sources {
        [one] => format!("„{}“", one.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()),
        many => format!("{} Elemente", many.len()),
    }
}

/// File name check; `allow_sep` permits `\` for nested new directories.
fn valid_name(name: &str, allow_sep: bool) -> bool {
    let bad = |c: char| matches!(c, '<' | '>' | ':' | '"' | '/' | '|' | '?' | '*') || (c == '\\' && !allow_sep);
    !name.is_empty() && name != "." && name != ".." && !name.chars().any(|c| bad(c) || (c as u32) < 0x20)
}

/// Turn path bar input into an absolute path.
fn parse_path(input: &[u16], base: &Path) -> Option<PathBuf> {
    let expanded = win::expand_env(input);
    let mut s = String::from_utf16_lossy(&expanded).trim().trim_matches('"').to_string();
    if s.is_empty() {
        return None;
    }
    if s.len() == 2 && s.ends_with(':') {
        s.push('\\');
    }
    let p = PathBuf::from(s);
    let p = if p.is_absolute() { p } else { base.join(p) };
    std::path::absolute(p).ok()
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
        cfg.window = crate::window_placement(hwnd);
        cfg
    }

    fn nav(&mut self, i: usize, path: PathBuf, select: Option<Box<[u16]>>) {
        self.panels[i].navigate(i, path, select, &self.sink);
        win::set_timer(self.hwnd, TIMER_LOADING, LOADING_DELAY_MS + 10);
    }

    fn invalidate(&self) {
        win::invalidate(self.hwnd);
    }

    fn notify(&mut self, text: String, tone: Tone) {
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

    // ---------- Jobs ----------

    fn start_job(&mut self, job: Job, title: &'static str, source_panel: usize) {
        let handle = ops::start(job, Some((SendHwnd::new(self.hwnd), WM_APP_JOB)));
        self.job = Some(RunningJob {
            handle,
            title,
            progress: ops::Progress::default(),
            source_panel,
            started: Instant::now(),
        });
        self.notice = None;
        win::set_timer(self.hwnd, TIMER_JOB, JOB_CARD_DELAY_MS + 10);
    }

    pub fn on_job(&mut self) {
        loop {
            let Some(job) = &mut self.job else { break };
            let Ok(ev) = job.handle.events.try_recv() else { break };
            match ev {
                ops::Event::Progress(p) => job.progress = p,
                ops::Event::Conflict(path) => self.mode = Mode::Conflict(path),
                ops::Event::Done(summary) => {
                    if let Some(job) = self.job.take() {
                        self.finish_job(job, summary);
                    }
                }
            }
        }
        self.invalidate();
    }

    fn finish_job(&mut self, job: RunningJob, s: ops::Summary) {
        if matches!(self.mode, Mode::Conflict(_)) {
            self.mode = Mode::Normal;
        }
        let (text, tone) = if let Some(first) = s.errors.first() {
            let more = if s.errors.len() > 1 { format!(" (+{} weitere)", s.errors.len() - 1) } else { String::new() };
            (format!("{}: {first}{more}", job.title), Tone::Error)
        } else if s.cancelled {
            (format!("{} abgebrochen", job.title), Tone::Info)
        } else {
            let what = if s.done == 1 { "Element" } else { "Elemente" };
            (format!("{}: fertig ({} {what})", job.title, s.done), Tone::Success)
        };
        self.notify(text, tone);
        if s.errors.is_empty() && !s.cancelled {
            self.panels[job.source_panel].clear_marks();
        }
        for i in 0..2 {
            let dir = self.panels[i].dir.clone();
            // Put the cursor on a new/renamed entry if it lives in this panel.
            let select = s
                .select
                .as_ref()
                .and_then(|p| p.strip_prefix(&dir).ok())
                .and_then(|rel| rel.components().next())
                .map(|c| fsx::to_wide_z(c.as_os_str()))
                .map(|w| w[..w.len() - 1].into());
            match select {
                Some(name) => self.nav(i, dir, Some(name)),
                None => self.panels[i].reload(i, &self.sink),
            }
        }
    }

    fn answer_conflict(&mut self, a: Answer) {
        if let Some(job) = &self.job {
            job.handle.answer(a);
        }
        self.mode = Mode::Normal;
    }

    // ---------- Layout ----------

    fn panel_x(&self, i: usize, w: f32) -> (f32, f32) {
        let half = (w / 2.0).floor();
        if i == 0 { (0.0, half) } else { (half + 1.0, w - half - 1.0) }
    }

    fn list_area(h: f32) -> (f32, f32) {
        let top = PATH_H + COLS_H;
        (top, (h - KEYBAR_H - STATUS_H - top).max(0.0))
    }

    pub fn layout(&mut self) {
        let (_, h) = self.gfx.size_dips(self.hwnd);
        let rows = (Self::list_area(h).1 / ROW_H).floor().max(1.0) as usize;
        for p in &mut self.panels {
            p.rows_visible = rows;
            // Only clamp: the user may have scrolled the cursor out of view on purpose.
            p.clamp_top();
        }
    }

    fn hit(&self, x: f32, y: f32) -> Hit {
        let (w, h) = self.gfx.size_dips(self.hwnd);
        if let Mode::Drives(d) = &self.mode {
            let (px, _) = self.panel_x(d.panel, w);
            let r = self.popup_rect(px, d.drives.len());
            if x >= r.left && x < r.right && y >= r.top + 4.0 && y < r.bottom - 4.0 {
                return Hit::Popup(((y - r.top - 4.0) / ROW_H) as usize);
            }
        }
        if y >= h - KEYBAR_H {
            return Hit::Nothing;
        }
        let p = if x < self.panel_x(1, w).0 { 0 } else { 1 };
        let (px, _) = self.panel_x(p, w);
        if y < PATH_H {
            return if x < px + PAD + CHIP_W { Hit::Drive(p) } else { Hit::Path(p) };
        }
        let (top, lh) = Self::list_area(h);
        if y >= top && y < top + lh {
            if self.scrollbar(p, w, h).is_some_and(|sb| x >= sb.zone.left) {
                return Hit::Scrollbar(p);
            }
            let row = self.panels[p].top + ((y - top) / ROW_H) as usize;
            if row < self.panels[p].entries.len() {
                return Hit::Row(p, row);
            }
        }
        Hit::Panel(p)
    }

    /// Scrollbar geometry; `None` if everything fits.
    fn scrollbar(&self, i: usize, w: f32, h: f32) -> Option<ScrollBar> {
        let p = &self.panels[i];
        let (top, lh) = Self::list_area(h);
        if p.entries.len() <= p.rows_visible || lh <= 0.0 {
            return None;
        }
        let (x, pw) = self.panel_x(i, w);
        let thumb_h = (lh * p.rows_visible as f32 / p.entries.len() as f32).max(THUMB_MIN_H).min(lh);
        let thumb_y = top + (lh - thumb_h) * p.top as f32 / p.max_top() as f32;
        Some(ScrollBar { zone: rect(x + pw - SCROLL_ZONE_W, top, SCROLL_ZONE_W, lh), thumb_y, thumb_h })
    }

    fn popup_rect(&self, px: f32, n: usize) -> Rect {
        rect(px + PAD - 4.0, PATH_H - 2.0, POPUP_W, n as f32 * ROW_H + 8.0)
    }

    // ---------- Actions ----------

    fn open_current(&mut self) {
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

    fn arm_timer(&self) {
        if self.panels.iter().any(|p| p.pending.is_some()) {
            win::set_timer(self.hwnd, TIMER_LOADING, LOADING_DELAY_MS + 10);
        }
    }

    fn open_drives(&mut self, panel: usize) {
        self.active = panel;
        let drives = win::drives();
        let cur = self.panels[panel].dir.as_os_str().encode_wide_first();
        let sel = drives.iter().position(|d| Some(fsx::fold(d.root[0])) == cur.map(fsx::fold)).unwrap_or(0);
        self.mode = Mode::Drives(DrivePopup { panel, drives, sel });
    }

    fn choose_drive(&mut self, idx: usize) {
        if let Mode::Drives(d) = std::mem::replace(&mut self.mode, Mode::Normal) {
            if let Some(drive) = d.drives.get(idx) {
                let root = String::from_utf16_lossy(&drive.root[..3]);
                // Same drive: stay in the current directory.
                if !self.panels[d.panel].dir.starts_with(&root) {
                    self.nav(d.panel, PathBuf::from(root), None);
                }
            }
        }
    }

    fn start_path_edit(&mut self, panel: usize) {
        self.active = panel;
        let text = wide(&self.panels[panel].dir.display().to_string());
        self.mode = Mode::PathEdit { panel, input: TextInput::new(text) };
    }

    fn commit_path_edit(&mut self) {
        if let Mode::PathEdit { panel, input } = std::mem::replace(&mut self.mode, Mode::Normal) {
            if let Some(p) = parse_path(&input.text, &self.panels[panel].dir) {
                self.nav(panel, p, None);
            }
        }
    }

    fn open_dialog(&mut self, kind: DialogKind) {
        if self.job.is_some() && !matches!(kind, DialogKind::Select { .. }) {
            self.notify("Es läuft bereits ein Vorgang".into(), Tone::Error);
            return;
        }
        let a = self.active;
        let p = &self.panels[a];
        let (sources, message, input) = match kind {
            DialogKind::Copy | DialogKind::Move => {
                let sources = p.targets();
                let mut dest = self.panels[a ^ 1].dir.display().to_string();
                if !dest.ends_with('\\') {
                    dest.push('\\');
                }
                let verb = if kind == DialogKind::Copy { "kopieren" } else { "verschieben" };
                let msg = format!("{} {verb} nach:", describe(&sources));
                (sources, msg, Some(TextInput::new(wide(&dest))))
            }
            DialogKind::Delete { recycle } => {
                let sources = p.targets();
                let msg = if recycle {
                    format!("{} in den Papierkorb verschieben?", describe(&sources))
                } else {
                    format!("{} endgültig löschen? Das lässt sich nicht rückgängig machen.", describe(&sources))
                };
                (sources, msg, None)
            }
            DialogKind::Rename => {
                let Some(e) = p.current().filter(|e| e.kind != Kind::Parent) else { return };
                let name = e.name.to_vec();
                // Caret before the extension, like Explorer.
                let caret = match name.iter().rposition(|&c| c == b'.' as u16) {
                    Some(dot) if dot > 0 && !e.is_dir() => dot,
                    _ => name.len(),
                };
                let msg = format!("„{}“ umbenennen in:", String::from_utf16_lossy(&name));
                (vec![p.dir.join(e.os_name())], msg, Some(TextInput { text: name, caret, all: false }))
            }
            DialogKind::MkDir => {
                let msg = format!("Neuer Ordner in {}:", p.dir.display());
                (Vec::new(), msg, Some(TextInput::new(Vec::new())))
            }
            DialogKind::Select { .. } => {
                let msg = "Maske, z. B. *.bmp;*.png · Ordner: \\ am Ende · | schließt aus".to_string();
                (Vec::new(), msg, Some(TextInput::selected(wide(&self.mask))))
            }
        };
        if sources.is_empty() && !matches!(kind, DialogKind::MkDir | DialogKind::Select { .. }) {
            return;
        }
        self.panels[a].search.clear();
        self.mode = Mode::Dialog(Dialog { kind, panel: a, sources, message, input });
    }

    fn commit_dialog(&mut self) {
        let Mode::Dialog(d) = std::mem::replace(&mut self.mode, Mode::Normal) else { return };
        let dir = self.panels[d.panel].dir.clone();
        let input = d.input.as_ref().map(|i| i.string().trim().to_string()).unwrap_or_default();
        if let DialogKind::Select { add } = d.kind {
            self.apply_mask(d.panel, input, add);
            return;
        }
        let job = match d.kind {
            DialogKind::Select { .. } => return,
            DialogKind::Copy | DialogKind::Move => {
                let Some(dest) = parse_path(&wide(&input), &dir) else { return };
                if d.kind == DialogKind::Copy {
                    Job::Copy { sources: d.sources, dest }
                } else {
                    Job::Move { sources: d.sources, dest }
                }
            }
            DialogKind::Delete { recycle } => Job::Delete { sources: d.sources, recycle },
            DialogKind::Rename => {
                let from = d.sources[0].clone();
                if from.file_name().is_some_and(|n| n == input.as_str()) {
                    return;
                }
                if !valid_name(&input, false) {
                    self.notify(format!("Ungültiger Name: „{input}“"), Tone::Error);
                    return;
                }
                Job::Rename { from, to: input.into() }
            }
            DialogKind::MkDir => {
                if !valid_name(&input, true) {
                    self.notify(format!("Ungültiger Name: „{input}“"), Tone::Error);
                    return;
                }
                Job::MkDir { path: dir.join(&input) }
            }
        };
        self.start_job(job, d.kind.title(), d.panel);
    }

    fn apply_mask(&mut self, panel: usize, input: String, add: bool) {
        let mask = Mask::parse(&input);
        let n = self.panels[panel].mark_where(add, |e| mask.matches(&e.name, e.is_dir()));
        if n == 0 {
            self.notify(format!("Keine Treffer für „{input}“"), Tone::Info);
        }
        if !input.is_empty() {
            self.mask = input;
        }
    }

    // ---------- Input ----------

    /// `true` = handled.
    pub fn on_key(&mut self, vk: VIRTUAL_KEY) -> bool {
        self.eat_char = false;
        let ctrl = win::key_down(VK_CONTROL);
        let shift = win::key_down(VK_SHIFT);
        let hwnd = self.hwnd;
        let handled = match &mut self.mode {
            Mode::Drives(d) => {
                match vk {
                    VK_UP => d.sel = d.sel.saturating_sub(1),
                    VK_DOWN => d.sel = (d.sel + 1).min(d.drives.len().saturating_sub(1)),
                    VK_RETURN => {
                        let s = d.sel;
                        self.choose_drive(s);
                    }
                    VK_ESCAPE => self.mode = Mode::Normal,
                    _ => return false,
                }
                true
            }
            Mode::PathEdit { input, .. } => match vk {
                VK_RETURN => {
                    self.commit_path_edit();
                    true
                }
                VK_ESCAPE => {
                    self.mode = Mode::Normal;
                    true
                }
                _ => input.key(vk, ctrl, hwnd),
            },
            Mode::Dialog(d) => match vk {
                VK_RETURN => {
                    self.commit_dialog();
                    true
                }
                VK_ESCAPE => {
                    self.mode = Mode::Normal;
                    true
                }
                _ => d.input.as_mut().is_some_and(|i| i.key(vk, ctrl, hwnd)),
            },
            Mode::Conflict(_) => match vk {
                VK_RETURN => {
                    self.answer_conflict(Answer::Overwrite);
                    true
                }
                VK_ESCAPE => {
                    self.answer_conflict(Answer::Cancel);
                    true
                }
                _ => false,
            },
            Mode::Normal => self.on_key_normal(vk, ctrl, shift),
        };
        if handled {
            self.invalidate();
        }
        handled
    }

    fn on_key_normal(&mut self, vk: VIRTUAL_KEY, ctrl: bool, shift: bool) -> bool {
        let a = self.active;
        let p = &mut self.panels[a];
        // Scrolled away with the mouse? Any key brings the cursor back into view.
        if !matches!(vk, VK_SHIFT | VK_CONTROL | VK_MENU | VK_LWIN | VK_RWIN) {
            let top = p.top;
            p.ensure_visible();
            if p.top != top {
                win::invalidate(self.hwnd);
            }
        }
        match vk {
            VK_TAB => {
                p.search.clear();
                self.active ^= 1;
            }
            VK_UP | VK_DOWN if shift => {
                p.search.clear();
                p.toggle_mark();
                p.move_cursor(if vk == VK_UP { -1 } else { 1 });
            }
            VK_UP => {
                p.search.clear();
                p.move_cursor(-1);
            }
            VK_DOWN => {
                p.search.clear();
                p.move_cursor(1);
            }
            VK_PRIOR if ctrl => p.go_up(a, &self.sink),
            VK_PRIOR => {
                p.search.clear();
                let d = -p.page();
                p.move_cursor(d);
            }
            VK_NEXT => {
                p.search.clear();
                let d = p.page();
                p.move_cursor(d);
            }
            VK_HOME => {
                p.search.clear();
                p.set_cursor(0);
            }
            VK_END => {
                p.search.clear();
                p.set_cursor(usize::MAX);
            }
            VK_RETURN => {
                p.search.clear();
                self.open_current();
            }
            VK_INSERT => {
                p.search.clear();
                p.toggle_mark();
                p.move_cursor(1);
            }
            VK_A if ctrl => p.mark_all(),
            // Numpad, like Total Commander. The main row's + and - stay quick search characters.
            VK_ADD if ctrl => {
                p.mark_where(true, |_| true);
            }
            VK_SUBTRACT if ctrl => p.clear_marks(),
            VK_ADD | VK_SUBTRACT => {
                self.eat_char = true;
                self.open_dialog(DialogKind::Select { add: vk == VK_ADD });
            }
            VK_MULTIPLY => {
                self.eat_char = true;
                p.invert_file_marks();
            }
            VK_BACK if !p.search.is_empty() => p.search_pop(),
            VK_BACK => p.go_up(a, &self.sink),
            VK_ESCAPE => {
                if !p.search.is_empty() {
                    p.search.clear();
                } else if let Some(job) = &self.job {
                    job.handle.cancel();
                }
            }
            VK_F2 => p.reload(a, &self.sink),
            VK_R if ctrl => p.reload(a, &self.sink),
            VK_L if ctrl => self.start_path_edit(a),
            VK_F5 => self.open_dialog(DialogKind::Copy),
            VK_F6 if shift => self.open_dialog(DialogKind::Rename),
            VK_F6 => self.open_dialog(DialogKind::Move),
            VK_F7 => self.open_dialog(DialogKind::MkDir),
            VK_F8 | VK_DELETE => self.open_dialog(DialogKind::Delete { recycle: !shift }),
            _ => return false,
        }
        self.arm_timer();
        true
    }

    pub fn on_syskey(&mut self, vk: VIRTUAL_KEY) -> bool {
        if !matches!(self.mode, Mode::Normal | Mode::Drives(_)) {
            return false;
        }
        let panel = match vk {
            VK_F1 => 0,
            VK_F2 => 1,
            _ => return false,
        };
        self.open_drives(panel);
        self.invalidate();
        true
    }

    pub fn on_char(&mut self, c: u16) {
        if std::mem::take(&mut self.eat_char) {
            return;
        }
        if c < 0x20 || c == 0x7f {
            return;
        }
        match &mut self.mode {
            Mode::Drives(d) => {
                let hit = d.drives.iter().position(|dr| fsx::fold(dr.root[0]) == fsx::fold(c));
                if let Some(i) = hit {
                    self.choose_drive(i);
                }
            }
            Mode::PathEdit { input, .. } => input.insert(c),
            Mode::Dialog(d) => {
                if let Some(i) = &mut d.input {
                    i.insert(c);
                }
            }
            Mode::Conflict(_) => match char::from_u32(fsx::fold(c)) {
                Some('s') => self.answer_conflict(Answer::Skip),
                Some('a') => self.answer_conflict(Answer::OverwriteAll),
                Some('k') => self.answer_conflict(Answer::SkipAll),
                _ => {}
            },
            Mode::Normal => {
                let a = self.active;
                let p = &mut self.panels[a];
                if c == b'\\' as u16 {
                    p.go_root(a, &self.sink);
                    self.arm_timer();
                } else if c == b' ' as u16 && p.search.is_empty() {
                    p.toggle_mark();
                    p.move_cursor(1);
                } else {
                    p.search_push(c);
                }
            }
        }
        self.invalidate();
    }

    /// Mouse button pressed. Returns `true` if a drag started and the mouse should be captured.
    pub fn on_button_down(&mut self, x: f32, y: f32, right: bool, double: bool) -> bool {
        if self.drag.is_some() || matches!(self.mode, Mode::Dialog(_) | Mode::Conflict(_)) {
            return false;
        }
        let hit = self.hit(x, y);
        if let Mode::Drives(_) = self.mode {
            match hit {
                Hit::Popup(i) if !right => self.choose_drive(i),
                _ => self.mode = Mode::Normal,
            }
            self.invalidate();
            return false;
        }
        if let Mode::PathEdit { panel, .. } = &self.mode {
            if !right && matches!(hit, Hit::Path(p) if p == *panel) {
                return false;
            }
            self.mode = Mode::Normal;
        }
        let mut capture = false;
        match hit {
            Hit::Row(p, row) if right => {
                // Toggle the row; dragging gives every passed row the same state.
                self.active = p;
                let panel = &mut self.panels[p];
                panel.search.clear();
                panel.set_cursor(row);
                let marked = !panel.entries[row].marked;
                panel.set_mark(row, marked);
                self.drag = Some(Drag::Mark { panel: p, marked, last: row, y });
                capture = true;
            }
            Hit::Row(p, row) => {
                self.active = p;
                self.panels[p].search.clear();
                self.panels[p].set_cursor(row);
                if double {
                    self.open_current();
                }
            }
            Hit::Scrollbar(p) if !right => capture = self.press_scrollbar(p, y),
            Hit::Drive(p) if !right => self.open_drives(p),
            Hit::Path(p) if !right => self.start_path_edit(p),
            Hit::Drive(p) | Hit::Path(p) | Hit::Scrollbar(p) | Hit::Panel(p) => self.active = p,
            Hit::Popup(_) | Hit::Nothing => {}
        }
        self.invalidate();
        capture
    }

    /// Mouse button released. Returns `true` if a drag ended (release the capture).
    pub fn on_button_up(&mut self, x: f32, y: f32, right: bool) -> bool {
        match self.drag {
            Some(Drag::Mark { .. }) if !right => return false,
            Some(Drag::Thumb { .. } | Drag::Page { .. }) if right => return false,
            None => return false,
            _ => {}
        }
        self.cancel_drag();
        self.on_mouse_move(x, y);
        true
    }

    /// Ends a drag, e.g. when another window takes the mouse capture.
    pub fn cancel_drag(&mut self) {
        if self.drag.take().is_some() {
            self.invalidate();
        }
        if self.drag_timer {
            win::kill_timer(self.hwnd, TIMER_DRAG);
            self.drag_timer = false;
        }
    }

    pub fn on_mouse_move(&mut self, x: f32, y: f32) {
        match &mut self.drag {
            Some(Drag::Thumb { panel, grab }) => {
                let (p, ty) = (*panel, y - *grab);
                self.drag_thumb(p, ty);
            }
            Some(Drag::Page { y: py, .. }) => *py = y,
            Some(Drag::Mark { y: my, .. }) => {
                *my = y;
                self.extend_mark();
                // Outside the list: the timer keeps scrolling and marking.
                let (_, h) = self.gfx.size_dips(self.hwnd);
                let (top, lh) = Self::list_area(h);
                if y < top || y >= top + lh {
                    self.arm_drag_timer(DRAG_REPEAT_MS);
                }
            }
            None => {
                let hover = match self.hit(x, y) {
                    Hit::Scrollbar(p) => Some(p),
                    _ => None,
                };
                if hover != self.hover_bar {
                    self.hover_bar = hover;
                    self.invalidate();
                }
            }
        }
    }

    pub fn on_mouse_leave(&mut self) {
        if self.hover_bar.take().is_some() {
            self.invalidate();
        }
    }

    fn arm_drag_timer(&mut self, ms: u32) {
        if !self.drag_timer {
            win::set_timer(self.hwnd, TIMER_DRAG, ms);
            self.drag_timer = true;
        }
    }

    fn drag_tick(&mut self) {
        match self.drag {
            Some(Drag::Page { panel, y }) => {
                self.page_towards(panel, y);
                self.arm_drag_timer(DRAG_REPEAT_MS);
            }
            Some(Drag::Mark { panel, y, .. }) => {
                let (_, h) = self.gfx.size_dips(self.hwnd);
                let (top, lh) = Self::list_area(h);
                if y < top || y >= top + lh {
                    // Farther outside the list = faster.
                    let dist = if y < top { top - y } else { y - (top + lh) };
                    let rows = (1.0 + dist / ROW_H).min(10.0) as isize;
                    self.panels[panel].scroll(if y < top { -rows } else { rows });
                    self.extend_mark();
                    self.arm_drag_timer(DRAG_REPEAT_MS);
                }
            }
            _ => {}
        }
    }

    /// Left click into the scrollbar zone: grab the thumb or start paging.
    fn press_scrollbar(&mut self, p: usize, y: f32) -> bool {
        let (w, h) = self.gfx.size_dips(self.hwnd);
        let Some(sb) = self.scrollbar(p, w, h) else { return false };
        if y >= sb.thumb_y && y < sb.thumb_y + sb.thumb_h {
            self.drag = Some(Drag::Thumb { panel: p, grab: y - sb.thumb_y });
        } else {
            self.drag = Some(Drag::Page { panel: p, y });
            self.page_towards(p, y);
            self.arm_drag_timer(win::repeat_delay_ms());
        }
        true
    }

    /// Scroll one page towards `y`; stops once the thumb is under the mouse.
    fn page_towards(&mut self, p: usize, y: f32) {
        let (w, h) = self.gfx.size_dips(self.hwnd);
        let Some(sb) = self.scrollbar(p, w, h) else { return };
        let page = self.panels[p].page();
        if y < sb.thumb_y {
            self.panels[p].scroll(-page);
        } else if y >= sb.thumb_y + sb.thumb_h {
            self.panels[p].scroll(page);
        } else {
            return;
        }
        self.invalidate();
    }

    /// Move the thumb so its top is at `thumb_y`.
    fn drag_thumb(&mut self, p: usize, thumb_y: f32) {
        let (w, h) = self.gfx.size_dips(self.hwnd);
        let Some(sb) = self.scrollbar(p, w, h) else { return };
        let (top, lh) = Self::list_area(h);
        let range = lh - sb.thumb_h;
        let frac = if range > 0.0 { ((thumb_y - top) / range).clamp(0.0, 1.0) } else { 0.0 };
        let panel = &mut self.panels[p];
        let new_top = (frac * panel.max_top() as f32).round() as usize;
        if new_top != panel.top {
            panel.scroll_to(new_top);
            self.invalidate();
        }
    }

    /// Right-drag: mark every row from the last one up to the row under the mouse
    /// (clamped to the visible rows) and put the cursor there.
    fn extend_mark(&mut self) {
        let Some(Drag::Mark { panel, marked, last, y }) = self.drag else { return };
        let (_, h) = self.gfx.size_dips(self.hwnd);
        let (top, _) = Self::list_area(h);
        let p = &mut self.panels[panel];
        let len = p.entries.len();
        if len == 0 {
            return;
        }
        let first_vis = p.top;
        let last_vis = (p.top + p.rows_visible).min(len) - 1;
        let row = (p.top as f32 + ((y - top) / ROW_H).floor()).clamp(first_vis as f32, last_vis as f32) as usize;
        let last = last.min(len - 1);
        for i in row.min(last)..=row.max(last) {
            p.set_mark(i, marked);
        }
        p.cursor = row;
        self.drag = Some(Drag::Mark { panel, marked, last: row, y });
        self.invalidate();
    }

    pub fn on_wheel(&mut self, x: f32, delta: i16) {
        let (w, _) = self.gfx.size_dips(self.hwnd);
        let p = if x < self.panel_x(1, w).0 { 0 } else { 1 };
        self.panels[p].scroll(-(delta as isize) * 3 / 120);
        self.invalidate();
    }

    // ---------- Drawing ----------

    pub fn paint(&mut self) {
        if !self.painted {
            self.timing.mark("WM_PAINT");
        }
        if self.gfx.ensure_target(self.hwnd).is_err() {
            return;
        }
        if !self.painted {
            self.timing.mark("render target ready");
        }
        self.layout();
        if self.hint_widths.is_none() {
            let f = &self.gfx.fonts;
            self.hint_widths = Some(
                HINTS
                    .iter()
                    .map(|(k, l)| (self.gfx.measure(&wide(k), &f.small_center), self.gfx.measure(&wide(l), &f.small)))
                    .collect(),
            );
        }
        let (w, h) = self.gfx.size_dips(self.hwnd);
        let input = match &self.mode {
            Mode::PathEdit { input, .. } => Some(input),
            Mode::Dialog(d) => d.input.as_ref(),
            _ => None,
        };
        let caret_x = input.map(|i| self.gfx.caret_x(&i.text[..i.caret]));
        let Some(c) = self.gfx.begin(self.theme.panel_bg) else { return };
        for i in 0..2 {
            self.draw_panel(&c, i, w, h, caret_x);
        }
        c.fill(rect(self.panel_x(1, w).0 - 1.0, 0.0, 1.0, h - KEYBAR_H), self.theme.divider);
        self.draw_keybar(&c, w, h);
        self.draw_job_or_notice(&c, w, h);
        match &self.mode {
            Mode::Drives(d) => self.draw_drives(&c, d, w),
            Mode::Dialog(d) => self.draw_dialog(&c, d, w, h, caret_x),
            Mode::Conflict(path) => self.draw_conflict(&c, path, w, h),
            _ => {}
        }
        drop(c);
        self.gfx.end();

        if !self.painted {
            self.painted = true;
            self.timing.mark("first frame");
        }
        if !self.painted_content && self.panels.iter().all(|p| !p.entries.is_empty()) {
            self.painted_content = true;
            self.timing.mark("first frame with content");
            if self.timing.bench {
                self.bench_frames();
                SendHwnd::new(self.hwnd).post(WM_CLOSE);
            }
        }
    }

    /// Bench: steady-state frame time (cursor moves, everything is redrawn).
    fn bench_frames(&mut self) {
        let n = 60;
        let start = Instant::now();
        for i in 0..n {
            self.panels[0].move_cursor(if i % 20 < 10 { 1 } else { -1 });
            let (w, h) = self.gfx.size_dips(self.hwnd);
            let Some(c) = self.gfx.begin(self.theme.panel_bg) else { return };
            for p in 0..2 {
                self.draw_panel(&c, p, w, h, None);
            }
            self.draw_keybar(&c, w, h);
            drop(c);
            self.gfx.end();
        }
        let (w, h) = self.gfx.size_dips(self.hwnd);
        let ms = start.elapsed().as_secs_f64() * 1000.0 / n as f64;
        self.timing.note(format!("  frame time: {ms:.2} ms (avg over {n} frames, {w:.0}×{h:.0} DIP, {} dpi)", self.gfx.dpi));
    }

    /// Text input box with caret; scrolls horizontally to keep the caret visible.
    fn draw_input(&self, c: &Canvas, r: Rect, input: &TextInput, caret_x: f32) {
        let t = self.theme;
        c.fill_round(r, 5.0, t.edit_bg);
        c.stroke_round(r, 5.0, 1.0, t.accent);
        let inner = rect(r.left + 6.0, r.top, r.right - r.left - 12.0, r.bottom - r.top);
        let shift = (caret_x - (inner.right - inner.left) + 4.0).max(0.0);
        c.clip(inner);
        if input.all {
            let tw = self.gfx.measure(&input.text, &c.fonts.name);
            let sel = rect(inner.left - shift - 1.0, inner.top + 5.0, tw + 2.0, inner.bottom - inner.top - 10.0);
            c.fill_round(sel, 2.0, t.cursor_active);
        }
        c.text(&input.text, &c.fonts.name, rect(inner.left - shift, inner.top, 10000.0, inner.bottom - inner.top), t.text);
        let caret = inner.left - shift + caret_x;
        c.fill(rect(caret, inner.top + 5.0, 1.5, inner.bottom - inner.top - 10.0), t.accent);
        c.unclip();
    }

    /// Row of key chips with labels; returns the x after the last one.
    fn draw_chips(&self, c: &Canvas, mut x: f32, y: f32, h: f32, chips: &[(&str, &str)]) -> f32 {
        let t = self.theme;
        for (key, label) in chips {
            let kw = self.gfx.measure(&wide(key), &c.fonts.small_center) + 12.0;
            let lw = self.gfx.measure(&wide(label), &c.fonts.small) + 2.0;
            let chip = rect(x, y + (h - 18.0) / 2.0, kw, 18.0);
            c.fill_round(chip, 4.0, t.popup_bg);
            c.text_str(key, &c.fonts.small_center, chip, t.accent);
            x += kw + 6.0;
            c.text_str(label, &c.fonts.small, rect(x, y, lw, h), t.text_dim);
            x += lw + 16.0;
        }
        x
    }

    fn draw_card(&self, c: &Canvas, r: Rect) {
        let t = self.theme;
        let shadow = rect(r.left + 2.0, r.top + 5.0, r.right - r.left, r.bottom - r.top);
        c.fill_round(shadow, 10.0, crate::theme::rgba(0x000000, 0.4));
        c.fill_round(r, 10.0, t.popup_bg);
        c.stroke_round(r, 10.0, 1.0, t.popup_border);
    }

    fn draw_dialog(&self, c: &Canvas, d: &Dialog, w: f32, h: f32, caret_x: Option<f32>) {
        let t = self.theme;
        c.fill(rect(0.0, 0.0, w, h), crate::theme::rgba(0x000000, 0.35));
        let dw = (w - 48.0).clamp(200.0, 600.0);
        let input_h = if d.input.is_some() { 44.0 } else { 0.0 };
        let dh = 20.0 + 28.0 + 26.0 + input_h + 44.0;
        let r = rect(((w - dw) / 2.0).floor(), ((h - dh) / 2.5).floor(), dw, dh);
        self.draw_card(c, r);
        let x = r.left + 20.0;
        let iw = dw - 40.0;
        let mut y = r.top + 16.0;
        let danger = d.kind == DialogKind::Delete { recycle: false };
        c.text_str(d.kind.title(), &c.fonts.path, rect(x, y, iw, 28.0), if danger { t.error } else { t.text });
        y += 28.0;
        c.text_str(&d.message, &c.fonts.name, rect(x, y, iw, 26.0), t.text_dim);
        y += 30.0;
        if let (Some(input), Some(cx)) = (&d.input, caret_x) {
            self.draw_input(c, rect(x, y, iw, 32.0), input, cx);
            y += input_h;
        }
        let ok = if matches!(d.kind, DialogKind::Delete { .. }) { "Löschen" } else { "OK" };
        self.draw_chips(c, x, y, 28.0, &[("Enter", ok), ("Esc", "Abbrechen")]);
    }

    fn draw_conflict(&self, c: &Canvas, path: &Path, w: f32, h: f32) {
        let t = self.theme;
        c.fill(rect(0.0, 0.0, w, h), crate::theme::rgba(0x000000, 0.35));
        let dw = (w - 48.0).clamp(200.0, 720.0);
        let dh = 20.0 + 28.0 + 26.0 + 44.0;
        let r = rect(((w - dw) / 2.0).floor(), ((h - dh) / 2.5).floor(), dw, dh);
        self.draw_card(c, r);
        let x = r.left + 20.0;
        let iw = dw - 40.0;
        c.text_str("Ziel existiert bereits", &c.fonts.path, rect(x, r.top + 16.0, iw, 28.0), t.marked);
        c.text_str(&path.display().to_string(), &c.fonts.name, rect(x, r.top + 44.0, iw, 26.0), t.text_dim);
        let chips = [
            ("Enter", "Überschreiben"),
            ("S", "Überspringen"),
            ("A", "Alle überschreiben"),
            ("K", "Keine überschreiben"),
            ("Esc", "Abbrechen"),
        ];
        self.draw_chips(c, x, r.top + 74.0, 28.0, &chips);
    }

    fn draw_job_or_notice(&self, c: &Canvas, w: f32, h: f32) {
        let t = self.theme;
        let cw = CARD_W.min(w - 32.0);
        let x = w - cw - 16.0;
        let bottom = h - KEYBAR_H - STATUS_H - 12.0;
        if let Some(job) = &self.job {
            if job.started.elapsed().as_millis() < JOB_CARD_DELAY_MS as u128 {
                return;
            }
            let p = &job.progress;
            let r = rect(x, bottom - 92.0, cw, 92.0);
            self.draw_card(c, r);
            let ix = r.left + 16.0;
            let iw = cw - 32.0;
            let frac = if p.bytes_total > 0 {
                p.bytes_done as f64 / p.bytes_total as f64
            } else if p.files_total > 0 {
                p.files_done as f64 / p.files_total as f64
            } else {
                0.0
            }
            .clamp(0.0, 1.0) as f32;
            c.text_str(&format!("{} …", job.title), &c.fonts.path, rect(ix, r.top + 8.0, iw, 24.0), t.text);
            c.text_str(&format!("{:.0} %", frac * 100.0), &c.fonts.small_right, rect(ix, r.top + 8.0, iw, 24.0), t.accent);
            let current = p.current.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            c.text_str(&current, &c.fonts.small, rect(ix, r.top + 32.0, iw, 18.0), t.text_dim);
            let bar = rect(ix, r.top + 54.0, iw, 6.0);
            c.fill_round(bar, 3.0, t.edit_bg);
            if frac > 0.0 {
                c.fill_round(rect(ix, bar.top, (iw * frac).max(6.0), 6.0), 3.0, t.accent);
            }
            let mut stats = format!("{} / {} Dateien", p.files_done, p.files_total);
            if p.bytes_total > 0 {
                stats += &format!("  ·  {} / {}", fmt_size(p.bytes_done), fmt_size(p.bytes_total));
            }
            c.text_str(&stats, &c.fonts.small, rect(ix, r.top + 64.0, iw, 22.0), t.text_dim);
            c.text_str("Esc bricht ab", &c.fonts.small_right, rect(ix, r.top + 64.0, iw, 22.0), t.text_hidden);
        } else if let Some(n) = &self.notice {
            let r = rect(x, bottom - 40.0, cw, 40.0);
            self.draw_card(c, r);
            let (glyph, col) = match n.tone {
                Tone::Success => (0xE73E, t.success),
                Tone::Info => (0xE946, t.text_dim),
                Tone::Error => (0xE783, t.error),
            };
            c.text(&[glyph], &c.fonts.icon, rect(r.left + 10.0, r.top, 24.0, 40.0), col);
            c.text_str(&n.text, &c.fonts.name, rect(r.left + 40.0, r.top, cw - 52.0, 40.0), t.text);
        }
    }

    fn draw_panel(&self, c: &Canvas, i: usize, w: f32, h: f32, caret_x: Option<f32>) {
        let t = self.theme;
        let p = &self.panels[i];
        let active = i == self.active;
        let (x, pw) = self.panel_x(i, w);
        let f = c.fonts;

        // Path bar
        c.fill(rect(x, 0.0, pw, PATH_H), t.bar_bg);
        if active {
            c.fill(rect(x, PATH_H - 2.0, pw, 2.0), t.accent);
        }
        let shown = p.shown_path();
        let drive: String = shown.display().to_string().chars().take(2).collect();
        let chip = rect(x + PAD, 8.0, CHIP_W - 6.0, PATH_H - 18.0);
        c.fill_round(chip, 5.0, t.popup_bg);
        c.text_str(&drive, &f.small_center, chip, if active { t.text } else { t.text_dim });

        let path_x = x + PAD + CHIP_W + 2.0;
        let loading = p.loading_for_ms().is_some_and(|ms| ms >= LOADING_DELAY_MS as u128);
        let right_w = if loading { 70.0 } else { 0.0 };
        match (&self.mode, caret_x) {
            (Mode::PathEdit { panel, input }, Some(cx)) if *panel == i => {
                let r = rect(path_x - 6.0, 6.0, x + pw - PAD - path_x + 6.0, PATH_H - 14.0);
                self.draw_input(c, r, input, cx);
            }
            _ => {
                let r = rect(path_x, 0.0, x + pw - PAD - path_x - right_w, PATH_H - 2.0);
                let s = shown.display().to_string();
                let s = s.get(2..).filter(|_| drive.ends_with(':')).unwrap_or(&s);
                c.text_str(s, &f.path, r, if active { t.text } else { t.text_dim });
                if loading {
                    c.text_str("lädt …", &f.small_right, rect(x + pw - PAD - right_w, 0.0, right_w, PATH_H - 2.0), t.accent);
                }
            }
        }

        // Column headers
        let date_x = x + pw - PAD - DATE_W;
        let size_x = date_x - 14.0 - SIZE_W;
        let name_x = x + PAD + ICON_W + 6.0;
        let hy = PATH_H;
        c.text_str("Name", &f.small, rect(name_x, hy, size_x - name_x, COLS_H), t.text_dim);
        c.text_str("Größe", &f.small_right, rect(size_x, hy, SIZE_W, COLS_H), t.text_dim);
        c.text_str("Geändert", &f.small, rect(date_x, hy, DATE_W, COLS_H), t.text_dim);
        c.fill(rect(x, hy + COLS_H - 1.0, pw, 1.0), t.divider);

        // Rows – visible ones only
        let (top, lh) = Self::list_area(h);
        c.clip(rect(x, top, pw, lh));
        let end = (p.top + p.rows_visible + 1).min(p.entries.len());
        for (r, idx) in (p.top..end).enumerate() {
            let e = &p.entries[idx];
            let y = top + r as f32 * ROW_H;
            if idx == p.cursor {
                let col = if active { t.cursor_active } else { t.cursor_inactive };
                c.fill_round(rect(x + 4.0, y + 1.0, pw - 12.0, ROW_H - 2.0), 4.0, col);
            } else if idx % 2 == 1 {
                c.fill(rect(x, y, pw, ROW_H), t.row_alt);
            }
            let (glyph, mut icol) = icon(e.kind, t);
            let mut tcol = t.text;
            if e.hidden {
                icol.a = 0.45;
                tcol = t.text_hidden;
            }
            if e.marked {
                tcol = t.marked;
                c.fill_round(rect(x + 5.0, y + 5.0, 3.0, ROW_H - 10.0), 1.5, t.marked);
            }
            c.text(&[glyph], &f.icon, rect(x + PAD, y, ICON_W, ROW_H), icol);
            c.text(&e.name, &f.name, rect(name_x, y, size_x - name_x - 8.0, ROW_H), tcol);
            let dim = if e.marked { t.marked } else { t.text_dim };
            match e.kind {
                Kind::Parent => {}
                Kind::Dir => c.text_str("Ordner", &f.small_right, rect(size_x, y, SIZE_W, ROW_H), dim),
                _ => c.text_str(&fmt_size(e.size), &f.right, rect(size_x, y, SIZE_W, ROW_H), if e.hidden && !e.marked { tcol } else { dim }),
            }
            if e.kind != Kind::Parent {
                c.text_str(&fmt_date(e.mtime), &f.small, rect(date_x, y, DATE_W, ROW_H), dim);
            }
        }
        c.unclip();

        // Scrollbar: thin at rest, wider and brighter under the mouse or while dragged.
        if let Some(sb) = self.scrollbar(i, w, h) {
            let dragging = matches!(self.drag, Some(Drag::Thumb { panel, .. }) if panel == i);
            let paging = matches!(self.drag, Some(Drag::Page { panel, .. }) if panel == i);
            let hot = dragging || paging || (self.drag.is_none() && self.hover_bar == Some(i));
            let (bw, col) = if dragging {
                (8.0, t.scroll_thumb_drag)
            } else if hot {
                (8.0, t.scroll_thumb_hover)
            } else {
                (4.0, t.scroll_thumb)
            };
            if hot {
                c.fill_round(rect(x + pw - 13.0, top + 2.0, 12.0, lh - 4.0), 6.0, t.scroll_track);
            }
            c.fill_round(rect(x + pw - 3.0 - bw, sb.thumb_y + 2.0, bw, sb.thumb_h - 4.0), bw / 2.0, col);
        }

        // Status bar
        let sy = h - KEYBAR_H - STATUS_H;
        c.fill(rect(x, sy, pw, STATUS_H), t.bar_bg);
        c.fill(rect(x, sy, pw, 1.0), t.divider);
        let mut status_w = pw - 2.0 * PAD;
        if !p.search.is_empty() {
            let label: Vec<u16> = p.search.clone();
            let tw = self.gfx.measure(&label, &f.small).min(pw * 0.5) + 40.0;
            let r = rect(x + pw - PAD - tw, sy + 4.0, tw, STATUS_H - 8.0);
            c.fill_round(r, 5.0, t.cursor_active);
            c.text(&[0xE721], &f.icon, rect(r.left + 4.0, r.top, 20.0, r.bottom - r.top), t.text);
            c.text(&label, &f.small, rect(r.left + 26.0, r.top, tw - 30.0, r.bottom - r.top), t.text);
            status_w -= tw + 8.0;
        }
        let sr = rect(x + PAD, sy, status_w, STATUS_H);
        let (marked, marked_bytes) = p.marked();
        match &p.error {
            Some(err) => c.text_str(err, &f.small, sr, t.error),
            None if marked > 0 => {
                let s = format!("{marked} markiert  ·  {}", fmt_size(marked_bytes));
                c.text_str(&s, &f.small, sr, t.marked);
            }
            None => {
                let s = format!("{} Ordner  ·  {} Dateien  ·  {}", p.dirs, p.files, fmt_size(p.bytes));
                c.text_str(&s, &f.small, sr, t.text_dim);
            }
        }
    }

    fn draw_keybar(&self, c: &Canvas, w: f32, h: f32) {
        let t = self.theme;
        let y = h - KEYBAR_H;
        c.fill(rect(0.0, y, w, KEYBAR_H), t.bar_bg);
        c.fill(rect(0.0, y, w, 1.0), t.divider);
        let Some(widths) = &self.hint_widths else { return };
        let mut x = PAD;
        for ((key, label), (kw, lw)) in HINTS.iter().zip(widths) {
            let chip_w = kw + 12.0;
            if x + chip_w + lw + 6.0 > w {
                break;
            }
            let chip = rect(x, y + 6.0, chip_w, KEYBAR_H - 12.0);
            c.fill_round(chip, 4.0, t.popup_bg);
            c.text_str(key, &c.fonts.small_center, chip, t.accent);
            x += chip_w + 6.0;
            c.text_str(label, &c.fonts.small, rect(x, y, lw + 2.0, KEYBAR_H), t.text_dim);
            x += lw + 18.0;
        }
    }

    fn draw_drives(&self, c: &Canvas, d: &DrivePopup, w: f32) {
        let t = self.theme;
        let (px, _) = self.panel_x(d.panel, w);
        let r = self.popup_rect(px, d.drives.len());
        let shadow = rect(r.left + 2.0, r.top + 4.0, r.right - r.left, r.bottom - r.top);
        c.fill_round(shadow, 8.0, crate::theme::rgba(0x000000, 0.35));
        c.fill_round(r, 8.0, t.popup_bg);
        c.stroke_round(r, 8.0, 1.0, t.popup_border);
        for (i, drive) in d.drives.iter().enumerate() {
            let y = r.top + 4.0 + i as f32 * ROW_H;
            if i == d.sel {
                c.fill_round(rect(r.left + 4.0, y, r.right - r.left - 8.0, ROW_H), 4.0, t.cursor_active);
            }
            let (glyph, label) = drive_info(drive.kind);
            c.text(&[glyph], &c.fonts.icon, rect(r.left + 8.0, y, ICON_W, ROW_H), t.accent);
            c.text(&drive.root[..2], &c.fonts.path, rect(r.left + 38.0, y, 30.0, ROW_H), t.text);
            c.text_str(label, &c.fonts.small, rect(r.left + 72.0, y, r.right - r.left - 80.0, ROW_H), t.text_dim);
        }
    }
}

trait FirstWide {
    fn encode_wide_first(&self) -> Option<u16>;
}

impl FirstWide for std::ffi::OsStr {
    fn encode_wide_first(&self) -> Option<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_name("neu.txt", false));
        assert!(!valid_name("a\\b", false));
        assert!(valid_name("a\\b", true));
        assert!(!valid_name("", true));
        assert!(!valid_name("..", false));
        assert!(!valid_name("x?.txt", false));
    }
}
