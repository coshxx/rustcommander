//! Alt+F5 (pack into a zip/7z archive) and Alt+F9 (unpack whole archives): their dialogs, which
//! have a row of choices below the target input, and starting the job.

use std::path::{Path, PathBuf};

use windows::Win32::UI::Input::KeyboardAndMouse::*;

use crate::app::dialogs::{Dialog, DialogKind, Extra};
use crate::app::format::{describe, parse_path, utf16};
use crate::app::text_input::TextInput;
use crate::app::{App, Mode, Tone};
use crate::config::PackSettings;
use crate::files::archive::is_archive_name;
use crate::files::folder_kind::{Action, FolderKind};
use crate::files::location::Location;
use crate::i18n::{t, tf};
use crate::ops::Job;
use crate::ops::pack::{Format, Level};

/// `name` with its extension replaced by the one of `format` (if it has `.zip` or `.7z`).
fn with_extension(name: &str, format: Format) -> String {
    let lower = name.to_ascii_lowercase();
    let base = Format::ALL
        .iter()
        .map(|f| format!(".{}", f.id()))
        .find(|ext| lower.ends_with(ext))
        .map_or(name, |ext| &name[..name.len() - ext.len()]);
    format!("{base}.{}", format.id())
}

impl App {
    /// Where packing and unpacking write to by default (`default_target`, but not on a server).
    fn default_target_dir(&self) -> PathBuf {
        match self.default_target() {
            Location::Local(dir) => dir,
            Location::Remote(_) => self.panels[self.active].last_local.clone(),
        }
    }

    /// Alt+F5: pack the marked entries (or the one under the cursor).
    pub(crate) fn open_pack(&mut self) {
        if self.job.is_some() {
            self.notify(t("notice.job_running").into(), Tone::Error);
            return;
        }
        let a = self.active;
        if !self.check(a, Action::Pack) {
            return;
        }
        let p = &self.panels[a];
        let sources = p.local_targets();
        let name = match sources.as_slice() {
            [] => return,
            [one] if one.is_dir() => one.file_name().map(|n| n.to_string_lossy().into_owned()),
            [one] => one.file_stem().map(|n| n.to_string_lossy().into_owned()),
            _ => p.dir.file_name(),
        }
        .unwrap_or_else(|| "archive".into());
        let format = self.pack.format;
        let dest = self.default_target_dir().join(format!("{name}.{}", format.id()));
        let sources: Vec<Location> = sources.into_iter().map(Location::Local).collect();
        let message = tf("dialog.pack_to", &[("what", &describe(&sources))]);
        let input = Some(TextInput::new(utf16(&dest.display().to_string())));
        let extra = Extra::Pack { format, level: self.pack.level };
        self.panels[a].search.clear();
        self.mode = Mode::Dialog(Dialog {
            kind: DialogKind::Pack,
            panel: a,
            sources,
            message,
            input,
            extra,
            focus: 0,
            keep_marks: false,
        });
    }

    /// Alt+F9: unpack the marked archives (or the one under the cursor).
    pub(crate) fn open_unpack(&mut self) {
        if self.job.is_some() {
            self.notify(t("notice.job_running").into(), Tone::Error);
            return;
        }
        let a = self.active;
        if !self.check(a, Action::Unpack) {
            return;
        }
        let p = &self.panels[a];
        let targets = p.local_targets();
        if targets.is_empty() {
            return;
        }
        let is_archive =
            |s: &&PathBuf| s.is_file() && s.file_name().is_some_and(|n| is_archive_name(&utf16(&n.to_string_lossy())));
        let sources: Vec<Location> = targets.iter().filter(is_archive).cloned().map(Location::Local).collect();
        if sources.is_empty() {
            self.notify(t("notice.no_archives").into(), Tone::Info);
            return;
        }
        let mut dest = self.default_target_dir().display().to_string();
        if !dest.ends_with('\\') {
            dest.push('\\');
        }
        let message = tf("dialog.unpack_to", &[("what", &describe(&sources))]);
        let extra = Extra::Unpack { subdir: self.pack.subdir };
        self.panels[a].search.clear();
        self.mode = Mode::Dialog(Dialog {
            kind: DialogKind::Unpack,
            panel: a,
            sources,
            message,
            input: Some(TextInput::new(utf16(&dest))),
            extra,
            focus: 0,
            keep_marks: false,
        });
    }

    /// Keys for the choice rows of a dialog: Tab / Shift+Tab move between the input and the
    /// rows, ←/→ pick, Space toggles. `false` = not handled here.
    pub(crate) fn dialog_extra_key(&mut self, vk: VIRTUAL_KEY, shift: bool) -> bool {
        let Mode::Dialog(d) = &mut self.mode else { return false };
        let rows = d.extra.rows();
        if rows == 0 {
            return false;
        }
        match vk {
            VK_TAB => {
                d.focus = if shift { (d.focus + rows) % (rows + 1) } else { (d.focus + 1) % (rows + 1) };
                true
            }
            VK_LEFT | VK_RIGHT | VK_SPACE if d.focus > 0 => {
                let step = if vk == VK_LEFT { -1 } else { 1 };
                let wrap = vk == VK_SPACE;
                let row = d.focus - 1;
                d.step_choice(row, step, wrap);
                true
            }
            // The rows take no other keys; the input does.
            _ => d.focus > 0,
        }
    }

    /// Click into a dialog: on the input focuses it, on a choice picks it.
    pub(crate) fn dialog_click(&mut self, x: f32, y: f32) {
        let (w, h) = self.gfx.size_dips(self.hwnd);
        let Mode::Dialog(d) = &self.mode else { return };
        let l = self.dialog_layout(d, w, h);
        let inside = |r: &crate::sys::gfx::Rect| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
        if l.input.as_ref().is_some_and(inside) {
            if let Mode::Dialog(d) = &mut self.mode {
                d.focus = 0;
            }
            return;
        }
        let Some(row) = l.rows.iter().position(inside) else { return };
        let choice = match &d.extra {
            Extra::Unpack { .. } => None,
            extra => self.segment_rects(l.rows[row], &extra.labels(row)).iter().position(inside),
        };
        if let Mode::Dialog(d) = &mut self.mode {
            d.focus = row + 1;
            match choice {
                Some(i) => d.set_choice(row, i),
                None => d.step_choice(row, 1, true),
            }
        }
    }

    /// Enter in the pack or unpack dialog: remember the choices and start the job.
    pub(crate) fn commit_archive_dialog(&mut self, d: Dialog) {
        let p = &self.panels[d.panel];
        let dir = p.dir.local().unwrap_or(&p.last_local).to_path_buf();
        let input = d.input.as_ref().map(|i| i.string().trim().to_string()).unwrap_or_default();
        let Some(dest) = parse_path(&utf16(&input), &dir) else { return };
        let sources: Vec<PathBuf> = d.sources.iter().filter_map(|l| l.local().map(Path::to_path_buf)).collect();
        // Pack names a new archive file, unpack the folder to write into.
        let into = matches!(d.extra, Extra::Unpack { .. });
        if let Err(r) = FolderKind::of_target(&Location::Local(dest.clone()), into).allows(Action::Target) {
            return self.refuse(r);
        }
        let job = match d.extra {
            Extra::Pack { format, level } => {
                if dest.is_dir() || input.ends_with(['\\', '/']) {
                    self.notify(tf("notice.is_folder", &[("name", &dest.display())]), Tone::Error);
                    return;
                }
                self.pack = PackSettings { format, level, ..self.pack };
                Job::Pack { sources, dest, format, level }
            }
            Extra::Unpack { subdir } => {
                self.pack.subdir = subdir;
                Job::Unpack { archives: sources, dest, subdir }
            }
            Extra::None => return,
        };
        self.config(self.hwnd).save();
        self.start_job(job, d.kind.title(), Some(d.panel));
    }
}

impl Extra {
    /// Number of choice rows below the input.
    pub(crate) fn rows(&self) -> usize {
        match self {
            Extra::None => 0,
            Extra::Pack { .. } => 2,
            Extra::Unpack { .. } => 1,
        }
    }

    /// Label in front of row `row`.
    pub(crate) fn row_label(&self, row: usize) -> &'static str {
        match (self, row) {
            (Extra::Pack { .. }, 0) => t("dialog.format"),
            (Extra::Pack { .. }, _) => t("dialog.level"),
            _ => t("dialog.subdir"),
        }
    }

    /// The choices of row `row` (pack dialog).
    pub(crate) fn labels(&self, row: usize) -> Vec<&'static str> {
        match (self, row) {
            (Extra::Pack { .. }, 0) => Format::ALL.iter().map(|f| f.id()).collect(),
            (Extra::Pack { .. }, _) => {
                Level::ALL.iter().map(|l| crate::i18n::lookup(&format!("pack.{}", l.id())).unwrap_or("?")).collect()
            }
            _ => Vec::new(),
        }
    }

    /// Index of the chosen entry in row `row` (for the unpack checkbox: 1 = on).
    pub(crate) fn chosen(&self, row: usize) -> usize {
        match (self, row) {
            (Extra::Pack { format, .. }, 0) => Format::ALL.iter().position(|f| f == format).unwrap_or(0),
            (Extra::Pack { level, .. }, _) => Level::ALL.iter().position(|l| l == level).unwrap_or(0),
            (Extra::Unpack { subdir }, _) => *subdir as usize,
            (Extra::None, _) => 0,
        }
    }
}

impl Dialog {
    /// Pick entry `i` of row `row`; a new format also changes the target's extension.
    fn set_choice(&mut self, row: usize, i: usize) {
        match (&mut self.extra, row) {
            (Extra::Pack { format, .. }, 0) => {
                let Some(&f) = Format::ALL.get(i) else { return };
                *format = f;
                if let Some(input) = &mut self.input {
                    let path = input.string();
                    let name_at = path.rfind(['\\', '/']).map_or(0, |i| i + 1);
                    let (dir, name) = path.split_at(name_at);
                    if !name.is_empty() {
                        *input = TextInput::new(utf16(&format!("{dir}{}", with_extension(name, f))));
                    }
                }
            }
            (Extra::Pack { level, .. }, _) => {
                if let Some(&l) = Level::ALL.get(i) {
                    *level = l;
                }
            }
            (Extra::Unpack { subdir }, _) => *subdir = i == 1,
            (Extra::None, _) => {}
        }
    }

    /// Move the choice of row `row` by `step` (`wrap`: past the end starts over).
    fn step_choice(&mut self, row: usize, step: isize, wrap: bool) {
        let n = match &self.extra {
            Extra::Unpack { .. } => 2,
            extra => extra.labels(row).len(),
        } as isize;
        let cur = self.extra.chosen(row) as isize;
        let next = if wrap { (cur + step).rem_euclid(n) } else { (cur + step).clamp(0, n - 1) };
        self.set_choice(row, next as usize);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_follows_format() {
        assert_eq!(with_extension("fotos.zip", Format::SevenZip), "fotos.7z");
        assert_eq!(with_extension("fotos.7Z", Format::Zip), "fotos.zip");
        assert_eq!(with_extension("fotos", Format::Zip), "fotos.zip");
        assert_eq!(with_extension("a.tar", Format::Zip), "a.tar.zip");
    }
}
