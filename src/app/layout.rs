//! Where things are on screen: sizes in DIPs, panel geometry and hit-testing.

use crate::app::{App, Mode};
use crate::sys::gfx::{Rect, rect};

// Layout in DIPs.
pub(crate) const PATH_H: f32 = 38.0;
pub(crate) const COLS_H: f32 = 24.0;
pub(crate) const ROW_H: f32 = 24.0;
pub(crate) const STATUS_H: f32 = 26.0;
pub(crate) const KEYBAR_H: f32 = 30.0;
pub(crate) const SIZE_W: f32 = 84.0;
pub(crate) const DATE_W: f32 = 118.0;
pub(crate) const PAD: f32 = 12.0;
pub(crate) const ICON_W: f32 = 24.0;
pub(crate) const CHIP_W: f32 = 40.0;
const POPUP_W: f32 = 230.0;
pub(crate) const CARD_W: f32 = 400.0;

/// Width of the scrollbar's mouse zone at the panel's right edge.
const SCROLL_ZONE_W: f32 = 14.0;
const THUMB_MIN_H: f32 = 24.0;

pub(crate) enum Hit {
    Drive(usize),
    Path(usize),
    Row(usize, usize),
    Scrollbar(usize),
    Panel(usize),
    Popup(usize),
    Nothing,
}

pub(crate) struct ScrollBar {
    zone: Rect,
    pub(crate) thumb_y: f32,
    pub(crate) thumb_h: f32,
}

impl App {
    pub(crate) fn panel_x(&self, i: usize, w: f32) -> (f32, f32) {
        let half = (w / 2.0).floor();
        if i == 0 { (0.0, half) } else { (half + 1.0, w - half - 1.0) }
    }

    pub(crate) fn list_area(h: f32) -> (f32, f32) {
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

    pub(crate) fn hit(&self, x: f32, y: f32) -> Hit {
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
    pub(crate) fn scrollbar(&self, i: usize, w: f32, h: f32) -> Option<ScrollBar> {
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

    pub(crate) fn popup_rect(&self, px: f32, n: usize) -> Rect {
        rect(px + PAD - 4.0, PATH_H - 2.0, POPUP_W, n as f32 * ROW_H + 8.0)
    }
}
