//! Single-line text input used by the path bar and the dialogs.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::*;

use crate::sys::win;

/// Single-line text input (path bar and dialogs).
pub(crate) struct TextInput {
    pub(crate) text: Vec<u16>,
    pub(crate) caret: usize,
    /// Whole text selected: typing replaces it.
    pub(crate) all: bool,
}

impl TextInput {
    /// Caret at the end, nothing selected.
    pub(crate) fn new(text: Vec<u16>) -> TextInput {
        let caret = text.len();
        TextInput { text, caret, all: false }
    }

    /// Whole text selected (typing replaces it).
    pub(crate) fn selected(text: Vec<u16>) -> TextInput {
        TextInput { all: !text.is_empty(), ..TextInput::new(text) }
    }

    /// Editing keys; `true` = handled.
    pub(crate) fn key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, hwnd: HWND) -> bool {
        let all = std::mem::take(&mut self.all);
        match vk {
            VK_LEFT if all => self.caret = 0,
            VK_RIGHT if all => self.caret = self.text.len(),
            VK_BACK | VK_DELETE if all => {
                self.text.clear();
                self.caret = 0;
            }
            VK_LEFT => self.caret = self.caret.saturating_sub(1),
            VK_RIGHT => self.caret = (self.caret + 1).min(self.text.len()),
            VK_HOME => self.caret = 0,
            VK_END => self.caret = self.text.len(),
            VK_BACK => {
                if self.caret > 0 {
                    self.caret -= 1;
                    self.text.remove(self.caret);
                }
            }
            VK_DELETE => {
                if self.caret < self.text.len() {
                    self.text.remove(self.caret);
                }
            }
            VK_V if ctrl => {
                if all {
                    self.text.clear();
                    self.caret = 0;
                }
                if let Some(clip) = win::clipboard_text(hwnd) {
                    let clip: Vec<u16> = clip.into_iter().filter(|&c| c >= 0x20).collect();
                    let n = clip.len();
                    self.text.splice(self.caret..self.caret, clip);
                    self.caret += n;
                }
            }
            VK_A if ctrl => self.all = !self.text.is_empty(),
            _ => {
                self.all = all;
                return false;
            }
        }
        true
    }

    /// Typed character (replaces the selection).
    pub(crate) fn insert(&mut self, c: u16) {
        if std::mem::take(&mut self.all) {
            self.text.clear();
            self.caret = 0;
        }
        self.text.insert(self.caret, c);
        self.caret += 1;
    }

    pub(crate) fn string(&self) -> String {
        String::from_utf16_lossy(&self.text)
    }
}
