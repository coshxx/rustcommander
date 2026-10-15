//! Folder sizes: Space counts the folder under the cursor, Alt+Shift+Enter all folders of the
//! panel. The counting runs in `files::dirsize`; while it does, a timer repaints the growing
//! numbers. Also the sort order, which can depend on counted sizes.

use crate::app::{App, TIMER_SIZES};
use crate::files::sort::SortKey;
use crate::sys::win;

/// Repaint interval while folder sizes are being counted.
const SIZES_REPAINT_MS: u32 = 100;

impl App {
    /// Space: toggle the mark, count the size if it is a folder, move down (unless the
    /// `space_stays` option is on).
    pub(crate) fn space(&mut self) {
        let a = self.active;
        let p = &mut self.panels[a];
        p.toggle_mark();
        let cursor = p.cursor;
        p.count_size(cursor, &self.sink);
        if !self.options.space_stays {
            p.move_cursor(1);
        }
        self.arm_sizes_timer();
    }

    /// Alt+Shift+Enter: count every folder of the active panel.
    pub(crate) fn count_all_sizes(&mut self) {
        self.panels[self.active].count_all_sizes(&self.sink);
        self.arm_sizes_timer();
    }

    /// `WM_APP_SIZE`: a count has finished. Panels sorted by size (with folders) sort again;
    /// the cursor stays on its entry.
    pub fn on_size_done(&mut self) {
        if self.options.sort_dirs {
            for p in self.panels.iter_mut().filter(|p| p.sort.key == SortKey::Size) {
                p.resort(true);
            }
        }
        self.invalidate();
    }

    /// Click on a column header, Ctrl+F3/F5/F6: sort panel `i` by `key`; again = other direction.
    pub(crate) fn sort_by(&mut self, i: usize, key: SortKey) {
        let p = &mut self.panels[i];
        p.sort = p.sort.choose(key);
        p.resort(self.options.sort_dirs);
        p.ensure_visible();
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
