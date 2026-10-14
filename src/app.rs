//! Anwendungszustand, Eingabe und Zeichnen.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::WM_CLOSE;

use crate::config::Config;
use crate::fsx::{self, Kind, Loaded};
use crate::gfx::{Canvas, Gfx, rect};
use crate::panel::{LoadSink, Panel};
use crate::theme::{Color, DARK, Theme};
use crate::timing::Timing;
use crate::win::{self, Drive, SendHwnd};

pub const LOADING_DELAY_MS: u32 = 150;
const TIMER_LOADING: usize = 1;

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

const HINTS: [(&str, &str); 7] = [
    ("Tab", "Seite"),
    ("Enter", "Öffnen"),
    ("⌫", "Hoch"),
    ("Alt+F1/F2", "Laufwerk"),
    ("Strg+L", "Pfad"),
    ("Strg+R", "Neu laden"),
    ("A–Z", "Suchen"),
];

struct LineEdit {
    panel: usize,
    text: Vec<u16>,
    caret: usize,
}

struct DrivePopup {
    panel: usize,
    drives: Vec<Drive>,
    sel: usize,
}

enum Mode {
    Normal,
    PathEdit(LineEdit),
    Drives(DrivePopup),
}

enum Hit {
    Drive(usize),
    Path(usize),
    Row(usize, usize),
    Panel(usize),
    Popup(usize),
    Nothing,
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

/// Eingabe aus der Pfadzeile in einen absoluten Pfad wandeln.
fn parse_path(input: &[u16], base: &std::path::Path) -> Option<PathBuf> {
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
        // Lade-Threads vor der Grafik-Initialisierung starten, damit beides parallel läuft.
        let mut panels = [Panel::default(), Panel::default()];
        let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
        let defaults = [Some(PathBuf::from(r"C:\")), home];
        for (i, panel) in panels.iter_mut().enumerate() {
            let p = cfg.paths[i].clone().or(defaults[i].clone()).unwrap_or(PathBuf::from(r"C:\"));
            panel.navigate(i, p, None, &sink);
        }
        let mut timing = timing;
        let gfx = Gfx::new().expect("Direct2D/DirectWrite nicht verfügbar");
        timing.mark("Direct2D/DirectWrite initialisiert");
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
            painted: false,
            painted_content: false,
        };
        app.arm_timer();
        app
    }

    pub fn config(&self, hwnd: HWND) -> Config {
        let mut cfg = Config { active: self.active, ..Default::default() };
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

    pub fn on_loaded(&mut self) {
        while let Ok(l) = self.rx.try_recv() {
            let info = match &l.result {
                Ok(li) => format!(
                    "  Ordner {} – {} Einträge in {:.2} ms",
                    li.dir.display(),
                    li.entries.len(),
                    li.elapsed.as_secs_f64() * 1000.0
                ),
                Err(e) => format!("  Fehler: {}", e.trim()),
            };
            let p = l.panel;
            if self.panels[p].apply(l.id, l.result) {
                self.timing.note(info);
                // Startpfad kaputt (z. B. Laufwerk weg)? Dann auf C:\ ausweichen.
                if self.panels[p].entries.is_empty() && self.panels[p].dir.as_os_str().is_empty() {
                    self.nav(p, PathBuf::from(r"C:\"), None);
                }
            }
        }
        self.invalidate();
    }

    pub fn on_timer(&mut self) {
        win::kill_timer(self.hwnd, TIMER_LOADING);
        self.invalidate();
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
            p.ensure_visible();
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
            let row = self.panels[p].top + ((y - top) / ROW_H) as usize;
            if row < self.panels[p].entries.len() {
                return Hit::Row(p, row);
            }
        }
        Hit::Panel(p)
    }

    fn popup_rect(&self, px: f32, n: usize) -> crate::gfx::Rect {
        rect(px + PAD - 4.0, PATH_H - 2.0, POPUP_W, n as f32 * ROW_H + 8.0)
    }

    // ---------- Aktionen ----------

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
                // Gleiches Laufwerk: im aktuellen Ordner bleiben.
                if !self.panels[d.panel].dir.starts_with(&root) {
                    self.nav(d.panel, PathBuf::from(root), None);
                }
            }
        }
    }

    fn start_path_edit(&mut self, panel: usize) {
        self.active = panel;
        let text: Vec<u16> = wide(&self.panels[panel].dir.display().to_string());
        let caret = text.len();
        self.mode = Mode::PathEdit(LineEdit { panel, text, caret });
    }

    fn commit_path_edit(&mut self) {
        if let Mode::PathEdit(e) = std::mem::replace(&mut self.mode, Mode::Normal) {
            if let Some(p) = parse_path(&e.text, &self.panels[e.panel].dir) {
                self.nav(e.panel, p, None);
            }
        }
    }

    // ---------- Eingabe ----------

    /// `true` = verarbeitet.
    pub fn on_key(&mut self, vk: VIRTUAL_KEY) -> bool {
        let ctrl = win::key_down(VK_CONTROL);
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
            Mode::PathEdit(e) => {
                match vk {
                    VK_LEFT => e.caret = e.caret.saturating_sub(1),
                    VK_RIGHT => e.caret = (e.caret + 1).min(e.text.len()),
                    VK_HOME => e.caret = 0,
                    VK_END => e.caret = e.text.len(),
                    VK_BACK if e.caret > 0 => {
                        e.caret -= 1;
                        e.text.remove(e.caret);
                    }
                    VK_DELETE if e.caret < e.text.len() => {
                        e.text.remove(e.caret);
                    }
                    VK_RETURN => self.commit_path_edit(),
                    VK_ESCAPE => self.mode = Mode::Normal,
                    VK_V if ctrl => {
                        if let Some(clip) = win::clipboard_text(self.hwnd) {
                            let clip: Vec<u16> = clip.into_iter().filter(|&c| c >= 0x20).collect();
                            let n = clip.len();
                            e.text.splice(e.caret..e.caret, clip);
                            e.caret += n;
                        }
                    }
                    VK_A if ctrl => {
                        e.text.clear();
                        e.caret = 0;
                    }
                    _ => return matches!(vk, VK_BACK | VK_DELETE),
                }
                true
            }
            Mode::Normal => self.on_key_normal(vk, ctrl),
        };
        if handled {
            self.invalidate();
        }
        handled
    }

    fn on_key_normal(&mut self, vk: VIRTUAL_KEY, ctrl: bool) -> bool {
        let a = self.active;
        let p = &mut self.panels[a];
        match vk {
            VK_TAB => {
                p.search.clear();
                self.active ^= 1;
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
            VK_BACK if !p.search.is_empty() => p.search_pop(),
            VK_BACK => p.go_up(a, &self.sink),
            VK_ESCAPE => p.search.clear(),
            VK_F2 => p.reload(a, &self.sink),
            VK_R if ctrl => p.reload(a, &self.sink),
            VK_L if ctrl => self.start_path_edit(a),
            _ => return false,
        }
        self.arm_timer();
        true
    }

    pub fn on_syskey(&mut self, vk: VIRTUAL_KEY) -> bool {
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
            Mode::PathEdit(e) => {
                e.text.insert(e.caret, c);
                e.caret += 1;
            }
            Mode::Normal => {
                let a = self.active;
                if c == b'\\' as u16 {
                    self.panels[a].go_root(a, &self.sink);
                    self.arm_timer();
                } else {
                    self.panels[a].search_push(c);
                }
            }
        }
        self.invalidate();
    }

    pub fn on_click(&mut self, x: f32, y: f32, double: bool) {
        let hit = self.hit(x, y);
        if let Mode::Drives(_) = self.mode {
            match hit {
                Hit::Popup(i) => self.choose_drive(i),
                _ => self.mode = Mode::Normal,
            }
            self.invalidate();
            return;
        }
        if let Mode::PathEdit(e) = &self.mode {
            if matches!(hit, Hit::Path(p) if p == e.panel) {
                return;
            }
            self.mode = Mode::Normal;
        }
        match hit {
            Hit::Drive(p) => self.open_drives(p),
            Hit::Path(p) => self.start_path_edit(p),
            Hit::Row(p, row) => {
                self.active = p;
                self.panels[p].search.clear();
                self.panels[p].set_cursor(row);
                if double {
                    self.open_current();
                }
            }
            Hit::Panel(p) => self.active = p,
            Hit::Popup(_) | Hit::Nothing => {}
        }
        self.invalidate();
    }

    pub fn on_wheel(&mut self, x: f32, delta: i16) {
        let (w, _) = self.gfx.size_dips(self.hwnd);
        let p = if x < self.panel_x(1, w).0 { 0 } else { 1 };
        self.panels[p].scroll(-(delta as isize) * 3 / 120);
        self.invalidate();
    }

    // ---------- Zeichnen ----------

    pub fn paint(&mut self) {
        if !self.painted {
            self.timing.mark("WM_PAINT");
        }
        if self.gfx.ensure_target(self.hwnd).is_err() {
            return;
        }
        if !self.painted {
            self.timing.mark("Render-Target bereit");
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
        let caret_x = match &self.mode {
            Mode::PathEdit(e) => Some(self.gfx.caret_x(&e.text[..e.caret])),
            _ => None,
        };
        let Some(c) = self.gfx.begin(self.theme.panel_bg) else { return };
        for i in 0..2 {
            self.draw_panel(&c, i, w, h, caret_x);
        }
        c.fill(rect(self.panel_x(1, w).0 - 1.0, 0.0, 1.0, h - KEYBAR_H), self.theme.divider);
        self.draw_keybar(&c, w, h);
        if let Mode::Drives(d) = &self.mode {
            self.draw_drives(&c, d, w);
        }
        drop(c);
        self.gfx.end();

        if !self.painted {
            self.painted = true;
            self.timing.mark("erster Frame");
        }
        if !self.painted_content && self.panels.iter().all(|p| !p.entries.is_empty()) {
            self.painted_content = true;
            self.timing.mark("erster Frame mit Inhalt");
            if self.timing.bench {
                self.bench_frames();
                SendHwnd::new(self.hwnd).post(WM_CLOSE);
            }
        }
    }

    /// Bench: Frame-Zeit im eingeschwungenen Zustand (Cursor wandert, alles neu gezeichnet).
    fn bench_frames(&mut self) {
        let n = 60;
        let start = std::time::Instant::now();
        for i in 0..n {
            self.panels[0].move_cursor(if i % 20 < 10 { 1 } else { -1 });
            self.painted_content = true;
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
        self.timing.note(format!("  Frame-Zeit: {ms:.2} ms (Ø über {n} Frames, {w:.0}×{h:.0} DIP, {} dpi)", self.gfx.dpi));
    }

    fn draw_panel(&self, c: &Canvas, i: usize, w: f32, h: f32, caret_x: Option<f32>) {
        let t = self.theme;
        let p = &self.panels[i];
        let active = i == self.active;
        let (x, pw) = self.panel_x(i, w);
        let f = c.fonts;

        // Pfadzeile
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
            (Mode::PathEdit(e), Some(cx)) if e.panel == i => {
                let r = rect(path_x - 6.0, 6.0, x + pw - PAD - path_x + 6.0, PATH_H - 14.0);
                c.fill_round(r, 5.0, t.edit_bg);
                c.stroke_round(r, 5.0, 1.0, t.accent);
                let inner = rect(r.left + 6.0, r.top, r.right - r.left - 12.0, r.bottom - r.top);
                let shift = (cx - (inner.right - inner.left) + 4.0).max(0.0);
                c.clip(inner);
                c.text(&e.text, &f.name, rect(inner.left - shift, inner.top, 10000.0, inner.bottom - inner.top), t.text);
                let caret = inner.left - shift + cx;
                c.fill(rect(caret, inner.top + 5.0, 1.5, inner.bottom - inner.top - 10.0), t.accent);
                c.unclip();
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

        // Spaltenköpfe
        let date_x = x + pw - PAD - DATE_W;
        let size_x = date_x - 14.0 - SIZE_W;
        let name_x = x + PAD + ICON_W + 6.0;
        let hy = PATH_H;
        c.text_str("Name", &f.small, rect(name_x, hy, size_x - name_x, COLS_H), t.text_dim);
        c.text_str("Größe", &f.small_right, rect(size_x, hy, SIZE_W, COLS_H), t.text_dim);
        c.text_str("Geändert", &f.small, rect(date_x, hy, DATE_W, COLS_H), t.text_dim);
        c.fill(rect(x, hy + COLS_H - 1.0, pw, 1.0), t.divider);

        // Zeilen – nur die sichtbaren
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
            c.text(&[glyph], &f.icon, rect(x + PAD, y, ICON_W, ROW_H), icol);
            c.text(&e.name, &f.name, rect(name_x, y, size_x - name_x - 8.0, ROW_H), tcol);
            match e.kind {
                Kind::Parent => {}
                Kind::Dir => c.text_str("Ordner", &f.small_right, rect(size_x, y, SIZE_W, ROW_H), t.text_dim),
                _ => c.text_str(&fmt_size(e.size), &f.right, rect(size_x, y, SIZE_W, ROW_H), if e.hidden { tcol } else { t.text_dim }),
            }
            if e.kind != Kind::Parent {
                c.text_str(&fmt_date(e.mtime), &f.small, rect(date_x, y, DATE_W, ROW_H), t.text_dim);
            }
        }
        c.unclip();

        // Scrollbalken (nur Anzeige)
        let n = p.entries.len();
        if n > p.rows_visible && lh > 0.0 {
            let th = (lh * p.rows_visible as f32 / n as f32).max(24.0);
            let ty = top + (lh - th) * p.top as f32 / (n - p.rows_visible) as f32;
            c.fill_round(rect(x + pw - 7.0, ty + 2.0, 4.0, th - 4.0), 2.0, t.scroll_thumb);
        }

        // Statuszeile
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
        match &p.error {
            Some(err) => c.text_str(err, &f.small, sr, t.error),
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
