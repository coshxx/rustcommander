//! Drawing one panel: path bar, column headers, file rows, scrollbar, status bar.

use crate::app::format::{format_bytes, format_date, format_size};
use crate::app::layout::{CHIP_W, COLS_H, Columns, DATE_W, ICON_W, KEYBAR_H, PAD, PATH_H, ROW_H, STATUS_H};
use crate::app::mouse::Drag;
use crate::app::{App, LOADING_DELAY_MS, Mode};
use crate::files::listing::{Entry, Kind};
use crate::files::sort::SortKey;
use crate::i18n::{self, tf, tn};
use crate::panel::Panel;
use crate::sys::gfx::{Canvas, rect};
use crate::theme::{Color, Theme};
use crate::ui::widgets::draw_input;

pub(crate) fn icon(kind: Kind, t: &Theme) -> (u16, Color) {
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

/// Which panel is drawn and where (DIPs).
struct Area<'a> {
    index: usize,
    panel: &'a Panel,
    active: bool,
    x: f32,
    width: f32,
}

impl App {
    pub(crate) fn draw_panel(&self, c: &Canvas, i: usize, w: f32, h: f32, caret_x: Option<f32>) {
        let (x, width) = self.panel_x(i, w);
        let a = Area { index: i, panel: &self.panels[i], active: i == self.active, x, width };
        let cols = self.columns(i, w);
        self.draw_path_bar(c, &a, caret_x);
        self.draw_column_headers(c, &a, &cols);
        self.draw_rows(c, &a, &cols, h, caret_x);
        self.draw_scrollbar(c, &a, w, h);
        self.draw_status_bar(c, &a, h);
    }

    /// Rounded (`1,2 GB`) or, with the option, exact (`1.288.490.188`).
    pub(crate) fn size_text(&self, bytes: u64) -> String {
        if self.options.exact_sizes { format_bytes(bytes) } else { format_size(bytes) }
    }

    /// Drive chip and path (or the path editor), plus "loading" on slow directories.
    fn draw_path_bar(&self, c: &Canvas, a: &Area, caret_x: Option<f32>) {
        let t = self.theme;
        let f = c.fonts;
        let (x, pw) = (a.x, a.width);
        c.fill(rect(x, 0.0, pw, PATH_H), t.bar_bg);
        if a.active {
            c.fill(rect(x, PATH_H - 2.0, pw, 2.0), t.accent);
        }
        let shown = a.panel.shown_path();
        let drive: String = shown.display().to_string().chars().take(2).collect();
        let chip = rect(x + PAD, 8.0, CHIP_W - 6.0, PATH_H - 18.0);
        c.fill_round(chip, 5.0, t.popup_bg);
        c.text_str(&drive, &f.small_center, chip, if a.active { t.text } else { t.text_dim });

        let path_x = x + PAD + CHIP_W + 2.0;
        let loading = a.panel.loading_for_ms().is_some_and(|ms| ms >= LOADING_DELAY_MS as u128);
        let right_w = if loading { 70.0 } else { 0.0 };
        match (&self.mode, caret_x) {
            (Mode::PathEdit { panel, input }, Some(cx)) if *panel == a.index => {
                let r = rect(path_x - 6.0, 6.0, x + pw - PAD - path_x + 6.0, PATH_H - 14.0);
                draw_input(c, &self.gfx, self.theme, r, input, Some(cx));
            }
            _ => {
                let r = rect(path_x, 0.0, x + pw - PAD - path_x - right_w, PATH_H - 2.0);
                let s = shown.display().to_string();
                let s = s.get(2..).filter(|_| drive.ends_with(':')).unwrap_or(&s);
                c.text_str(s, &f.path, r, if a.active { t.text } else { t.text_dim });
                if loading {
                    c.text_str(
                        i18n::t("panel.loading"),
                        &f.small_right,
                        rect(x + pw - PAD - right_w, 0.0, right_w, PATH_H - 2.0),
                        t.accent,
                    );
                }
            }
        }
    }

    /// Column headers; the sort column gets ▲/▼, the one under the mouse a highlight.
    fn draw_column_headers(&self, c: &Canvas, a: &Area, cols: &Columns) {
        let t = self.theme;
        let f = c.fonts;
        let y = PATH_H;
        let sort = a.panel.sort;
        let headers = [
            (SortKey::Name, i18n::t("panel.name"), cols.name_x, cols.size_x - cols.name_x - 8.0, false),
            (SortKey::Size, i18n::t("panel.size"), cols.size_x, cols.size_w, true),
            (SortKey::Date, i18n::t("panel.modified"), cols.date_x, DATE_W, false),
        ];
        for (key, label, x, w, right) in headers {
            if self.hover_header == Some((a.index, key)) {
                c.fill_round(rect(x - 6.0, y + 2.0, w + 12.0, COLS_H - 4.0), 4.0, t.cursor_inactive);
            }
            if key != sort.key {
                let font = if right { &f.small_right } else { &f.small };
                c.text_str(label, font, rect(x, y, w, COLS_H), t.text_dim);
                continue;
            }
            // Arrow after the label; for the right-aligned size column the label moves left.
            let arrow = if sort.desc { "▼" } else { "▲" };
            let lw = self.gfx.measure(&crate::app::format::utf16(label), &f.small) + 2.0;
            let label_x = if right { x + w - lw - 14.0 } else { x };
            c.text_str(label, &f.small, rect(label_x, y, lw, COLS_H), t.text);
            c.text_str(arrow, &f.small, rect(label_x + lw + 2.0, y, 14.0, COLS_H), t.accent);
        }
        c.fill(rect(a.x, y + COLS_H - 1.0, a.width, 1.0), t.divider);
    }

    /// Only the visible rows (plus one partially visible) are drawn.
    fn draw_rows(&self, c: &Canvas, a: &Area, cols: &Columns, h: f32, caret_x: Option<f32>) {
        let p = a.panel;
        let (top, lh) = Self::list_area(h);
        c.clip(rect(a.x, top, a.width, lh));
        let end = (p.top + p.rows_visible + 1).min(p.entries.len());
        for (r, idx) in (p.top..end).enumerate() {
            self.draw_row(c, a, cols, idx, top + r as f32 * ROW_H, caret_x);
        }
        c.unclip();
    }

    /// One entry: cursor or zebra background, mark bar, icon, name (or its rename input), size,
    /// date.
    fn draw_row(&self, c: &Canvas, a: &Area, cols: &Columns, idx: usize, y: f32, caret_x: Option<f32>) {
        let t = self.theme;
        let f = c.fonts;
        let (x, pw) = (a.x, a.width);
        let e: &Entry = &a.panel.entries[idx];
        if idx == a.panel.cursor {
            let col = if a.active { t.cursor_active } else { t.cursor_inactive };
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
        match (&self.mode, caret_x) {
            (Mode::Rename(r), Some(cx)) if r.panel == a.index && r.row == idx => {
                let r_in = rect(cols.name_x - 6.0, y + 1.0, cols.size_x - 8.0 - cols.name_x + 6.0, ROW_H - 2.0);
                draw_input(c, &self.gfx, t, r_in, &r.input, Some(cx));
            }
            _ => c.text(&e.name, &f.name, rect(cols.name_x, y, cols.size_x - cols.name_x - 8.0, ROW_H), tcol),
        }
        let dim = if e.marked { t.marked } else { t.text_dim };
        let size_rect = rect(cols.size_x, y, cols.size_w, ROW_H);
        match e.kind {
            Kind::Parent => {}
            Kind::Dir => match a.panel.size_of(e) {
                Some(s) => {
                    let text = self.size_text(s.bytes());
                    let text = if s.incomplete() { format!("≥ {text}") } else { text };
                    c.text_str(&text, &f.right, size_rect, if s.done() { dim } else { t.accent });
                }
                None => c.text_str(i18n::t("common.folder"), &f.small_right, size_rect, dim),
            },
            _ => {
                c.text_str(&self.size_text(e.size), &f.right, size_rect, if e.hidden && !e.marked { tcol } else { dim })
            }
        }
        if e.kind != Kind::Parent {
            c.text_str(&format_date(e.mtime), &f.small, rect(cols.date_x, y, DATE_W, ROW_H), dim);
        }
    }

    /// Thin at rest, wider and brighter under the mouse or while dragged.
    fn draw_scrollbar(&self, c: &Canvas, a: &Area, w: f32, h: f32) {
        let t = self.theme;
        let i = a.index;
        let Some(sb) = self.scrollbar(i, w, h) else { return };
        let (top, lh) = Self::list_area(h);
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
        let right = a.x + a.width;
        if hot {
            c.fill_round(rect(right - 13.0, top + 2.0, 12.0, lh - 4.0), 6.0, t.scroll_track);
        }
        c.fill_round(rect(right - 3.0 - bw, sb.thumb_y + 2.0, bw, sb.thumb_h - 4.0), bw / 2.0, col);
    }

    /// Error, marked count or directory summary; the quick search term on the right.
    fn draw_status_bar(&self, c: &Canvas, a: &Area, h: f32) {
        let t = self.theme;
        let f = c.fonts;
        let p = a.panel;
        let (x, pw) = (a.x, a.width);
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
                let s = tf("panel.marked", &[("n", &marked), ("size", &self.size_text(marked_bytes))]);
                c.text_str(&s, &f.small, sr, t.marked);
            }
            None => {
                let s = [tn("panel.dirs", p.dirs as u64), tn("panel.files", p.files as u64), self.size_text(p.bytes)]
                    .join("  ·  ");
                c.text_str(&s, &f.small, sr, t.text_dim);
            }
        }
    }
}
