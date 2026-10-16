//! The command line's history as a list (Ctrl+↓), like Total Commander's: it opens above the
//! line, newest entry at the bottom; Enter takes the entry into the line (without running it),
//! Del removes it from the history.

use windows::Win32::UI::Input::KeyboardAndMouse::*;

use crate::app::layout::ROW_H;
use crate::app::text_input::TextInput;
use crate::app::{App, Mode, Tone};
use crate::config::history;
use crate::i18n::t;
use crate::sys::gfx::{Rect, rect};

/// Rows shown at once; more scroll.
pub(crate) const VISIBLE: usize = 10;

pub(crate) struct HistoryList {
    /// The matching commands, oldest first (the newest is at the bottom, next to the line).
    pub(crate) items: Vec<String>,
    pub(crate) sel: usize,
    /// First visible item.
    pub(crate) top: usize,
}

impl HistoryList {
    fn select(&mut self, i: usize) {
        self.sel = i.min(self.items.len().saturating_sub(1));
        if self.sel < self.top {
            self.top = self.sel;
        } else if self.sel >= self.top + VISIBLE {
            self.top = self.sel + 1 - VISIBLE;
        }
    }

    fn scroll(&mut self, delta: isize) {
        let max = self.items.len().saturating_sub(VISIBLE);
        self.top = (self.top as isize + delta).clamp(0, max as isize) as usize;
    }
}

/// The commands containing `filter` (ignoring case), oldest first; `history` is newest first.
fn matching(history: &[String], filter: &str) -> Vec<String> {
    let filter = filter.to_lowercase();
    history.iter().rev().filter(|c| c.to_lowercase().contains(&filter)).cloned().collect()
}

impl App {
    /// Ctrl+↓: open the list, filtered by the text in the line.
    pub(crate) fn open_history_list(&mut self) {
        let filter = self.cmdline.string();
        let items = matching(&self.history, filter.trim());
        if items.is_empty() {
            let key = if self.history.is_empty() { "notice.no_history" } else { "notice.no_history_matches" };
            self.notify(t(key).into(), Tone::Info);
            return;
        }
        let mut list = HistoryList { items, sel: 0, top: 0 };
        list.select(usize::MAX);
        self.mode = Mode::History(list);
    }

    /// Keys while the list is open; `false` = not handled.
    pub(crate) fn history_list_key(&mut self, vk: VIRTUAL_KEY) -> bool {
        let Mode::History(list) = &mut self.mode else { return false };
        let page = VISIBLE as isize - 1;
        let step = |l: &HistoryList, d: isize| (l.sel as isize + d).max(0) as usize;
        match vk {
            VK_UP => list.select(step(list, -1)),
            VK_DOWN => list.select(step(list, 1)),
            VK_PRIOR => list.select(step(list, -page)),
            VK_NEXT => list.select(step(list, page)),
            VK_HOME => list.select(0),
            VK_END => list.select(usize::MAX),
            VK_RETURN => {
                let i = list.sel;
                self.take_history_item(i);
            }
            VK_DELETE => self.delete_history_item(),
            VK_ESCAPE => self.mode = Mode::Normal,
            _ => return false,
        }
        true
    }

    /// Put item `i` into the command line and close the list.
    pub(crate) fn take_history_item(&mut self, i: usize) {
        let Mode::History(list) = std::mem::replace(&mut self.mode, Mode::Normal) else { return };
        if let Some(cmd) = list.items.get(i) {
            self.cmdline = TextInput::new(cmd.encode_utf16().collect());
            self.history_pos = None;
        }
    }

    /// Del: remove the selected command from the history for good.
    fn delete_history_item(&mut self) {
        let Mode::History(list) = &mut self.mode else { return };
        if list.sel >= list.items.len() {
            return;
        }
        let cmd = list.items.remove(list.sel);
        let (sel, empty) = (list.sel, list.items.is_empty());
        list.select(sel);
        list.scroll(0);
        self.history.retain(|c| *c != cmd);
        history::save(&self.history);
        self.history_pos = None;
        if empty {
            self.mode = Mode::Normal;
        }
    }

    /// Wheel over the open list: scroll it, three rows per notch.
    pub(crate) fn history_list_wheel(&mut self, delta: i16) {
        if let Mode::History(list) = &mut self.mode {
            list.scroll(-(delta as isize) * 3 / 120);
        }
    }

    /// The list just above the command line's input box.
    pub(crate) fn history_list_rect(&self, list: &HistoryList) -> Rect {
        let (w, h) = self.gfx.size_dips(self.hwnd);
        let (bar, _, input) = self.command_line_layout(w, h);
        let rows = list.items.len().min(VISIBLE) as f32;
        let height = rows * ROW_H + 8.0;
        rect(input.left - 4.0, bar.top - height - 2.0, input.right - input.left + 8.0, height)
    }

    /// Item of the open list at (`x`, `y`): `Some(Some(i))` = item `i`, `Some(None)` = on the
    /// list but not on an item, `None` = outside (or no list open).
    pub(crate) fn history_item_at(&self, x: f32, y: f32) -> Option<Option<usize>> {
        let Mode::History(list) = &self.mode else { return None };
        let r = self.history_list_rect(list);
        if x < r.left || x >= r.right || y < r.top || y >= r.bottom {
            return None;
        }
        let row = (y - r.top - 4.0) / ROW_H;
        let i = list.top + row.max(0.0) as usize;
        Some((row >= 0.0 && i < list.items.len() && i < list.top + VISIBLE).then_some(i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn filter_ignores_case_and_puts_the_newest_last() {
        let history = h(&[r"cd \\192.168.1.88\bla", "git status", r"CD \\nas\x"]);
        assert_eq!(matching(&history, r"cd \\"), h(&[r"CD \\nas\x", r"cd \\192.168.1.88\bla"]));
        assert_eq!(matching(&history, ""), h(&[r"CD \\nas\x", "git status", r"cd \\192.168.1.88\bla"]));
        assert!(matching(&history, "xyz").is_empty());
    }

    #[test]
    fn selection_scrolls() {
        let mut l = HistoryList { items: (0..25).map(|i| i.to_string()).collect(), sel: 0, top: 0 };
        l.select(usize::MAX);
        assert_eq!((l.sel, l.top), (24, 15));
        l.select(3);
        assert_eq!((l.sel, l.top), (3, 3));
        l.scroll(-10);
        assert_eq!(l.top, 0);
        l.scroll(100);
        assert_eq!(l.top, 15);
    }
}
