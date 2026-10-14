//! Reusable drawing pieces: card, key chips, text input box and the key bar.

use crate::app::App;
use crate::app::format::utf16;
use crate::app::layout::{KEYBAR_H, PAD};
use crate::app::text_input::TextInput;
use crate::sys::gfx::{Canvas, Rect, rect};

pub(crate) const HINTS: [(&str, &str); 10] = [
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

impl App {
    /// Text input box with caret; scrolls horizontally to keep the caret visible.
    pub(crate) fn draw_input(&self, c: &Canvas, r: Rect, input: &TextInput, caret_x: f32) {
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
        c.text(
            &input.text,
            &c.fonts.name,
            rect(inner.left - shift, inner.top, 10000.0, inner.bottom - inner.top),
            t.text,
        );
        let caret = inner.left - shift + caret_x;
        c.fill(rect(caret, inner.top + 5.0, 1.5, inner.bottom - inner.top - 10.0), t.accent);
        c.unclip();
    }

    /// Row of key chips with labels; returns the x after the last one.
    pub(crate) fn draw_chips(&self, c: &Canvas, mut x: f32, y: f32, h: f32, chips: &[(&str, &str)]) -> f32 {
        let t = self.theme;
        for (key, label) in chips {
            let kw = self.gfx.measure(&utf16(key), &c.fonts.small_center) + 12.0;
            let lw = self.gfx.measure(&utf16(label), &c.fonts.small) + 2.0;
            let chip = rect(x, y + (h - 18.0) / 2.0, kw, 18.0);
            c.fill_round(chip, 4.0, t.popup_bg);
            c.text_str(key, &c.fonts.small_center, chip, t.accent);
            x += kw + 6.0;
            c.text_str(label, &c.fonts.small, rect(x, y, lw, h), t.text_dim);
            x += lw + 16.0;
        }
        x
    }

    pub(crate) fn draw_card(&self, c: &Canvas, r: Rect) {
        let t = self.theme;
        let shadow = rect(r.left + 2.0, r.top + 5.0, r.right - r.left, r.bottom - r.top);
        c.fill_round(shadow, 10.0, crate::theme::rgba(0x000000, 0.4));
        c.fill_round(r, 10.0, t.popup_bg);
        c.stroke_round(r, 10.0, 1.0, t.popup_border);
    }

    pub(crate) fn draw_keybar(&self, c: &Canvas, w: f32, h: f32) {
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
}
