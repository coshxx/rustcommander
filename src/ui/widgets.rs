//! Reusable drawing pieces: card, key chips, text input box (also used by the lister) and the
//! key bar.

use crate::app::App;
use crate::app::Mode;
use crate::app::format::utf16;
use crate::app::layout::{GEAR_W, KEYBAR_H, PAD, gear_rect};
use crate::app::text_input::TextInput;
use crate::i18n::lookup;
use crate::sys::gfx::{Canvas, Gfx, Rect, rect};
use crate::theme::Theme;

/// Key bar entries: `keybar.<id>.key` and `keybar.<id>.label` in the translations. The options
/// (`keybar.options`) are the gear button at the right end instead.
pub(crate) const HINTS: [&str; 13] =
    ["tab", "view", "edit", "mark", "select", "copy", "move", "rename", "mkdir", "delete", "drive", "path", "find"];

/// Key and label of a key bar entry in the current language.
pub(crate) fn hint(id: &str) -> (&'static str, &'static str) {
    let text = |part: &str| lookup(&format!("keybar.{id}.{part}")).unwrap_or("?");
    (text("key"), text("label"))
}

/// Text input box; with `caret_x` (focused) it shows the caret and scrolls horizontally to keep
/// it visible.
pub(crate) fn draw_input(c: &Canvas, gfx: &Gfx, t: &Theme, r: Rect, input: &TextInput, caret_x: Option<f32>) {
    c.fill_round(r, 5.0, t.edit_bg);
    c.stroke_round(r, 5.0, 1.0, if caret_x.is_some() { t.accent } else { t.popup_border });
    let focused = caret_x.is_some();
    let caret_x = caret_x.unwrap_or(0.0);
    let inner = rect(r.left + 6.0, r.top, r.right - r.left - 12.0, r.bottom - r.top);
    let shift = (caret_x - (inner.right - inner.left) + 4.0).max(0.0);
    c.clip(inner);
    if let Some(sel) = input.selection().filter(|_| focused) {
        let x0 = gfx.caret_x(&input.text[..sel.start]);
        let x1 = gfx.caret_x(&input.text[..sel.end]);
        let r = rect(inner.left - shift + x0 - 1.0, inner.top + 4.0, x1 - x0 + 2.0, inner.bottom - inner.top - 8.0);
        c.fill_round(r, 2.0, t.cursor_active);
    }
    c.text(&input.text, &c.fonts.name, rect(inner.left - shift, inner.top, 10000.0, inner.bottom - inner.top), t.text);
    if focused {
        let caret = inner.left - shift + caret_x;
        c.fill(rect(caret, inner.top + 5.0, 1.5, inner.bottom - inner.top - 10.0), t.accent);
    }
    c.unclip();
}

/// Row of key chips with labels; returns the x after the last one.
pub(crate) fn draw_chips(c: &Canvas, gfx: &Gfx, t: &Theme, mut x: f32, y: f32, h: f32, chips: &[(&str, &str)]) -> f32 {
    for (key, label) in chips {
        let kw = gfx.measure(&utf16(key), &c.fonts.small_center) + 12.0;
        let lw = gfx.measure(&utf16(label), &c.fonts.small) + 2.0;
        let chip = rect(x, y + (h - 18.0) / 2.0, kw, 18.0);
        c.fill_round(chip, 4.0, t.popup_bg);
        c.text_str(key, &c.fonts.small_center, chip, t.accent);
        x += kw + 6.0;
        c.text_str(label, &c.fonts.small, rect(x, y, lw, h), t.text_dim);
        x += lw + 16.0;
    }
    x
}

/// Rounded card with shadow (dialogs, popups, notices).
pub(crate) fn draw_card(c: &Canvas, t: &Theme, r: Rect) {
    let shadow = rect(r.left + 2.0, r.top + 5.0, r.right - r.left, r.bottom - r.top);
    c.fill_round(shadow, 10.0, crate::theme::rgba(0x000000, 0.4));
    c.fill_round(r, 10.0, t.popup_bg);
    c.stroke_round(r, 10.0, 1.0, t.popup_border);
}

impl App {
    /// Bottom row of key hints and the gear button; hints that don't fit are left out.
    pub(crate) fn draw_keybar(&self, c: &Canvas, w: f32, h: f32) {
        let t = self.theme;
        let y = h - KEYBAR_H;
        c.fill(rect(0.0, y, w, KEYBAR_H), t.bar_bg);
        c.fill(rect(0.0, y, w, 1.0), t.divider);
        let gear = gear_rect(w, h);
        let lit = self.hover_gear || matches!(self.mode, Mode::Options { .. });
        if self.hover_gear {
            c.fill_round(gear, 4.0, t.cursor_inactive);
        }
        c.text(&[0xE713], &c.fonts.icon, gear, if lit { t.accent } else { t.text_dim });
        let Some(widths) = &self.hint_widths else { return };
        let mut x = PAD;
        for (id, (kw, lw)) in HINTS.iter().zip(widths) {
            let (key, label) = hint(id);
            let chip_w = kw + 12.0;
            if x + chip_w + lw + 6.0 > w - GEAR_W - 12.0 {
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

    /// "Options (Ctrl+,)" above the hovered gear button.
    pub(crate) fn draw_gear_tip(&self, c: &Canvas, w: f32, h: f32) {
        if !self.hover_gear || matches!(self.mode, Mode::Options { .. }) {
            return;
        }
        let t = self.theme;
        let (key, label) = hint("options");
        let text = format!("{label} ({key})");
        let tw = self.gfx.measure(&utf16(&text), &c.fonts.small) + 20.0;
        let g = gear_rect(w, h);
        let r = rect((g.right - tw).max(4.0), g.top - 34.0, tw, 26.0);
        c.fill_round(r, 5.0, t.popup_bg);
        c.stroke_round(r, 5.0, 1.0, t.popup_border);
        c.text_str(&text, &c.fonts.small_center, r, t.text);
    }
}
