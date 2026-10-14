//! Folder sizes: Space counts the folder under the cursor, Alt+Shift+Enter all folders of the
//! panel. The counting runs in `files::dirsize`; while it does, a timer repaints the growing
//! numbers.

use crate::app::{App, TIMER_SIZES};
use crate::sys::win;

/// Repaint interval while folder sizes are being counted.
const SIZES_REPAINT_MS: u32 = 100;

impl App {
    /// Space: toggle the mark, count the size if it is a folder, move down.
    pub(crate) fn space(&mut self) {
        let a = self.active;
        let p = &mut self.panels[a];
        p.toggle_mark();
        let cursor = p.cursor;
        p.count_size(cursor, &self.sink);
        p.move_cursor(1);
        self.arm_sizes_timer();
    }

    /// Alt+Shift+Enter: count every folder of the active panel.
    pub(crate) fn count_all_sizes(&mut self) {
        self.panels[self.active].count_all_sizes(&self.sink);
        self.arm_sizes_timer();
    }

    /// `WM_APP_SIZE`: a count has finished.
    pub fn on_size_done(&mut self) {
        self.invalidate();
    }

    pub(crate) fn sizes_running(&self) -> bool {
        self.panels.iter().any(|p| p.sizes_running())
    }

    /// Keep repainting while counts are running (re-armed from `on_timer`).
    pub(crate) fn arm_sizes_timer(&self) {
        if self.sizes_running() {
            win::set_timer(self.hwnd, TIMER_SIZES, SIZES_REPAINT_MS);
        }
    }
}
