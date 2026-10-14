//! Modal interactions: drive popup, path editing and the small dialogs
//! (copy, move, delete, rename, new directory, mark by mask).

use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;

use crate::app::format::{describe, parse_path, utf16, valid_name};
use crate::app::text_input::TextInput;
use crate::app::{App, Mode, Tone};
use crate::files::listing::Kind;
use crate::files::mask::Mask;
use crate::files::names::fold_case;
use crate::ops::Job;
use crate::sys::win::{self, Drive};

pub(crate) struct DrivePopup {
    pub(crate) panel: usize,
    pub(crate) drives: Vec<Drive>,
    pub(crate) sel: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DialogKind {
    Copy,
    Move,
    Delete {
        recycle: bool,
    },
    Rename,
    MkDir,
    /// Mark (`add`) or unmark entries by file mask.
    Select {
        add: bool,
    },
}

impl DialogKind {
    pub(crate) fn title(self) -> &'static str {
        match self {
            DialogKind::Copy => "Kopieren",
            DialogKind::Move => "Verschieben",
            DialogKind::Delete { recycle: true } => "In den Papierkorb",
            DialogKind::Delete { recycle: false } => "Endgültig löschen",
            DialogKind::Rename => "Umbenennen",
            DialogKind::MkDir => "Neuer Ordner",
            DialogKind::Select { add: true } => "Markieren",
            DialogKind::Select { add: false } => "Markierung aufheben",
        }
    }
}

pub(crate) struct Dialog {
    pub(crate) kind: DialogKind,
    pub(crate) panel: usize,
    pub(crate) sources: Vec<PathBuf>,
    pub(crate) message: String,
    pub(crate) input: Option<TextInput>,
}

impl App {
    pub(crate) fn open_drives(&mut self, panel: usize) {
        self.active = panel;
        let drives = win::drives();
        let cur = self.panels[panel].dir.as_os_str().encode_wide().next();
        let sel = drives.iter().position(|d| Some(fold_case(d.root[0])) == cur.map(fold_case)).unwrap_or(0);
        self.mode = Mode::Drives(DrivePopup { panel, drives, sel });
    }

    pub(crate) fn choose_drive(&mut self, idx: usize) {
        if let Mode::Drives(d) = std::mem::replace(&mut self.mode, Mode::Normal)
            && let Some(drive) = d.drives.get(idx)
        {
            let root = String::from_utf16_lossy(&drive.root[..3]);
            // Same drive: stay in the current directory.
            if !self.panels[d.panel].dir.starts_with(&root) {
                self.navigate(d.panel, PathBuf::from(root), None);
            }
        }
    }

    pub(crate) fn start_path_edit(&mut self, panel: usize) {
        self.active = panel;
        let text = utf16(&self.panels[panel].dir.display().to_string());
        self.mode = Mode::PathEdit { panel, input: TextInput::new(text) };
    }

    pub(crate) fn commit_path_edit(&mut self) {
        if let Mode::PathEdit { panel, input } = std::mem::replace(&mut self.mode, Mode::Normal)
            && let Some(p) = parse_path(&input.text, &self.panels[panel].dir)
        {
            self.navigate(panel, p, None);
        }
    }

    pub(crate) fn open_dialog(&mut self, kind: DialogKind) {
        if self.job.is_some() && !matches!(kind, DialogKind::Select { .. }) {
            self.notify("Es läuft bereits ein Vorgang".into(), Tone::Error);
            return;
        }
        let a = self.active;
        let p = &self.panels[a];
        let (sources, message, input) = match kind {
            DialogKind::Copy | DialogKind::Move => {
                let sources = p.targets();
                let mut dest = self.panels[a ^ 1].dir.display().to_string();
                if !dest.ends_with('\\') {
                    dest.push('\\');
                }
                let verb = if kind == DialogKind::Copy { "kopieren" } else { "verschieben" };
                let msg = format!("{} {verb} nach:", describe(&sources));
                (sources, msg, Some(TextInput::new(utf16(&dest))))
            }
            DialogKind::Delete { recycle } => {
                let sources = p.targets();
                let msg = if recycle {
                    format!("{} in den Papierkorb verschieben?", describe(&sources))
                } else {
                    format!("{} endgültig löschen? Das lässt sich nicht rückgängig machen.", describe(&sources))
                };
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
                let msg = format!("„{}“ umbenennen in:", String::from_utf16_lossy(&name));
                (vec![p.dir.join(e.os_name())], msg, Some(TextInput { text: name, caret, all: false }))
            }
            DialogKind::MkDir => {
                let msg = format!("Neuer Ordner in {}:", p.dir.display());
                (Vec::new(), msg, Some(TextInput::new(Vec::new())))
            }
            DialogKind::Select { .. } => {
                let msg = "Maske, z. B. *.bmp;*.png · Ordner: \\ am Ende · | schließt aus".to_string();
                (Vec::new(), msg, Some(TextInput::selected(utf16(&self.mask))))
            }
        };
        if sources.is_empty() && !matches!(kind, DialogKind::MkDir | DialogKind::Select { .. }) {
            return;
        }
        self.panels[a].search.clear();
        self.mode = Mode::Dialog(Dialog { kind, panel: a, sources, message, input });
    }

    pub(crate) fn commit_dialog(&mut self) {
        let Mode::Dialog(d) = std::mem::replace(&mut self.mode, Mode::Normal) else { return };
        let dir = self.panels[d.panel].dir.clone();
        let input = d.input.as_ref().map(|i| i.string().trim().to_string()).unwrap_or_default();
        if let DialogKind::Select { add } = d.kind {
            self.apply_mask(d.panel, input, add);
            return;
        }
        let job = match d.kind {
            DialogKind::Select { .. } => return,
            DialogKind::Copy | DialogKind::Move => {
                let Some(dest) = parse_path(&utf16(&input), &dir) else { return };
                if d.kind == DialogKind::Copy {
                    Job::Copy { sources: d.sources, dest }
                } else {
                    Job::Move { sources: d.sources, dest }
                }
            }
            DialogKind::Delete { recycle } => Job::Delete { sources: d.sources, recycle },
            DialogKind::Rename => {
                let from = d.sources[0].clone();
                if from.file_name().is_some_and(|n| n == input.as_str()) {
                    return;
                }
                if !valid_name(&input, false) {
                    self.notify(format!("Ungültiger Name: „{input}“"), Tone::Error);
                    return;
                }
                Job::Rename { from, to: input.into() }
            }
            DialogKind::MkDir => {
                if !valid_name(&input, true) {
                    self.notify(format!("Ungültiger Name: „{input}“"), Tone::Error);
                    return;
                }
                Job::MkDir { path: dir.join(&input) }
            }
        };
        self.start_job(job, d.kind.title(), d.panel);
    }

    fn apply_mask(&mut self, panel: usize, input: String, add: bool) {
        let mask = Mask::parse(&input);
        let n = self.panels[panel].mark_where(add, |e| mask.matches(&e.name, e.is_dir()));
        if n == 0 {
            self.notify(format!("Keine Treffer für „{input}“"), Tone::Info);
        }
        if !input.is_empty() {
            self.mask = input;
        }
    }
}
