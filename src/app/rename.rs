//! Inline renaming in the file list: a slow second click in the name column of the entry under the cursor (like
//! Explorer) turns it into a text input. Slow means more than `QUICK_CLICK_MS` apart. Shift+F6 keeps using the rename dialog.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;

use windows::Win32::UI::Input::KeyboardAndMouse::{GetDoubleClickTime, VIRTUAL_KEY, VK_ESCAPE, VK_RETURN, VK_TAB};

use crate::app::format::{utf16, valid_name};
use crate::app::layout::ROW_H;
use crate::app::text_input::TextInput;
use crate::app::{App, Mode, TIMER_RENAME, Tone};
use crate::files::listing::Kind;
use crate::i18n::{t, tf};
use crate::ops::{error_text, same_name_ignoring_case};
use crate::sys::win;

/// The mouse may move this far (DIPs) between the click and the start of the rename.
const SLOP: f32 = 4.0;
/// Two clicks on a row closer than this are a double click (open); slower ones are two single
/// clicks (rename). Shorter than the system's double-click time (usually 500 ms), which made
/// the pause before a rename feel sluggish.
const QUICK_CLICK_MS: u32 = 300;

/// Double-click time for the file rows: `QUICK_CLICK_MS`, or the system's if that is shorter.
pub(crate) fn double_click_ms() -> u32 {
    unsafe { GetDoubleClickTime() }.min(QUICK_CLICK_MS)
}

/// Name being edited in place.
pub(crate) struct InlineRename {
    pub(crate) panel: usize,
    /// Index of the entry (follows it when the list is reloaded).
    pub(crate) row: usize,
    /// Its name before the rename.
    pub(crate) name: Box<[u16]>,
    pub(crate) input: TextInput,
}

/// A click on the name under the cursor: becomes a rename unless a second click (a double
/// click), a key or a mouse move comes first.
#[derive(Clone, Copy)]
pub(crate) struct PendingRename {
    panel: usize,
    row: usize,
    at: (f32, f32),
}

impl App {
    /// Left click in the name column of `row`, which already had the cursor in the active panel.
    pub(crate) fn arm_rename(&mut self, panel: usize, row: usize, at: (f32, f32)) {
        let p = &self.panels[panel];
        if p.archive.is_some() || p.entries.get(row).is_none_or(|e| e.kind == Kind::Parent) {
            return;
        }
        self.pending_rename = Some(PendingRename { panel, row, at });
        win::set_timer(self.hwnd, TIMER_RENAME, double_click_ms());
    }

    pub(crate) fn cancel_pending_rename(&mut self) {
        if self.pending_rename.take().is_some() {
            win::kill_timer(self.hwnd, TIMER_RENAME);
        }
    }

    /// A mouse move cancels a pending rename.
    pub(crate) fn pending_rename_moved(&mut self, x: f32, y: f32) {
        if let Some(r) = self.pending_rename
            && ((x - r.at.0).abs() > SLOP || (y - r.at.1).abs() > SLOP)
        {
            self.cancel_pending_rename();
        }
    }

    /// `TIMER_RENAME`: no second click came; start editing if nothing changed meanwhile.
    pub(crate) fn rename_tick(&mut self) {
        let Some(r) = self.pending_rename.take() else { return };
        if matches!(self.mode, Mode::Normal)
            && self.drag.is_none()
            && self.active == r.panel
            && self.panels[r.panel].cursor == r.row
        {
            self.start_rename(r.panel, r.row);
        }
    }

    /// Edit the name of entry `row` in place: files with the name before the extension selected,
    /// folders entirely.
    fn start_rename(&mut self, panel: usize, row: usize) {
        let p = &mut self.panels[panel];
        let Some(e) = p.entries.get(row).filter(|e| e.kind != Kind::Parent) else { return };
        let name = e.name.clone();
        let end = match name.iter().rposition(|&c| c == b'.' as u16) {
            Some(dot) if dot > 0 && !e.is_dir() => dot,
            _ => name.len(),
        };
        p.search.clear();
        p.ensure_visible();
        let input = TextInput::with_selection(name.to_vec(), 0..end);
        self.mode = Mode::Rename(InlineRename { panel, row, name, input });
    }

    /// Keys while renaming: Enter / Tab rename, Esc cancels, the rest edits.
    pub(crate) fn rename_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool) -> bool {
        let hwnd = self.hwnd;
        let Mode::Rename(r) = &mut self.mode else { return false };
        match vk {
            VK_RETURN | VK_TAB => {
                self.commit_rename();
            }
            VK_ESCAPE => self.mode = Mode::Normal,
            // Everything else stays in the input (no cursor movement in the list meanwhile).
            _ => {
                r.input.key(vk, ctrl, hwnd);
            }
        }
        true
    }

    /// Rename to the typed name. `false` = it failed: the notice says why and the input stays
    /// open. An unchanged or empty name just ends the editing.
    pub(crate) fn commit_rename(&mut self) -> bool {
        let Mode::Rename(r) = &self.mode else { return true };
        let new = r.input.string().trim().to_string();
        let panel = r.panel;
        let dir = self.panels[panel].dir.clone();
        let from = dir.join(OsString::from_wide(&r.name));
        if new.is_empty() || from.file_name().is_some_and(|n| n == new.as_str()) {
            self.mode = Mode::Normal;
            return true;
        }
        if !valid_name(&new, false) {
            self.notify(tf("notice.invalid_name", &[("name", &new)]), Tone::Error);
            return false;
        }
        let to = dir.join(&new);
        if to.exists() && !same_name_ignoring_case(&from, &to) {
            self.notify(tf("notice.exists", &[("name", &new)]), Tone::Error);
            return false;
        }
        if let Err(e) = std::fs::rename(&from, &to) {
            let msg = format!("{}: {}", t("dialog.rename"), error_text(&e));
            self.notify(msg, Tone::Error);
            return false;
        }
        self.mode = Mode::Normal;
        self.navigate(panel, dir, Some(utf16(&new).into()));
        true
    }

    /// After `panel` was reloaded: keep editing the same entry, or stop if it is gone.
    pub(crate) fn follow_rename(&mut self, panel: usize) {
        let Mode::Rename(r) = &mut self.mode else { return };
        if r.panel != panel {
            return;
        }
        match self.panels[panel].entries.iter().position(|e| e.name == r.name) {
            Some(i) => r.row = i,
            None => self.mode = Mode::Normal,
        }
    }

    /// `WM_KILLFOCUS`: finish the rename (on failure the input stays open).
    pub fn on_focus_lost(&mut self) {
        self.cancel_pending_rename();
        // Alt+Tab: the Alt release goes to the other window.
        self.on_alt_up();
        if let Mode::Rename(_) = self.mode {
            self.commit_rename();
            self.invalidate();
        }
    }

    /// Whether (`x`, `y`) is on the input of the inline rename.
    pub(crate) fn in_rename_input(&self, x: f32, y: f32) -> bool {
        let Mode::Rename(r) = &self.mode else { return false };
        let (w, h) = self.gfx.size_dips(self.hwnd);
        let p = &self.panels[r.panel];
        let (top, _) = self.list_area(h);
        let Some(vis) = r.row.checked_sub(p.top) else { return false };
        let c = self.columns(r.panel, w);
        let ry = top + vis as f32 * ROW_H;
        x >= c.name_x - 6.0 && x < c.size_x - 8.0 && y >= ry && y < ry + ROW_H
    }
}
