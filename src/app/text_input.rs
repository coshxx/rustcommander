//! Single-line text input used by the path bar, the dialogs and inline renaming.

use std::ops::Range;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::*;

use crate::sys::win;

/// Single-line text input with a selection (path bar, dialogs, inline renaming).
pub(crate) struct TextInput {
    pub(crate) text: Vec<u16>,
    pub(crate) caret: usize,
    /// Other end of the selection (the caret is one end); `None` = nothing selected. Typing
    /// replaces the selection.
    pub(crate) anchor: Option<usize>,
}

impl TextInput {
    /// Caret at the end, nothing selected.
    pub(crate) fn new(text: Vec<u16>) -> TextInput {
        let caret = text.len();
        TextInput { text, caret, anchor: None }
    }

    /// Whole text selected.
    pub(crate) fn selected(text: Vec<u16>) -> TextInput {
        let n = text.len();
        TextInput::with_selection(text, 0..n)
    }

    /// `sel` selected, the caret at its end.
    pub(crate) fn with_selection(text: Vec<u16>, sel: Range<usize>) -> TextInput {
        TextInput { anchor: (sel.start < sel.end).then_some(sel.start), caret: sel.end, text }
    }

    /// The selected range, if any.
    pub(crate) fn selection(&self) -> Option<Range<usize>> {
        let a = self.anchor.filter(|&a| a != self.caret)?;
        Some(a.min(self.caret)..a.max(self.caret))
    }

    /// Remove the selected text; `false` if nothing was selected.
    fn delete_selection(&mut self) -> bool {
        let sel = self.selection();
        self.anchor = None;
        let Some(sel) = sel else { return false };
        self.caret = sel.start;
        self.text.drain(sel);
        true
    }

    /// Editing keys (Shift with the cursor keys selects); `true` = handled.
    pub(crate) fn key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, hwnd: HWND) -> bool {
        match vk {
            VK_LEFT | VK_RIGHT | VK_HOME | VK_END => {
                let shift = win::key_down(VK_SHIFT);
                let sel = self.selection().filter(|_| !shift);
                let to = match (vk, sel) {
                    (VK_LEFT, Some(s)) => s.start,
                    (VK_RIGHT, Some(s)) => s.end,
                    (VK_LEFT, None) => self.caret.saturating_sub(1),
                    (VK_RIGHT, None) => (self.caret + 1).min(self.text.len()),
                    (VK_HOME, _) => 0,
                    _ => self.text.len(),
                };
                self.anchor = if shift { Some(self.anchor.unwrap_or(self.caret)) } else { None };
                self.caret = to;
            }
            VK_BACK => {
                if !self.delete_selection() && self.caret > 0 {
                    self.caret -= 1;
                    self.text.remove(self.caret);
                }
            }
            VK_DELETE => {
                if !self.delete_selection() && self.caret < self.text.len() {
                    self.text.remove(self.caret);
                }
            }
            VK_C | VK_X if ctrl => {
                if let Some(sel) = self.selection() {
                    win::set_clipboard_text(hwnd, &self.text[sel]);
                    if vk == VK_X {
                        self.delete_selection();
                    }
                }
            }
            VK_V if ctrl => {
                self.delete_selection();
                if let Some(clip) = win::clipboard_text(hwnd) {
                    let clip: Vec<u16> = clip.into_iter().filter(|&c| c >= 0x20).collect();
                    let n = clip.len();
                    self.text.splice(self.caret..self.caret, clip);
                    self.caret += n;
                }
            }
            VK_A if ctrl => {
                self.anchor = Some(0);
                self.caret = self.text.len();
            }
            _ => return false,
        }
        true
    }

    /// Typed character (replaces the selection).
    pub(crate) fn insert(&mut self, c: u16) {
        self.delete_selection();
        self.text.insert(self.caret, c);
        self.caret += 1;
    }

    pub(crate) fn string(&self) -> String {
        String::from_utf16_lossy(&self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(s: &str, sel: Range<usize>) -> TextInput {
        TextInput::with_selection(s.encode_utf16().collect(), sel)
    }

    #[test]
    fn typing_replaces_the_selection() {
        let mut i = input("report.pdf", 0..6);
        i.insert(b'x' as u16);
        assert_eq!(i.string(), "x.pdf");
        assert_eq!(i.caret, 1);
        assert_eq!(i.selection(), None);
    }

    #[test]
    fn deleting_the_selection() {
        let mut i = input("abcdef", 2..4);
        assert!(i.delete_selection());
        assert_eq!(i.string(), "abef");
        assert_eq!(i.caret, 2);
        assert!(!i.delete_selection());
        let empty = TextInput::selected(Vec::new());
        assert_eq!(empty.selection(), None);
    }
}
