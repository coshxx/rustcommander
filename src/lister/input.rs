//! Keyboard and mouse input of a lister window.

use windows::Win32::UI::Input::KeyboardAndMouse::*;

use crate::lister::text::{Encoding, Mode};
use crate::lister::{Lister, shared};
use crate::sys::win;

/// Wheel delta per line (one notch = 120 = three lines, like the file lists).
const WHEEL_PER_LINE: i32 = 40;

impl Lister {
    /// `WM_KEYDOWN`; `true` = handled.
    pub(super) fn on_key(&mut self, vk: VIRTUAL_KEY) -> bool {
        let ctrl = win::key_down(VK_CONTROL);
        let shift = win::key_down(VK_SHIFT);
        if let Some(input) = &mut self.find {
            match vk {
                VK_RETURN => self.commit_find(),
                VK_ESCAPE => self.find = None,
                _ => {
                    if !input.key(vk, ctrl, self.hwnd) {
                        return false;
                    }
                }
            }
            self.invalidate();
            return true;
        }
        if !matches!(vk, VK_SHIFT | VK_CONTROL | VK_MENU) {
            self.message = None;
        }
        let page = self.rows as isize;
        let step = if ctrl { (self.cols / 2).max(1) } else { 4 };
        match vk {
            VK_ESCAPE => match &self.running {
                Some(running) => running.cancel(),
                None => self.close(),
            },
            VK_F3 if self.searched => self.search(!shift),
            VK_F3 => self.close(),
            VK_F7 => self.open_find(),
            VK_F if ctrl => self.open_find(),
            VK_UP => self.scroll(-1),
            VK_DOWN => self.scroll(1),
            VK_PRIOR => self.scroll(-page),
            VK_NEXT => self.scroll(page),
            VK_HOME => {
                self.top = 0;
                self.left = 0;
                self.relayout();
            }
            VK_END => self.top = self.last_top(),
            VK_LEFT => self.left = self.left.saturating_sub(step),
            VK_RIGHT => {
                self.left += step;
                self.clamp();
            }
            _ => return false,
        }
        self.invalidate();
        true
    }

    /// `WM_SYSKEYDOWN`: Alt+G in the search dialog toggles "match case".
    pub(super) fn on_syskey(&mut self, vk: VIRTUAL_KEY) -> bool {
        if self.find.is_some() && vk == VK_G {
            let mut s = shared();
            s.match_case = !s.match_case;
            drop(s);
            self.invalidate();
            return true;
        }
        false
    }

    /// `WM_CHAR`: typing in the search dialog, otherwise the letter and digit commands.
    pub(super) fn on_char(&mut self, c: u16) {
        if c < 0x20 || c == 0x7F {
            return;
        }
        if let Some(input) = &mut self.find {
            input.insert(c);
            self.invalidate();
            return;
        }
        match char::from_u32(c as u32).unwrap_or('\0') {
            '1' => self.set_mode(Mode::Text),
            '2' => self.set_mode(Mode::Binary),
            '3' => self.set_mode(Mode::Hex),
            'a' | 'A' => self.set_encoding(Encoding::Ansi),
            's' | 'S' => self.set_encoding(Encoding::Oem),
            'u' => {
                self.set_encoding(Encoding::Utf8);
                self.set_mode(Mode::Text);
            }
            'U' => {
                let be = self.file.bytes().starts_with(&[0xFE, 0xFF]);
                self.set_encoding(if be { Encoding::Utf16Be } else { Encoding::Utf16Le });
                self.set_mode(Mode::Text);
            }
            'w' | 'W' => self.toggle_wrap(),
            'q' | 'Q' => self.close(),
            _ => return,
        }
        self.message = None;
        self.invalidate();
    }

    /// `WM_MOUSEWHEEL`.
    pub(super) fn on_wheel(&mut self, delta: i16) {
        self.wheel += delta as i32;
        let lines = self.wheel / WHEEL_PER_LINE;
        self.wheel -= lines * WHEEL_PER_LINE;
        if lines != 0 {
            self.scroll(-lines as isize);
            self.invalidate();
        }
    }

    /// Left button: grab the scrollbar thumb, or page by clicking its track.
    /// `true` = capture the mouse.
    pub(super) fn on_button_down(&mut self, x: f32, y: f32) -> bool {
        let Some(bar) = self.scrollbar() else { return false };
        if x < bar.track.left || y >= bar.track.bottom {
            return false;
        }
        if y >= bar.thumb_y && y < bar.thumb_y + bar.thumb_h {
            self.drag = Some(y - bar.thumb_y);
            self.invalidate();
            return true;
        }
        let page = self.rows as isize;
        self.scroll(if y < bar.thumb_y { -page } else { page });
        self.invalidate();
        false
    }

    /// Dragging the thumb: the thumb position is a byte position in the file.
    pub(super) fn on_mouse_move(&mut self, y: f32) {
        let (Some(grab), Some(bar)) = (self.drag, self.scrollbar()) else { return };
        let range = (bar.track.bottom - bar.track.top - bar.thumb_h).max(1.0);
        let frac = ((y - grab - bar.track.top) / range).clamp(0.0, 1.0) as f64;
        let base = self.doc().base();
        let last = self.last_top();
        let pos = base + ((last - base) as f64 * frac) as usize;
        self.top = self.doc().align(pos);
        self.clamp();
        self.invalidate();
    }

    /// `true` = a drag ended (release the capture).
    pub(super) fn on_button_up(&mut self) -> bool {
        let ended = self.drag.take().is_some();
        self.invalidate();
        ended
    }

    pub(super) fn cancel_drag(&mut self) {
        if self.drag.take().is_some() {
            self.invalidate();
        }
    }
}
