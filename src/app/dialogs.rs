//! Modal interactions: drive popup, path editing and the small dialogs
//! (copy, move, delete, rename, new directory, new file, mark by mask; pack and unpack are in
//! `packing.rs`).

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::app::command_line;
use crate::app::format::{describe, parse_path, utf16, valid_name};
use crate::app::text_input::TextInput;
use crate::app::{App, Mode, Tone};
use crate::files::listing::Kind;
use crate::files::mask::Mask;
use crate::files::names::fold_case;
use crate::i18n::{t, tf};
use crate::ops::Job;
use crate::panel::drive_target;
use crate::sys::win::{self, Drive};

pub(crate) struct DrivePopup {
    pub(crate) panel: usize,
    pub(crate) drives: Vec<Drive>,
    pub(crate) sel: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DialogKind {
    Copy,
    /// Copy out of an archive.
    Extract,
    Move,
    Delete {
        recycle: bool,
    },
    Rename,
    MkDir,
    /// Shift+F4: create a file (unless it exists) and open it in the editor.
    NewFile,
    /// Mark (`add`) or unmark entries by file mask.
    Select {
        add: bool,
    },
    /// Alt+F5: pack into a new archive.
    Pack,
    /// Alt+F9: unpack whole archives.
    Unpack,
}

impl DialogKind {
    pub(crate) fn title(self) -> &'static str {
        t(match self {
            DialogKind::Copy => "dialog.copy",
            DialogKind::Extract => "dialog.extract",
            DialogKind::Move => "dialog.move",
            DialogKind::Delete { recycle: true } => "dialog.recycle",
            DialogKind::Delete { recycle: false } => "dialog.delete",
            DialogKind::Rename => "dialog.rename",
            DialogKind::MkDir => "dialog.mkdir",
            DialogKind::NewFile => "dialog.edit",
            DialogKind::Select { add: true } => "dialog.mark",
            DialogKind::Select { add: false } => "dialog.unmark",
            DialogKind::Pack => "dialog.pack",
            DialogKind::Unpack => "dialog.unpack",
        })
    }
}

pub(crate) struct Dialog {
    pub(crate) kind: DialogKind,
    pub(crate) panel: usize,
    pub(crate) sources: Vec<PathBuf>,
    pub(crate) message: String,
    pub(crate) input: Option<TextInput>,
    /// Choice rows below the input (pack and unpack dialog).
    pub(crate) extra: Extra,
    /// 0 = the input, 1.. = a choice row (Tab moves).
    pub(crate) focus: usize,
    /// Files dropped from another program: `panel`'s marks have nothing to do with them.
    pub(crate) keep_marks: bool,
}

/// What a dialog shows below its input.
pub(crate) enum Extra {
    None,
    /// Format and compression level.
    Pack {
        format: crate::ops::pack::Format,
        level: crate::ops::pack::Level,
    },
    /// "Each archive into its own folder".
    Unpack {
        subdir: bool,
    },
}

impl App {
    /// Open the drive popup for `panel`, preselecting its current drive.
    pub(crate) fn open_drives(&mut self, panel: usize) {
        self.active = panel;
        let drives = win::drives();
        let cur = self.panels[panel].dir.as_os_str().encode_wide().next();
        let sel = drives.iter().position(|d| Some(fold_case(d.root[0])) == cur.map(fold_case)).unwrap_or(0);
        self.mode = Mode::Drives(DrivePopup { panel, drives, sel });
    }

    /// Switch to the drive at `idx` of the popup (stays put if it is the current drive).
    pub(crate) fn choose_drive(&mut self, idx: usize) {
        if let Mode::Drives(d) = std::mem::replace(&mut self.mode, Mode::Normal)
            && let Some(drive) = d.drives.get(idx)
        {
            self.switch_drive(d.panel, drive.root[0] as u8);
        }
    }

    /// Switch `panel` to drive `d` (`b'C'`): stays if it is on it already, else goes to the other
    /// panel's folder there, the folder it was in last on that drive, or the root.
    pub(crate) fn switch_drive(&mut self, panel: usize, d: u8) {
        match drive_target(&self.panels[panel], &self.panels[panel ^ 1], d.to_ascii_uppercase()) {
            Some((dir, true)) => self.navigate_remembered(panel, dir),
            Some((dir, false)) => self.navigate(panel, dir, None),
            None => {}
        }
    }

    /// Make the path bar of `panel` editable (Ctrl+L, click on the path).
    pub(crate) fn start_path_edit(&mut self, panel: usize) {
        self.active = panel;
        let text = utf16(&self.panels[panel].dir.display().to_string());
        self.mode = Mode::PathEdit { panel, input: TextInput::new(text) };
    }

    /// Enter in the path bar: go to the typed path (`D:` alone like the drive list).
    pub(crate) fn commit_path_edit(&mut self) {
        let Mode::PathEdit { panel, input } = std::mem::replace(&mut self.mode, Mode::Normal) else { return };
        if let Some(d) = command_line::drive(input.string().trim()) {
            // `start_path_edit` made `panel` the active one.
            self.go_to_drive(d);
        } else if let Some(p) = parse_path(&input.text, &self.panels[panel].dir) {
            self.navigate(panel, p, None);
        }
    }

    /// Open a dialog for the marked entries (or the one under the cursor).
    pub(crate) fn open_dialog(&mut self, kind: DialogKind) {
        if self.job.is_some() && !matches!(kind, DialogKind::Select { .. }) {
            self.notify(t("notice.job_running").into(), Tone::Error);
            return;
        }
        let a = self.active;
        // Archives are read-only: copying out of one extracts, everything that writes is refused.
        let kind = match kind {
            DialogKind::Copy if self.panels[a].archive.is_some() => DialogKind::Extract,
            DialogKind::Move
            | DialogKind::Delete { .. }
            | DialogKind::Rename
            | DialogKind::MkDir
            | DialogKind::NewFile
                if self.panels[a].archive.is_some() =>
            {
                self.notify(t("notice.read_only").into(), Tone::Info);
                return;
            }
            DialogKind::Pack => return self.open_pack(),
            DialogKind::Unpack => return self.open_unpack(),
            _ => kind,
        };
        let p = &self.panels[a];
        let (sources, message, input) = match kind {
            DialogKind::Copy | DialogKind::Extract | DialogKind::Move => {
                let sources = p.targets();
                let (msg, input) = transfer_prompt(kind, &sources, &self.panels[a ^ 1].dir);
                (sources, msg, Some(input))
            }
            DialogKind::Delete { recycle } => {
                let sources = p.targets();
                let key = if recycle { "dialog.recycle_question" } else { "dialog.delete_question" };
                let msg = tf(key, &[("what", &describe(&sources))]);
                (sources, msg, None)
            }
            DialogKind::Rename => {
                let Some(e) = p.current().filter(|e| e.kind != Kind::Parent) else { return };
                let name = e.name.to_vec();
                // Caret before the extension, like Explorer.
                let caret = match name.iter().rposition(|&c| c == b'.' as u16) {
                    Some(dot) if dot > 0 && !e.is_dir() => dot,
                    _ => name.len(),
                };
                let msg = tf("dialog.rename_to", &[("name", &String::from_utf16_lossy(&name))]);
                (vec![p.dir.join(e.os_name())], msg, Some(TextInput { text: name, caret, anchor: None }))
            }
            DialogKind::MkDir => {
                let msg = tf("dialog.mkdir_in", &[("dir", &p.dir.display())]);
                (Vec::new(), msg, Some(TextInput::new(Vec::new())))
            }
            DialogKind::NewFile => {
                let msg = tf("dialog.edit_in", &[("dir", &p.dir.display())]);
                let name = p.current().filter(|e| !e.is_dir()).map(|e| e.name.to_vec()).unwrap_or_default();
                (Vec::new(), msg, Some(TextInput::selected(name)))
            }
            DialogKind::Select { .. } => {
                let msg = t("dialog.mask_hint").to_string();
                (Vec::new(), msg, Some(TextInput::selected(utf16(&self.mask))))
            }
            DialogKind::Pack | DialogKind::Unpack => return,
        };
        if sources.is_empty() && !matches!(kind, DialogKind::MkDir | DialogKind::NewFile | DialogKind::Select { .. }) {
            return;
        }
        self.panels[a].search.clear();
        self.mode = Mode::Dialog(Dialog {
            kind,
            panel: a,
            sources,
            message,
            input,
            extra: Extra::None,
            focus: 0,
            keep_marks: false,
        });
    }

    /// Enter in a dialog: start the job (or apply the mask).
    pub(crate) fn commit_dialog(&mut self) {
        let Mode::Dialog(d) = std::mem::replace(&mut self.mode, Mode::Normal) else { return };
        let dir = self.panels[d.panel].dir.clone();
        let input = d.input.as_ref().map(|i| i.string().trim().to_string()).unwrap_or_default();
        if let DialogKind::Select { add } = d.kind {
            self.apply_mask(d.panel, input, add);
            return;
        }
        if matches!(d.kind, DialogKind::Pack | DialogKind::Unpack) {
            self.commit_archive_dialog(d);
            return;
        }
        if d.kind == DialogKind::NewFile {
            self.new_file(d.panel, input);
            return;
        }
        let job = match d.kind {
            DialogKind::Select { .. } | DialogKind::NewFile | DialogKind::Pack | DialogKind::Unpack => return,
            DialogKind::Copy | DialogKind::Extract | DialogKind::Move => {
                let Some(dest) = parse_path(&utf16(&input), &dir) else { return };
                if self.panels.iter().any(|p| p.archive.as_ref().is_some_and(|a| dest.starts_with(a))) {
                    self.notify(t("notice.read_only").into(), Tone::Info);
                    return;
                }
                match d.kind {
                    DialogKind::Copy => Job::Copy { sources: d.sources, dest },
                    DialogKind::Extract => Job::Extract { sources: d.sources, dest },
                    _ => Job::Move { sources: d.sources, dest },
                }
            }
            DialogKind::Delete { recycle } => Job::Delete { sources: d.sources, recycle },
            DialogKind::Rename => {
                let from = d.sources[0].clone();
                if from.file_name().is_some_and(|n| n == input.as_str()) {
                    return;
                }
                if !valid_name(&input, false) {
                    self.notify(tf("notice.invalid_name", &[("name", &input)]), Tone::Error);
                    return;
                }
                Job::Rename { from, to: input.into() }
            }
            DialogKind::MkDir => {
                if !valid_name(&input, true) {
                    self.notify(tf("notice.invalid_name", &[("name", &input)]), Tone::Error);
                    return;
                }
                Job::MkDir { path: dir.join(&input) }
            }
        };
        self.start_job(job, d.kind.title(), (!d.keep_marks).then_some(d.panel));
    }

    /// Shift+F4: create `name` in the panel's directory if it doesn't exist, put the cursor on it
    /// and open it in the editor.
    fn new_file(&mut self, panel: usize, name: String) {
        if !valid_name(&name, false) {
            self.notify(tf("notice.invalid_name", &[("name", &name)]), Tone::Error);
            return;
        }
        let dir = self.panels[panel].dir.clone();
        let path = dir.join(&name);
        if !path.exists()
            && let Err(e) = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)
        {
            self.notify(tf("notice.cannot_create", &[("name", &name), ("error", &e)]), Tone::Error);
            return;
        }
        if path.is_dir() {
            self.notify(tf("notice.is_folder", &[("name", &name)]), Tone::Error);
            return;
        }
        self.navigate(panel, dir, Some(utf16(&name).into()));
        self.edit(&path);
    }

    /// Mark (`add`) or unmark all entries of `panel` that match `input`.
    fn apply_mask(&mut self, panel: usize, input: String, add: bool) {
        let mask = Mask::parse(&input);
        let n = self.panels[panel].mark_where(add, |e| mask.matches(&e.name, e.is_dir()));
        if n == 0 {
            self.notify(tf("notice.no_matches", &[("mask", &input)]), Tone::Info);
        }
        if !input.is_empty() {
            self.mask = input;
        }
    }
}

/// Message and prefilled target (`dest` with a trailing backslash) of a copy, extract or move
/// dialog.
pub(crate) fn transfer_prompt(kind: DialogKind, sources: &[PathBuf], dest: &Path) -> (String, TextInput) {
    let mut dest = dest.display().to_string();
    if !dest.ends_with('\\') {
        dest.push('\\');
    }
    let key = match kind {
        DialogKind::Copy => "dialog.copy_to",
        DialogKind::Extract => "dialog.extract_to",
        _ => "dialog.move_to",
    };
    (tf(key, &[("what", &describe(sources))]), TextInput::new(utf16(&dest)))
}
