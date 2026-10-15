//! Drawing. Everything is redrawn on every `WM_PAINT`; only visible rows are drawn.

mod find;
mod overlays;
mod panel;
pub(crate) mod widgets;

use std::time::Instant;

use windows::Win32::UI::WindowsAndMessaging::WM_CLOSE;

use crate::app::format::utf16;
use crate::app::layout::KEYBAR_H;
use crate::app::{App, Mode};
use crate::sys::gfx::rect;
use crate::sys::win::SendHwnd;
use crate::ui::widgets::{HINTS, hint};

impl App {
    /// `WM_PAINT`: draw the whole window.
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
                    .map(|id| {
                        let (k, l) = hint(id);
                        (self.gfx.measure(&utf16(k), &f.small_center), self.gfx.measure(&utf16(l), &f.small))
                    })
                    .collect(),
            );
        }
        let (w, h) = self.gfx.size_dips(self.hwnd);
        let input = match &self.mode {
            Mode::PathEdit { input, .. } => Some(input),
            Mode::Dialog(d) => d.input.as_ref(),
            Mode::Find => self.find.as_ref().and_then(|f| f.inputs.get(f.focus)),
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
            Mode::Options { sel, popup } => self.draw_options(&c, *sel, *popup, w, h),
            Mode::Find => self.draw_find(&c, w, h, caret_x),
            _ => {}
        }
        self.draw_gear_tip(&c, w, h);
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
            self.gfx.end();
        }
        let (w, h) = self.gfx.size_dips(self.hwnd);
        let ms = start.elapsed().as_secs_f64() * 1000.0 / n as f64;
        self.timing
            .note(format!("  frame time: {ms:.2} ms (avg over {n} frames, {w:.0}×{h:.0} DIP, {} dpi)", self.gfx.dpi));
    }
}
