//! Modal interactions: drive popup (drives and SFTP hosts), path editing and the small dialogs
//! (copy, move, delete, rename, new directory, new file, mark by mask, disconnect; pack and
//! unpack are in `packing.rs`).

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::app::command_line;
use crate::app::format::{describe, ends_with_separator, parse_location, utf16, valid_name, valid_remote_name};
use crate::app::text_input::TextInput;
use crate::app::{App, Mode, Tone};
use crate::files::folder_kind::{Action, FolderKind};
use crate::files::listing::Kind;
use crate::files::location::Location;
use crate::files::mask::Mask;
use crate::files::names::fold_case;
use crate::i18n::{t, tf};
use crate::ops::Job;
use crate::panel::drive_target;
use crate::sftp::{self, RemotePath};
use crate::sys::win::{self, Drive};

/// One row of the drive popup.
pub(crate) enum PopupItem {
    Drive(Drive),
    /// An SFTP host from `~/.ssh/config` or connected right now.
    Host {
        name: String,
        connected: bool,
    },
}

pub(crate) struct DrivePopup {
    pub(crate) panel: usize,
    pub(crate) items: Vec<PopupItem>,
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
    /// Disconnect from the SFTP host in `sources[0]` although a job uses it.
    Disconnect,
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
            DialogKind::Disconnect => "dialog.disconnect",
        })
    }
}

pub(crate) struct Dialog {
    pub(crate) kind: DialogKind,
    pub(crate) panel: usize,
    pub(crate) sources: Vec<Location>,
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

/// The items of one panel: all local or all on one server.
enum Sources {
    Local(Vec<PathBuf>),
    Remote(Vec<RemotePath>),
}

fn split(sources: Vec<Location>) -> Sources {
    if sources.first().is_some_and(Location::is_remote) {
        Sources::Remote(sources.into_iter().filter_map(|l| l.remote().cloned()).collect())
    } else {
        Sources::Local(sources.into_iter().filter_map(|l| l.local().map(Path::to_path_buf)).collect())
    }
}

impl App {
    /// Open the drive popup for `panel`, preselecting its current drive (or host).
    pub(crate) fn open_drives(&mut self, panel: usize) {
        self.active = panel;
        let mut items: Vec<PopupItem> = win::drives().into_iter().map(PopupItem::Drive).collect();
        let connected = sftp::connected();
        let mut hosts = sftp::config_hosts();
        for h in &connected {
            if !hosts.contains(h) {
                hosts.push(h.clone());
            }
        }
        items.extend(hosts.into_iter().map(|name| {
            let connected = connected.contains(&name);
            PopupItem::Host { name, connected }
        }));
        let dir = &self.panels[panel].dir;
        let cur = dir.local().and_then(|d| d.as_os_str().encode_wide().next());
        let sel = items
            .iter()
            .position(|i| match i {
                PopupItem::Drive(d) => Some(fold_case(d.root[0])) == cur.map(fold_case),
                PopupItem::Host { name, .. } => dir.host() == Some(name.as_str()),
            })
            .unwrap_or(0);
        self.mode = Mode::Drives(DrivePopup { panel, items, sel });
    }

    /// Switch to the drive (or host) at `idx` of the popup (stays put if it is the current drive).
    pub(crate) fn choose_drive(&mut self, idx: usize) {
        if let Mode::Drives(d) = std::mem::replace(&mut self.mode, Mode::Normal) {
            match d.items.get(idx) {
                Some(PopupItem::Drive(drive)) => self.switch_drive(d.panel, drive.root[0] as u8),
                Some(PopupItem::Host { name, .. }) => self.open_host(d.panel, name),
                None => {}
            }
        }
    }

    /// Show `host` in `panel`: the folder the panel was in last there, else the home directory.
    pub(crate) fn open_host(&mut self, panel: usize, host: &str) {
        let p = &self.panels[panel];
        if p.dir.host() == Some(host) {
            return;
        }
        let target = p.host_dirs.get(host).cloned().unwrap_or_else(|| RemotePath::new(host, ""));
        self.navigate(panel, Location::Remote(target), None);
    }

    /// Del (or the ✕) on a connected host in the drive popup.
    pub(crate) fn disconnect_popup_item(&mut self, idx: usize) {
        let Mode::Drives(d) = &self.mode else { return };
        let Some(PopupItem::Host { name, connected: true }) = d.items.get(idx) else { return };
        let (host, panel) = (name.clone(), d.panel);
        if self.job.as_ref().is_some_and(|j| j.host.as_deref() == Some(host.as_str())) {
            self.mode = Mode::Dialog(Dialog {
                kind: DialogKind::Disconnect,
                panel,
                sources: vec![Location::Remote(RemotePath::new(&host, "/"))],
                message: tf("dialog.disconnect_question", &[("host", &host)]),
                input: None,
                extra: Extra::None,
                focus: 0,
                keep_marks: true,
            });
            return;
        }
        self.disconnect(&host);
        self.open_drives(panel);
        if let Mode::Drives(d) = &mut self.mode {
            d.sel = idx.min(d.items.len().saturating_sub(1));
        }
    }

    /// Close the connection to `host` (cancelling a job that uses it); panels showing it go back
    /// to their last local folder.
    pub(crate) fn disconnect(&mut self, host: &str) {
        if let Some(job) = self.job.as_ref().filter(|j| j.host.as_deref() == Some(host)) {
            job.handle.cancel();
        }
        sftp::disconnect(host);
        for i in 0..2 {
            let on_host = self.panels[i].dir.host() == Some(host)
                || self.panels[i].pending.as_ref().is_some_and(|p| p.path.host() == Some(host));
            if on_host {
                let back = self.panels[i].last_local.clone();
                let back = if back.as_os_str().is_empty() { PathBuf::from(r"C:\") } else { back };
                self.navigate(i, back.into(), None);
            }
        }
        self.notify(tf("notice.disconnected", &[("host", &host)]), Tone::Info);
    }

    /// Switch `panel` to drive `d` (`b'C'`): stays if it is on it already, else goes to the other
    /// panel's folder there, the folder it was in last on that drive, or the root.
    pub(crate) fn switch_drive(&mut self, panel: usize, d: u8) {
        match drive_target(&self.panels[panel], &self.panels[panel ^ 1], d.to_ascii_uppercase()) {
            Some((dir, true)) => self.navigate_remembered(panel, dir.into()),
            Some((dir, false)) => self.navigate(panel, dir.into(), None),
            None => {}
        }
    }

    /// Make the path bar of `panel` editable (Ctrl+L, click on the path).
    pub(crate) fn start_path_edit(&mut self, panel: usize) {
        self.active = panel;
        let text = utf16(&self.panels[panel].dir.to_string());
        self.mode = Mode::PathEdit { panel, input: TextInput::new(text) };
    }

    /// Enter in the path bar: go to the typed path (`D:` alone like the drive list).
    pub(crate) fn commit_path_edit(&mut self) {
        let Mode::PathEdit { panel, input } = std::mem::replace(&mut self.mode, Mode::Normal) else { return };
        if let Some(d) = command_line::drive(input.string().trim()) {
            // `start_path_edit` made `panel` the active one.
            self.go_to_drive(d);
        } else if let Some(p) = parse_location(&input.text, &self.panels[panel].dir) {
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
        let action = match kind {
            DialogKind::Copy | DialogKind::Extract => Some(Action::Copy),
            DialogKind::Move => Some(Action::Move),
            DialogKind::Delete { .. } => Some(Action::Delete),
            DialogKind::Rename => Some(Action::Rename),
            DialogKind::MkDir => Some(Action::MkDir),
            DialogKind::NewFile => Some(Action::NewFile),
            DialogKind::Pack => return self.open_pack(),
            DialogKind::Unpack => return self.open_unpack(),
            DialogKind::Select { .. } | DialogKind::Disconnect => None,
        };
        if let Some(action) = action
            && !self.check(a, action)
        {
            return;
        }
        let kind = match kind {
            // Copying out of an archive extracts.
            DialogKind::Copy if self.panels[a].kind.is_archive() => DialogKind::Extract,
            // A server has no recycle bin.
            DialogKind::Delete { .. } if self.panels[a].dir.is_remote() => DialogKind::Delete { recycle: false },
            _ => kind,
        };
        let p = &self.panels[a];
        let (sources, message, input) = match kind {
            DialogKind::Copy | DialogKind::Extract | DialogKind::Move => {
                let sources = p.targets();
                let (msg, input) = transfer_prompt(kind, &sources, &self.default_target());
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
                let msg = tf("dialog.mkdir_in", &[("dir", &p.dir)]);
                (Vec::new(), msg, Some(TextInput::new(Vec::new())))
            }
            DialogKind::NewFile => {
                let msg = tf("dialog.edit_in", &[("dir", &p.dir)]);
                let name = p.current().filter(|e| !e.is_dir()).map(|e| e.name.to_vec()).unwrap_or_default();
                (Vec::new(), msg, Some(TextInput::selected(name)))
            }
            DialogKind::Select { .. } => {
                let msg = t("dialog.mask_hint").to_string();
                (Vec::new(), msg, Some(TextInput::selected(utf16(&self.mask))))
            }
            DialogKind::Pack | DialogKind::Unpack | DialogKind::Disconnect => return,
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
        match d.kind {
            DialogKind::Select { add } => return self.apply_mask(d.panel, input, add),
            DialogKind::Pack | DialogKind::Unpack => return self.commit_archive_dialog(d),
            DialogKind::NewFile => return self.new_file(d.panel, input),
            DialogKind::Disconnect => {
                if let Some(host) = d.sources.first().and_then(Location::host) {
                    self.disconnect(host);
                }
                return;
            }
            _ => {}
        }
        let job = match d.kind {
            DialogKind::Select { .. }
            | DialogKind::NewFile
            | DialogKind::Pack
            | DialogKind::Unpack
            | DialogKind::Disconnect => return,
            DialogKind::Copy | DialogKind::Extract | DialogKind::Move => {
                let Some(dest) = parse_location(&utf16(&input), &dir) else { return };
                let into = ends_with_separator(&input);
                if let Err(r) = FolderKind::of_target(&dest, into).allows(Action::Target) {
                    return self.refuse(r);
                }
                match self.transfer_job(d.kind, d.sources, dest, into) {
                    Some(job) => job,
                    None => return,
                }
            }
            DialogKind::Delete { recycle } => match split(d.sources) {
                Sources::Local(sources) => Job::Delete { sources, recycle },
                Sources::Remote(sources) => Job::RemoteDelete { sources },
            },
            DialogKind::Rename => {
                let from = d.sources[0].clone();
                if from.file_name().as_deref() == Some(input.as_str()) {
                    return;
                }
                let ok = if from.is_remote() { valid_remote_name(&input, false) } else { valid_name(&input, false) };
                if !ok {
                    self.notify(tf("notice.invalid_name", &[("name", &input)]), Tone::Error);
                    return;
                }
                match from {
                    Location::Local(from) => Job::Rename { from, to: input.into() },
                    Location::Remote(from) => Job::RemoteRename { from, to: input },
                }
            }
            DialogKind::MkDir => {
                let ok = if dir.is_remote() { valid_remote_name(&input, true) } else { valid_name(&input, true) };
                if !ok {
                    self.notify(tf("notice.invalid_name", &[("name", &input)]), Tone::Error);
                    return;
                }
                match &dir {
                    Location::Local(dir) => Job::MkDir { path: dir.join(&input) },
                    Location::Remote(dir) => Job::RemoteMkDir { path: dir.resolve(&input) },
                }
            }
        };
        self.start_job(job, d.kind.title(), (!d.keep_marks).then_some(d.panel));
    }

    /// The job for copying / extracting / moving `sources` to `dest`; `None` (with a notice) for
    /// combinations that don't work (yet). `into` = the target was typed with a trailing `\`/`/`.
    fn transfer_job(&mut self, kind: DialogKind, sources: Vec<Location>, dest: Location, into: bool) -> Option<Job> {
        let refuse = |app: &mut App, key: &str| {
            app.notify(t(key).into(), Tone::Info);
            None
        };
        match (split(sources), dest) {
            (Sources::Local(sources), Location::Local(dest)) => Some(match kind {
                DialogKind::Copy => Job::Copy { sources, dest },
                DialogKind::Extract => Job::Extract { sources, dest },
                _ => Job::Move { sources, dest },
            }),
            (Sources::Local(sources), Location::Remote(dest)) => match kind {
                DialogKind::Copy => Some(Job::Upload { sources, dest, into }),
                DialogKind::Move => refuse(self, "notice.move_between"),
                _ => refuse(self, "notice.remote_unsupported"),
            },
            (Sources::Remote(sources), Location::Local(dest)) => match kind {
                DialogKind::Move => refuse(self, "notice.move_between"),
                _ => Some(Job::Download { sources, dest }),
            },
            (Sources::Remote(sources), Location::Remote(dest)) => {
                let same_host = sources.iter().all(|s| s.host == dest.host);
                match kind {
                    DialogKind::Move if same_host => Some(Job::RemoteMove { sources, dest, into }),
                    _ => refuse(self, "notice.remote_to_remote"),
                }
            }
        }
    }

    /// Shift+F4: create `name` in the panel's directory if it doesn't exist, put the cursor on it
    /// and open it in the editor.
    fn new_file(&mut self, panel: usize, name: String) {
        if !valid_name(&name, false) {
            self.notify(tf("notice.invalid_name", &[("name", &name)]), Tone::Error);
            return;
        }
        let Some(dir) = self.panels[panel].dir.local().map(Path::to_path_buf) else { return };
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
        self.navigate(panel, dir.into(), Some(utf16(&name).into()));
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

/// Message and prefilled target (`dest` with a trailing separator) of a copy, extract or move
/// dialog.
pub(crate) fn transfer_prompt(kind: DialogKind, sources: &[Location], dest: &Location) -> (String, TextInput) {
    let sep = if dest.is_remote() { '/' } else { '\\' };
    let mut dest = dest.to_string();
    if !dest.ends_with(sep) {
        dest.push(sep);
    }
    let key = match kind {
        DialogKind::Copy => "dialog.copy_to",
        DialogKind::Extract => "dialog.extract_to",
        _ => "dialog.move_to",
    };
    (tf(key, &[("what", &describe(sources))]), TextInput::new(utf16(&dest)))
}
