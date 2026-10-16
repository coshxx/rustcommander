//! The command line above the key bar (option `command_line`), like Total Commander's: typing
//! goes there, Enter runs it in the active panel's folder. `D:`, `cd <path>` and a bare folder
//! path steer the active panel instead.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows::Win32::UI::Input::KeyboardAndMouse::*;

use crate::app::format::{parse_path, utf16};
use crate::app::text_input::TextInput;
use crate::app::{App, Tone};
use crate::config::history;
use crate::files::listing::Kind;
use crate::i18n::tf;

/// `CREATE_NEW_CONSOLE`: the command gets its own console window.
const CREATE_NEW_CONSOLE: u32 = 0x10;

/// What a line means.
#[derive(Debug, PartialEq)]
enum Parsed {
    /// `D:`: switch to that drive.
    Drive(char),
    /// `cd <path>` (`None` = plain `cd`, which does nothing).
    Cd(Option<String>),
    /// Anything else: run it (or, if it is a folder, go there).
    Other,
}

fn parse(line: &str) -> Parsed {
    let b = line.as_bytes();
    if b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return Parsed::Drive(b[0].to_ascii_uppercase() as char);
    }
    let lower = line.to_ascii_lowercase();
    if lower == "cd" || lower == "chdir" {
        return Parsed::Cd(None);
    }
    // `cd ..`, `cd\`, `cd..` and `cd /d D:\x` like cmd.
    for cmd in ["chdir", "cd"] {
        if let Some(rest) = lower.strip_prefix(cmd)
            && (rest.starts_with([' ', '\\', '.', '/']))
        {
            let mut arg = line[cmd.len()..].trim();
            if arg.len() >= 2 && arg[..2].eq_ignore_ascii_case("/d") {
                arg = arg[2..].trim();
            }
            return Parsed::Cd((!arg.is_empty()).then(|| arg.to_string()));
        }
    }
    Parsed::Other
}

impl App {
    /// Keys for the command line (only when it is shown and no dialog is open); `false` = the
    /// key belongs to the file list. With an empty line Enter, Backspace, Del, Ctrl+A keep their
    /// usual meaning.
    pub(crate) fn command_line_key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, shift: bool) -> bool {
        let has_text = !self.cmdline.text.is_empty();
        match vk {
            VK_RETURN if ctrl => self.insert_current_name(shift),
            VK_RETURN if has_text => self.run_command_line(shift),
            VK_ESCAPE if has_text => {
                self.cmdline = TextInput::new(Vec::new());
                self.history_pos = None;
            }
            VK_UP if ctrl => self.previous_command(),
            VK_DOWN if ctrl => self.open_history_list(),
            VK_LEFT | VK_RIGHT => {
                self.cmdline.key(vk, ctrl, self.hwnd);
            }
            VK_BACK | VK_DELETE if has_text => {
                self.cmdline.key(vk, ctrl, self.hwnd);
            }
            VK_V if ctrl => {
                self.cmdline.key(vk, ctrl, self.hwnd);
            }
            VK_A if ctrl && has_text => {
                self.cmdline.key(vk, ctrl, self.hwnd);
            }
            _ => return false,
        }
        true
    }

    /// A typed character; `false` = not for the command line (Space and `\` on an empty line
    /// mark and go to the root, a running quick search takes plain characters too).
    pub(crate) fn command_line_char(&mut self, c: u16) -> bool {
        let empty = self.cmdline.text.is_empty();
        if !self.panels[self.active].search.is_empty() || (empty && (c == b' ' as u16 || c == b'\\' as u16)) {
            return false;
        }
        self.cmdline.insert(c);
        true
    }

    /// Ctrl+Enter: the name under the cursor (Ctrl+Shift+Enter: its full path) into the line,
    /// quoted if it contains spaces.
    fn insert_current_name(&mut self, full: bool) {
        let p = &self.panels[self.active];
        let Some(e) = p.current().filter(|e| e.kind != Kind::Parent) else { return };
        let mut s =
            if full { p.dir.join(e.os_name()).display().to_string() } else { e.os_name().display().to_string() };
        if s.contains(' ') {
            s = format!("\"{s}\"");
        }
        let input = &mut self.cmdline;
        if input.caret > 0 && input.text[input.caret - 1] != b' ' as u16 {
            input.insert(b' ' as u16);
        }
        for c in utf16(&s) {
            input.insert(c);
        }
        input.insert(b' ' as u16);
    }

    /// Ctrl+↑: the previous command into the line, again for the one before (Ctrl+↓ opens the
    /// whole list, see `history_list.rs`).
    fn previous_command(&mut self) {
        let Some(last) = self.history.len().checked_sub(1) else { return };
        let pos = self.history_pos.map_or(0, |i| (i + 1).min(last));
        self.history_pos = Some(pos);
        self.cmdline = TextInput::new(utf16(&self.history[pos]));
    }

    /// Enter / Shift+Enter: steer the panel or run the line in a console (Shift: it stays open).
    /// On success the line is cleared; on an error the notice says why and the text stays.
    fn run_command_line(&mut self, keep_open: bool) {
        let line = self.cmdline.string().trim().to_string();
        if line.is_empty() {
            return;
        }
        history::push(&mut self.history, &line);
        history::save(&self.history);
        self.history_pos = None;
        let a = self.active;
        let dir = self.panels[a].dir.clone();
        let ok = match parse(&line) {
            Parsed::Drive(d) => self.go_to_drive(d),
            Parsed::Cd(None) => true,
            // Not checked here: on an unreachable network path that would freeze the window. A
            // failed load shows its error like any other.
            Parsed::Cd(Some(arg)) => match parse_path(&utf16(&arg), &dir) {
                Some(p) => {
                    self.navigate(a, p, None);
                    true
                }
                None => {
                    self.notify(tf("notice.no_folder", &[("path", &arg)]), Tone::Error);
                    false
                }
            },
            Parsed::Other if line.trim_start_matches('"').starts_with(r"\\") => {
                match parse_path(&utf16(&line), &dir) {
                    Some(p) => self.navigate(a, p, None),
                    None => self.notify(tf("notice.no_folder", &[("path", &line)]), Tone::Error),
                }
                true
            }
            Parsed::Other => match parse_path(&utf16(&line), &dir).filter(|p| p.is_dir()) {
                Some(p) => {
                    self.navigate(a, p, None);
                    true
                }
                None => self.run_external(&line, keep_open),
            },
        };
        if ok {
            self.cmdline = TextInput::new(Vec::new());
        }
    }

    /// `D:`: like the drive list (see `switch_drive`).
    fn go_to_drive(&mut self, d: char) -> bool {
        let root = PathBuf::from(format!("{d}:\\"));
        if !root.exists() {
            self.notify(tf("notice.no_drive", &[("drive", &format!("{d}:"))]), Tone::Error);
            return false;
        }
        self.switch_drive(self.active, d as u8);
        true
    }

    /// `cmd /c` (or `/k`) in a new console, in the active panel's folder (for an archive: the
    /// folder holding it).
    fn run_external(&mut self, line: &str, keep_open: bool) -> bool {
        let p = &self.panels[self.active];
        let cwd = p.archive.as_deref().and_then(Path::parent).unwrap_or(&p.dir).to_path_buf();
        let flag = if keep_open { "/k" } else { "/c" };
        let spawned = Command::new("cmd.exe")
            .raw_arg(format!("{flag} {line}"))
            .current_dir(&cwd)
            .creation_flags(CREATE_NEW_CONSOLE)
            .spawn();
        match spawned {
            Ok(_) => true,
            Err(e) => {
                self.notify(tf("notice.command_failed", &[("command", &line), ("error", &e)]), Tone::Error);
                false
            }
        }
    }

    /// Click into the command line: put the caret at the nearest character boundary.
    pub(crate) fn click_command_line(&mut self, x: f32) {
        let (w, h) = self.gfx.size_dips(self.hwnd);
        let input_x = self.command_line_layout(w, h).1.left + 6.0;
        let text = &self.cmdline.text;
        let caret = (0..=text.len())
            .min_by(|&a, &b| {
                let da = (self.gfx.caret_x(&text[..a]) + input_x - x).abs();
                let db = (self.gfx.caret_x(&text[..b]) + input_x - x).abs();
                da.total_cmp(&db)
            })
            .unwrap_or(text.len());
        self.cmdline.caret = caret;
        self.cmdline.anchor = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing() {
        assert_eq!(parse("d:"), Parsed::Drive('D'));
        assert_eq!(parse("cd"), Parsed::Cd(None));
        assert_eq!(parse("cd .."), Parsed::Cd(Some("..".into())));
        assert_eq!(parse("cd.."), Parsed::Cd(Some("..".into())));
        assert_eq!(parse(r"cd\"), Parsed::Cd(Some(r"\".into())));
        assert_eq!(parse(r"CD /d E:\Foto Album"), Parsed::Cd(Some(r"E:\Foto Album".into())));
        assert_eq!(parse("cdrecord x"), Parsed::Other);
        assert_eq!(parse("git status"), Parsed::Other);
        assert_eq!(parse("d:x"), Parsed::Other);
    }
}
