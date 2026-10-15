//! Drawing a lister window: the visible lines, scrollbar, status bar and search dialog.

use crate::app::format::{format_size, utf16};
use crate::i18n::{t as t_, tf};
use crate::lister::text::{Line, Mode};
use crate::lister::{Lister, shared};
use crate::sys::gfx::{Canvas, Rect, rect};
use crate::ui::widgets::{draw_card, draw_chips, draw_input};

// Layout in DIPs.
pub(super) const LINE_H: f32 = 18.0;
const STATUS_H: f32 = 26.0;
const PAD_X: f32 = 10.0;
const PAD_Y: f32 = 6.0;
const BAR_W: f32 = 10.0;
const THUMB_MIN_H: f32 = 24.0;

/// Where the lines go in a window `w` × `h` DIPs.
pub(super) fn text_area(w: f32, h: f32) -> Rect {
    rect(PAD_X, PAD_Y, (w - 2.0 * PAD_X - BAR_W).max(0.0), (h - STATUS_H - 2.0 * PAD_Y).max(0.0))
}

pub(super) struct ScrollBar {
    pub(super) track: Rect,
    pub(super) thumb_y: f32,
    pub(super) thumb_h: f32,
}

impl Lister {
    /// Scrollbar geometry; `None` if the whole file fits.
    pub(super) fn scrollbar(&mut self) -> Option<ScrollBar> {
        let last = self.last_top();
        let doc = self.doc();
        let base = doc.base();
        let len = self.file.bytes().len();
        if last <= base || len == 0 {
            return None;
        }
        let (w, h) = self.gfx.size_dips(self.hwnd);
        let track = rect(w - BAR_W - 2.0, 2.0, BAR_W, (h - STATUS_H - 4.0).max(0.0));
        let track_h = track.bottom - track.top;
        let shown = doc.view_end(self.top, self.rows).saturating_sub(self.top) as f32 / len as f32;
        let thumb_h = (track_h * shown).max(THUMB_MIN_H).min(track_h);
        let frac = (self.top - base) as f32 / (last - base) as f32;
        Some(ScrollBar { track, thumb_y: track.top + (track_h - thumb_h) * frac.min(1.0), thumb_h })
    }

    /// `WM_PAINT`: draw the whole window.
    pub(super) fn paint(&mut self) {
        if self.gfx.ensure_target(self.hwnd).is_err() {
            return;
        }
        let bar = self.scrollbar();
        let (w, h) = self.gfx.size_dips(self.hwnd);
        let t = self.theme;
        let caret_x = self.find.as_ref().map(|i| self.gfx.caret_x(&i.text[..i.caret]));
        let Some(c) = self.gfx.begin(t.panel_bg) else { return };
        let area = text_area(w, h);
        c.clip(area);
        let doc = self.doc();
        let len = self.file.bytes().len();
        if len == 0 {
            c.text_str(t_("lister.empty"), &c.fonts.mono, rect(area.left, area.top, 400.0, LINE_H), t.text_dim);
        }
        let mut p = self.top;
        for row in 0..self.rows {
            if p >= len {
                break;
            }
            let line = doc.line(p);
            self.draw_line(&c, &line, area.left, area.top + row as f32 * LINE_H);
            if line.end <= p {
                break;
            }
            p = line.end;
        }
        c.unclip();
        if let Some(bar) = bar {
            c.fill_round(bar.track, 3.0, t.scroll_track);
            let color = if self.drag.is_some() { t.scroll_thumb_drag } else { t.scroll_thumb };
            let thumb = rect(bar.track.left + 2.0, bar.thumb_y, BAR_W - 4.0, bar.thumb_h);
            c.fill_round(thumb, 3.0, color);
        }
        self.draw_status(&c, w, h);
        if let (Some(input), Some(cx)) = (&self.find, caret_x) {
            c.fill(rect(0.0, 0.0, w, h), crate::theme::rgba(0x000000, 0.35));
            let dw = (w - 48.0).clamp(200.0, 520.0);
            let r = rect(((w - dw) / 2.0).floor(), ((h - 136.0) / 2.5).floor(), dw, 136.0);
            draw_card(&c, t, r);
            let (x, iw) = (r.left + 20.0, dw - 40.0);
            c.text_str(t_("lister.search"), &c.fonts.path, rect(x, r.top + 16.0, iw, 28.0), t.text);
            draw_input(&c, &self.gfx, t, rect(x, r.top + 50.0, iw, 32.0), input, Some(cx));
            let case = t_(if shared().match_case { "lister.match_case" } else { "lister.ignore_case" });
            let chips = [("Enter", t_("lister.search")), ("Alt+G", case), ("Esc", t_("common.cancel"))];
            draw_chips(&c, &self.gfx, t, x, r.top + 94.0, 28.0, &chips);
        }
        self.gfx.end();
    }

    /// One line: search hit background, then the text (the hex offset column dimmed).
    fn draw_line(&self, c: &Canvas, line: &Line, x: f32, y: f32) {
        let t = self.theme;
        let from = self.left.min(line.text.len());
        let to = (self.left + self.cols + 1).min(line.text.len());
        let cw = self.char_w;
        if let Some((s, e)) = self.hit {
            let hit = |i: usize| (s..e).contains(&line.offs[i]);
            let mut i = from;
            while i < to {
                if hit(i) {
                    let run = (i..to).take_while(|&j| hit(j)).count();
                    c.fill(rect(x + (i - from) as f32 * cw, y, run as f32 * cw, LINE_H), t.search_hit);
                    i += run;
                } else {
                    i += 1;
                }
            }
        }
        let dim_until = if self.mode == Mode::Hex {
            line.offs.iter().position(|&o| o != usize::MAX).unwrap_or(line.text.len())
        } else {
            0
        };
        let split = dim_until.clamp(from, to);
        let f = &c.fonts.mono;
        c.text(&line.text[from..split], f, rect(x, y, 100000.0, LINE_H), t.text_dim);
        c.text(&line.text[split..to], f, rect(x + (split - from) as f32 * cw, y, 100000.0, LINE_H), t.text);
    }

    /// Bottom bar: mode, encoding, size, position; on the right search progress, a message or
    /// the key hints.
    fn draw_status(&self, c: &Canvas, w: f32, h: f32) {
        let t = self.theme;
        let y = h - STATUS_H;
        c.fill(rect(0.0, y, w, STATUS_H), t.bar_bg);
        c.fill(rect(0.0, y, w, 1.0), t.divider);
        let doc = self.doc();
        let len = self.file.bytes().len();
        let mut parts = vec![self.mode.label().to_string()];
        if self.mode == Mode::Text {
            parts.push(self.enc.label().into());
            parts.push(t_(if self.wrap { "lister.wrap_on" } else { "lister.wrap_off" }).into());
        } else {
            parts.push(self.enc.single_byte().label().into());
        }
        parts.push(format_size(len as u64));
        let at_end = doc.view_end(self.top, self.rows) >= len;
        let percent = if at_end { 100 } else { self.top * 100 / len };
        parts.push(format!("{percent} %"));
        if self.mode == Mode::Hex {
            parts.push(format!("Offset {:X}", self.top));
        }
        let status = parts.join("  ·  ");
        let sw = self.gfx.measure(&utf16(&status), &c.fonts.small);
        c.text_str(&status, &c.fonts.small, rect(PAD_X, y, sw + 4.0, STATUS_H), t.text_dim);
        let x = PAD_X + sw + 28.0;
        let right = rect(x, y, (w - x - PAD_X).max(0.0), STATUS_H);
        c.clip(right);
        if let Some(running) = &self.running {
            let text = tf("lister.searching", &[("percent", &running.percent())]);
            c.text_str(&text, &c.fonts.small, right, t.accent);
        } else if let Some(message) = &self.message {
            c.text_str(message, &c.fonts.small, right, t.marked);
        } else {
            let hints = [
                ("1 2 3", t_("lister.modes")),
                ("W", t_("lister.wrap")),
                ("A S U ⇧U", t_("lister.encoding")),
                ("F7", t_("lister.search")),
                ("Esc", t_("common.close")),
            ];
            draw_chips(c, &self.gfx, t, x, y, STATUS_H, &hints);
        }
        c.unclip();
    }
}
