//! Drawing one panel: path bar, column headers, file rows, scrollbar, status bar.

use crate::app::format::{format_date, format_size};
use crate::app::layout::{
    CHIP_W, COLS_H, DATE_W, ICON_W, KEYBAR_H, PAD, PATH_H, ROW_H, SIZE_W, STATUS_H,
};
use crate::app::mouse::Drag;
use crate::app::{App, LOADING_DELAY_MS, Mode};
use crate::files::listing::Kind;
use crate::sys::gfx::{Canvas, rect};
use crate::theme::{Color, Theme};

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

impl App {
    pub(crate) fn draw_panel(&self, c: &Canvas, i: usize, w: f32, h: f32, caret_x: Option<f32>) {
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
                _ => c.text_str(&format_size(e.size), &f.right, rect(size_x, y, SIZE_W, ROW_H), if e.hidden && !e.marked { tcol } else { dim }),
            }
            if e.kind != Kind::Parent {
                c.text_str(&format_date(e.mtime), &f.small, rect(date_x, y, DATE_W, ROW_H), dim);
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
                let s = format!("{marked} markiert  ·  {}", format_size(marked_bytes));
                c.text_str(&s, &f.small, sr, t.marked);
            }
            None => {
                let s = format!("{} Ordner  ·  {} Dateien  ·  {}", p.dirs, p.files, format_size(p.bytes));
                c.text_str(&s, &f.small, sr, t.text_dim);
            }
        }
    }
}
