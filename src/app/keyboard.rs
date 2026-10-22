//! Keyboard input: key presses (`WM_KEYDOWN`) and typed characters (`WM_CHAR`, `WM_SYSCHAR`).

use windows::Win32::UI::Input::KeyboardAndMouse::*;

use crate::app::dialogs::{DialogKind, PopupItem};
use crate::app::{App, Mode};
use crate::files::names::fold_case;
use crate::files::sort::SortKey;
use crate::i18n::{Lang, t};
use crate::ops::Answer;
use crate::sys::win::{self, SendHwnd};
use windows::Win32::UI::WindowsAndMessaging::WM_CLOSE;

impl App {
    /// `true` = handled.
    pub fn on_key(&mut self, vk: VIRTUAL_KEY) -> bool {
        self.swallow_next_char = false;
        self.cancel_pending_rename();
        let ctrl = win::key_down(VK_CONTROL);
        let shift = win::key_down(VK_SHIFT);
        let hwnd = self.hwnd;
        let handled = match &mut self.mode {
            Mode::History(_) => self.history_list_key(vk),
            Mode::Drives(d) => {
                match vk {
                    VK_UP => d.sel = d.sel.saturating_sub(1),
                    VK_DOWN => d.sel = (d.sel + 1).min(d.items.len().saturating_sub(1)),
                    VK_RETURN => {
                        let s = d.sel;
                        self.choose_drive(s);
                    }
                    VK_DELETE => {
                        let s = d.sel;
                        self.disconnect_popup_item(s);
                    }
                    VK_ESCAPE => self.mode = Mode::Normal,
                    _ => return false,
                }
                true
            }
            Mode::PathEdit { input, .. } => match vk {
                VK_RETURN => {
                    self.commit_path_edit();
                    true
                }
                VK_ESCAPE => {
                    self.mode = Mode::Normal;
                    true
                }
                _ => input.key(vk, ctrl, hwnd),
            },
            Mode::Dialog(d) => match vk {
                VK_RETURN => {
                    self.commit_dialog();
                    true
                }
                VK_ESCAPE => {
                    self.mode = Mode::Normal;
                    true
                }
                _ if d.extra.rows() > 0 && self.dialog_extra_key(vk, shift) => true,
                _ => match &mut self.mode {
                    Mode::Dialog(d) => d.input.as_mut().is_some_and(|i| i.key(vk, ctrl, hwnd)),
                    _ => false,
                },
            },
            Mode::Options { sel, popup: Some(p) } => {
                let n = Lang::ALL.len();
                match vk {
                    VK_UP => *p = p.saturating_sub(1),
                    VK_DOWN => *p = (*p + 1).min(n - 1),
                    VK_SPACE | VK_RETURN => {
                        let lang = Lang::ALL[*p];
                        self.choose_language(lang);
                    }
                    VK_ESCAPE => self.mode = Mode::Options { sel: *sel, popup: None },
                    _ => return false,
                }
                true
            }
            Mode::Options { sel, popup: None } => {
                let n = crate::app::options::ROWS;
                match vk {
                    VK_UP => *sel = sel.saturating_sub(1),
                    VK_DOWN => *sel = (*sel + 1).min(n - 1),
                    VK_HOME => *sel = 0,
                    VK_END => *sel = n - 1,
                    VK_SPACE | VK_RETURN => {
                        let row = *sel;
                        self.activate_option(row);
                    }
                    VK_ESCAPE => self.mode = Mode::Normal,
                    _ => return false,
                }
                true
            }
            Mode::Find => self.find_key(vk, ctrl, shift),
            Mode::Rename(_) => self.rename_key(vk, ctrl),
            Mode::Conflict(_) => match vk {
                VK_RETURN => {
                    self.answer_conflict(Answer::Overwrite);
                    true
                }
                VK_ESCAPE => {
                    self.answer_conflict(Answer::Cancel);
                    true
                }
                _ => false,
            },
            Mode::Normal => self.on_key_normal(vk, ctrl, shift),
        };
        if handled {
            self.invalidate();
        }
        handled
    }

    /// Keys in the file lists (no dialog or popup open).
    fn on_key_normal(&mut self, vk: VIRTUAL_KEY, ctrl: bool, shift: bool) -> bool {
        if !matches!(vk, VK_SHIFT | VK_CONTROL | VK_MENU | VK_LWIN | VK_RWIN) {
            self.bring_cursor_into_view();
        }
        let handled = (self.options.command_line && self.command_line_key(vk, ctrl, shift))
            || self.cursor_key(vk, ctrl, shift)
            || self.mark_key(vk, ctrl)
            || self.command_key(vk, ctrl, shift);
        if handled {
            self.arm_loading_indicator();
        }
        handled
    }

    /// Scrolled away with the mouse? Any key brings the cursor back into view.
    fn bring_cursor_into_view(&mut self) {
        let p = &mut self.panels[self.active];
        let top = p.top;
        p.ensure_visible();
        if p.top != top {
            win::invalidate(self.hwnd);
        }
    }

    /// Moving the cursor (Shift+↑/↓ and Insert also toggle the mark); ends the quick search.
    fn cursor_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, shift: bool) -> bool {
        let p = &mut self.panels[self.active];
        match vk {
            VK_UP | VK_DOWN if shift => {
                p.toggle_mark();
                p.move_cursor(if vk == VK_UP { -1 } else { 1 });
            }
            VK_UP => p.move_cursor(-1),
            VK_DOWN => p.move_cursor(1),
            VK_PRIOR if !ctrl => {
                let d = -p.page();
                p.move_cursor(d);
            }
            VK_NEXT if !ctrl => {
                let d = p.page();
                p.move_cursor(d);
            }
            VK_HOME => p.set_cursor(0),
            VK_END => p.set_cursor(usize::MAX),
            VK_INSERT => {
                p.toggle_mark();
                p.move_cursor(1);
            }
            _ => return false,
        }
        p.search.clear();
        true
    }

    /// Marking by key. Numpad keys like Total Commander; the main row's + and - stay quick
    /// search characters.
    fn mark_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool) -> bool {
        let p = &mut self.panels[self.active];
        match vk {
            VK_A if ctrl => p.mark_all(),
            VK_ADD if ctrl => {
                p.mark_where(true, |_| true);
            }
            VK_SUBTRACT if ctrl => p.clear_marks(),
            VK_ADD | VK_SUBTRACT => {
                self.swallow_next_char = true;
                self.open_dialog(DialogKind::Select { add: vk == VK_ADD });
            }
            VK_MULTIPLY => {
                self.swallow_next_char = true;
                p.invert_file_marks();
            }
            _ => return false,
        }
        true
    }

    /// Switching panels, navigation, quick search editing, sorting and the file operation keys.
    fn command_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, shift: bool) -> bool {
        let a = self.active;
        let p = &mut self.panels[a];
        match vk {
            VK_TAB => {
                p.search.clear();
                self.active ^= 1;
            }
            VK_PRIOR if ctrl => p.go_up(a, &self.sink),
            VK_RETURN => {
                p.search.clear();
                self.open_current(shift);
            }
            VK_NEXT if ctrl => {
                p.search.clear();
                self.enter_current();
            }
            VK_BACK if !p.search.is_empty() => p.search_pop(),
            VK_BACK => p.go_up(a, &self.sink),
            VK_ESCAPE => {
                if !p.search.is_empty() {
                    p.search.clear();
                } else if let Some(job) = &self.job {
                    job.handle.cancel();
                } else {
                    for p in &mut self.panels {
                        p.cancel_sizes();
                    }
                }
            }
            VK_F2 => self.reload_by_hand(),
            VK_R if ctrl => self.reload_by_hand(),
            VK_L if ctrl => self.start_path_edit(a),
            VK_OEM_COMMA if ctrl => self.open_options(),
            VK_F3 if ctrl => self.sort_by(a, SortKey::Name),
            VK_F5 if ctrl => self.sort_by(a, SortKey::Date),
            VK_F6 if ctrl => self.sort_by(a, SortKey::Size),
            VK_F3 => self.view_current(),
            VK_F4 if shift => self.open_dialog(DialogKind::NewFile),
            VK_F4 => self.edit_current(),
            VK_F5 => self.open_dialog(DialogKind::Copy),
            VK_F6 if shift => self.open_dialog(DialogKind::Rename),
            VK_F6 => self.open_dialog(DialogKind::Move),
            VK_F7 => self.open_dialog(DialogKind::MkDir),
            VK_F8 | VK_DELETE => self.open_dialog(DialogKind::Delete { recycle: !shift }),
            _ => return false,
        }
        true
    }

    /// Alt+F1 / Alt+F2: drive popup for the left / right panel; Alt+Shift+Enter: count the
    /// size of every folder; Alt+F5 / Alt+F9: pack / unpack; Alt+F7: file search. Alt alone
    /// switches the key bar to the Alt keys (not AltGr, which comes with Ctrl).
    pub fn on_syskey(&mut self, vk: VIRTUAL_KEY) -> bool {
        if vk == VK_MENU {
            if !self.alt_down && !win::key_down(VK_CONTROL) {
                self.alt_down = true;
                self.invalidate();
            }
            return false;
        }
        if !matches!(self.mode, Mode::Normal | Mode::Drives(_)) {
            return false;
        }
        if matches!(vk, VK_F5 | VK_F9) && matches!(self.mode, Mode::Normal) {
            if vk == VK_F5 {
                self.open_pack()
            } else {
                self.open_unpack()
            }
            self.invalidate();
            return true;
        }
        if vk == VK_RETURN && matches!(self.mode, Mode::Normal) && win::key_down(VK_SHIFT) {
            self.count_all_sizes();
            self.invalidate();
            return true;
        }
        if vk == VK_F7 && matches!(self.mode, Mode::Normal) {
            self.open_find();
            self.invalidate();
            return true;
        }
        let panel = match vk {
            VK_F1 => 0,
            VK_F2 => 1,
            _ => return false,
        };
        self.open_drives(panel);
        self.invalidate();
        true
    }

    /// Alt released (or the window lost the focus): back to the normal key bar.
    pub fn on_alt_up(&mut self) {
        if self.alt_down {
            self.alt_down = false;
            self.invalidate();
        }
    }

    /// A click on a key bar entry does what its key does.
    pub(crate) fn run_hint(&mut self, id: &str) {
        let a = self.active;
        match id {
            "tab" => {
                self.panels[a].search.clear();
                self.active ^= 1;
            }
            "view" => self.view_current(),
            "edit" => self.edit_current(),
            "mark" => {
                let p = &mut self.panels[a];
                p.toggle_mark();
                p.move_cursor(1);
            }
            "select" => self.open_dialog(DialogKind::Select { add: true }),
            "copy" => self.open_dialog(DialogKind::Copy),
            "move" => self.open_dialog(DialogKind::Move),
            "rename" => self.open_dialog(DialogKind::Rename),
            "mkdir" => self.open_dialog(DialogKind::MkDir),
            "delete" => self.open_dialog(DialogKind::Delete { recycle: true }),
            "drive" => self.open_drives(a),
            "drive_left" => self.open_drives(0),
            "drive_right" => self.open_drives(1),
            "path" => self.start_path_edit(a),
            "find" => self.open_find(),
            "pack" => self.open_pack(),
            "unpack" => self.open_unpack(),
            "quit" => SendHwnd::new(self.hwnd).post(WM_CLOSE),
            _ => {}
        }
        self.arm_loading_indicator();
    }

    /// Typed characters: quick search, text inputs, drive letters, conflict answers.
    pub fn on_char(&mut self, c: u16) {
        if std::mem::take(&mut self.swallow_next_char) {
            return;
        }
        if c < 0x20 || c == 0x7f {
            return;
        }
        if matches!(self.mode, Mode::Normal) && self.options.command_line && self.command_line_char(c) {
            self.invalidate();
            return;
        }
        match &mut self.mode {
            Mode::Drives(d) => {
                let hit = d.items.iter().position(|i| match i {
                    PopupItem::Drive(dr) => fold_case(dr.root[0]) == fold_case(c),
                    PopupItem::Host { .. } => false,
                });
                if let Some(i) = hit {
                    self.choose_drive(i);
                }
            }
            Mode::PathEdit { input, .. } => input.insert(c),
            Mode::Dialog(d) => {
                if let Some(i) = d.input.as_mut().filter(|_| d.focus == 0) {
                    i.insert(c);
                }
            }
            Mode::Options { .. } | Mode::History(_) => {}
            Mode::Find => self.find_char(c),
            Mode::Rename(r) => r.input.insert(c),
            Mode::Conflict(_) => {
                // The answer keys are translated (German "K" = keine, English "N" = none).
                let is = |key: &str| t(key).encode_utf16().next().is_some_and(|k| fold_case(k) == fold_case(c));
                if is("conflict.skip_key") {
                    self.answer_conflict(Answer::Skip);
                } else if is("conflict.all_key") {
                    self.answer_conflict(Answer::OverwriteAll);
                } else if is("conflict.none_key") {
                    self.answer_conflict(Answer::SkipAll);
                }
            }
            Mode::Normal => {
                let a = self.active;
                let p = &mut self.panels[a];
                if c == b'\\' as u16 {
                    p.go_root(a, &self.sink);
                    self.arm_loading_indicator();
                } else if c == b' ' as u16 && p.search.is_empty() {
                    self.space();
                } else if !(self.options.alt_search || self.options.command_line) || !p.search.is_empty() {
                    // With the Alt option (or the command line), plain characters only continue a
                    // running search.
                    p.search_push(c);
                }
            }
        }
        self.invalidate();
    }

    /// Alt+character: starts or continues the quick search if the Alt option or the command line
    /// is on.
    /// `false` = not used (Alt+Space must still open the system menu).
    pub fn on_syschar(&mut self, c: u16) -> bool {
        let alt = self.options.alt_search || self.options.command_line;
        if !alt || !matches!(self.mode, Mode::Normal) || c <= 0x20 || c == 0x7f {
            return false;
        }
        self.panels[self.active].search_push(c);
        self.invalidate();
        true
    }
}
