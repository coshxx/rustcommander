//! Drawing the search dialog (Alt+F7): inputs, status line, hits, key chips.

use crate::app::App;
use crate::app::find::{Find, LABELS, LIST};
use crate::app::format::{decimal, format_date};
use crate::app::layout::{DATE_W, FIND_LABEL_W, ICON_W, ROW_H};
use crate::files::listing::{Kind, kind_from_extension};
use crate::files::names::to_utf16_nul;
use crate::i18n::{self, tf, tn};
use crate::sys::gfx::{Canvas, Rect, rect};
use crate::ui::panel::icon;
use crate::ui::widgets::{draw_card, draw_chips, draw_input};

impl App {
    pub(crate) fn draw_find(&self, c: &Canvas, w: f32, h: f32, caret_x: Option<f32>) {
        let Some(f) = &self.find else { return };
        let t = self.theme;
        c.fill(rect(0.0, 0.0, w, h), crate::theme::rgba(0x000000, 0.35));
        let l = Self::find_layout(w, h);
        draw_card(c, t, l.card);
        let x = l.card.left + 20.0;
        c.text_str(i18n::t("find.title"), &c.fonts.path, rect(x, l.card.top + 14.0, 200.0, 28.0), t.text);
        for (i, (r, label)) in l.inputs.iter().zip(LABELS).enumerate() {
            let col = if f.focus == i { t.text } else { t.text_dim };
            c.text_str(i18n::t(label), &c.fonts.name, rect(x, r.top, FIND_LABEL_W, r.bottom - r.top), col);
            draw_input(c, &self.gfx, t, *r, &f.inputs[i], caret_x.filter(|_| f.focus == i));
        }
        let (status, col) = self.find_status(f);
        c.text_str(&status, &c.fonts.small, l.status, col);
        self.draw_hits(c, f, l.list);
        let running = f.running.is_some();
        let tr = i18n::t;
        let esc = ("Esc", tr(if running { "common.cancel" } else { "common.close" }));
        let field = ("Tab", tr("find.field"));
        let chips = if f.focus == LIST {
            vec![("Enter", tr("find.go")), ("F3", tr("find.view")), field, esc]
        } else {
            vec![("Enter", tr(if running { "find.search_again" } else { "find.search" })), field, esc]
        };
        draw_chips(c, &self.gfx, t, x, l.chips_y, 28.0, &chips);
    }

    /// Hint, progress, result or error.
    fn find_status(&self, f: &Find) -> (String, crate::theme::Color) {
        let t = self.theme;
        if let Some(e) = &f.error {
            return (e.clone(), t.error);
        }
        if !f.searched {
            return (i18n::t("find.hint").into(), t.text_hidden);
        }
        let status = format!("{}  ·  {}", tn("find.hits", f.hits.len() as u64), tn("find.dirs", f.dirs));
        if f.running.is_some() {
            return (tf("find.running", &[("status", &status)]), t.accent);
        }
        let secs = decimal(&format!("{:.1}", f.elapsed.as_secs_f64()));
        let text = if f.cancelled {
            tf("find.cancelled", &[("status", &status)])
        } else {
            tf("find.done", &[("status", &status), ("secs", &secs)])
        };
        (text, t.text_dim)
    }

    /// Visible hits: icon, name, folder (relative to the searched one), size, date.
    fn draw_hits(&self, c: &Canvas, f: &Find, list: Rect) {
        let t = self.theme;
        let fonts = c.fonts;
        let lw = list.right - list.left;
        c.fill(rect(list.left, list.top - 1.0, lw, 1.0), t.divider);
        let size_w = if self.options.exact_sizes { 140.0 } else { 84.0 };
        let date_x = list.right - 8.0 - DATE_W;
        let size_x = date_x - 14.0 - size_w;
        let name_x = list.left + 8.0 + ICON_W + 6.0;
        let dir_x = name_x + ((size_x - name_x) * 0.45).floor();
        c.clip(list);
        let rows = ((list.bottom - list.top) / ROW_H).ceil() as usize;
        for (r, i) in (f.top..(f.top + rows).min(f.hits.len())).enumerate() {
            let hit = &f.hits[i];
            let y = list.top + r as f32 * ROW_H;
            if i == f.sel {
                let col = if f.focus == LIST { t.cursor_active } else { t.cursor_inactive };
                c.fill_round(rect(list.left + 2.0, y + 1.0, lw - 4.0, ROW_H - 2.0), 4.0, col);
            } else if i % 2 == 1 {
                c.fill(rect(list.left, y, lw, ROW_H), t.row_alt);
            }
            let name = hit.path.file_name().map(to_utf16_nul).unwrap_or_default();
            let name = &name[..name.len().saturating_sub(1)];
            let kind = if hit.is_dir { Kind::Dir } else { kind_from_extension(name) };
            let (glyph, icol) = icon(kind, t);
            c.text(&[glyph], &fonts.icon, rect(list.left + 8.0, y, ICON_W, ROW_H), icol);
            c.text(name, &fonts.name, rect(name_x, y, dir_x - name_x - 8.0, ROW_H), t.text);
            let dir = hit.path.parent().and_then(|p| p.strip_prefix(&f.root).ok()).map(|p| p.display().to_string());
            let dir = dir.filter(|d| !d.is_empty()).map_or_else(|| ".".to_string(), |d| format!(".\\{d}"));
            c.text_str(&dir, &fonts.small, rect(dir_x, y, size_x - dir_x - 8.0, ROW_H), t.text_dim);
            let size = if hit.is_dir { i18n::t("common.folder").to_string() } else { self.size_text(hit.size) };
            let size_font = if hit.is_dir { &fonts.small_right } else { &fonts.right };
            c.text_str(&size, size_font, rect(size_x, y, size_w, ROW_H), t.text_dim);
            c.text_str(&format_date(hit.mtime), &fonts.small, rect(date_x, y, DATE_W, ROW_H), t.text_dim);
        }
        c.unclip();
        // Thin position indicator when not everything fits.
        let visible = ((list.bottom - list.top) / ROW_H).floor() as usize;
        if f.hits.len() > visible && visible > 0 {
            let lh = list.bottom - list.top;
            let th = (lh * visible as f32 / f.hits.len() as f32).max(24.0);
            let max_top = f.hits.len() - visible;
            let ty = list.top + (lh - th) * f.top.min(max_top) as f32 / max_top as f32;
            c.fill_round(rect(list.right - 5.0, ty + 2.0, 4.0, th - 4.0), 2.0, t.scroll_thumb);
        }
    }
}
