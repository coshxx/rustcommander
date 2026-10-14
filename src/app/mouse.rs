//! Mouse input: clicks, right-click marking, scrollbar dragging and the wheel.

use crate::app::layout::{Hit, ROW_H};
use crate::app::{App, Mode, TIMER_DRAG};
use crate::sys::win;

const DRAG_REPEAT_MS: u32 = 50;

/// Mouse interaction while a button is held (the window has the mouse capture).
#[derive(Clone, Copy)]
pub(crate) enum Drag {
    /// Dragging the scrollbar thumb; `grab` = mouse offset from the thumb's top.
    Thumb { panel: usize, grab: f32 },
    /// Left button held on the track: page towards the mouse, auto-repeating.
    Page { panel: usize, y: f32 },
    /// Right button held: every row the mouse passes gets `marked`.
    Mark { panel: usize, marked: bool, last: usize, y: f32 },
}

impl App {
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
        if self.drag_timer_armed {
            win::kill_timer(self.hwnd, TIMER_DRAG);
            self.drag_timer_armed = false;
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
        if !self.drag_timer_armed {
            win::set_timer(self.hwnd, TIMER_DRAG, ms);
            self.drag_timer_armed = true;
        }
    }

    pub(crate) fn drag_tick(&mut self) {
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
}
