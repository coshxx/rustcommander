//! What sort of folder a location is (local folder, archive, server list, SFTP folder) and which
//! actions it allows. The one place that knows why something can't be done here: callers ask
//! `allows` and show the refusal's notice (or, for gestures like dragging, just don't start).

use std::path::{Path, PathBuf};

use crate::files::archive;
use crate::files::location::Location;

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum FolderKind {
    #[default]
    Local,
    /// Inside this archive file; read-only.
    Archive(PathBuf),
    /// `\\server`: the list of its shares, only for navigating.
    Servers,
    /// A folder on an SFTP server.
    Remote,
}

/// A command that a folder kind may refuse. Commands that work everywhere (view, open a file,
/// mark by mask) have none.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// F5; inside an archive it extracts.
    Copy,
    Move,
    Delete,
    /// Shift+F6.
    Rename,
    /// A slow second click on a name (gesture).
    InlineRename,
    MkDir,
    NewFile,
    Edit,
    /// Into an archive file under the cursor (Enter, Ctrl+PgDn).
    EnterArchive,
    Pack,
    Unpack,
    ContextMenu,
    /// Dragging entries out of the panel (gesture).
    DragOut,
    /// Files dragged onto the panel (gesture).
    DropInto,
    CountSizes,
    Find,
    RunCommand,
    /// Writing into the folder: the target of a copy, move, extract, pack or unpack.
    Target,
}

/// Why an action isn't possible; each has its notice.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    ReadOnly,
    PackInArchive,
    UnpackInArchive,
    ArchiveInArchive,
    NoContextMenu,
    ServerView,
    RemoteUnsupported,
}

impl Refusal {
    /// The i18n key of the notice.
    pub fn notice(self) -> &'static str {
        match self {
            Refusal::ReadOnly => "notice.read_only",
            Refusal::PackInArchive => "notice.pack_in_archive",
            Refusal::UnpackInArchive => "notice.unpack_in_archive",
            Refusal::ArchiveInArchive => "notice.archive_in_archive",
            Refusal::NoContextMenu => "notice.no_context_menu",
            Refusal::ServerView => "notice.server_view",
            Refusal::RemoteUnsupported => "notice.remote_unsupported",
        }
    }
}

impl FolderKind {
    /// The kind of a loaded folder; `archive` = the archive file it lies in (from the listing).
    pub fn of_folder(dir: &Location, archive: Option<PathBuf>) -> FolderKind {
        match dir {
            Location::Remote(_) => FolderKind::Remote,
            Location::Local(_) if dir.is_server() => FolderKind::Servers,
            Location::Local(_) => archive.map_or(FolderKind::Local, FolderKind::Archive),
        }
    }

    /// The kind of the folder that writing to a typed target `dest` goes into. `into` = `dest`
    /// is that folder itself (typed with a trailing separator); otherwise `dest` may name a file,
    /// so an existing archive file as the target is a file to replace, not a folder. Looks at the
    /// disk to find archives (`archive::split`).
    pub fn of_target(dest: &Location, into: bool) -> FolderKind {
        let archive = dest
            .local()
            .and_then(archive::split)
            .map(|(file, _)| file)
            .filter(|file| into || dest.local() != Some(file.as_path()));
        FolderKind::of_folder(dest, archive)
    }

    pub fn archive(&self) -> Option<&Path> {
        match self {
            FolderKind::Archive(file) => Some(file),
            _ => None,
        }
    }

    pub fn is_archive(&self) -> bool {
        matches!(self, FolderKind::Archive(_))
    }

    /// The local folder that searching and commands work in: `dir`, or inside an archive the
    /// folder holding it; `None` on an SFTP server.
    pub fn working_folder<'a>(&'a self, dir: &'a Location) -> Option<&'a Path> {
        let dir = dir.local()?;
        Some(self.archive().and_then(Path::parent).unwrap_or(dir))
    }

    /// Whether `action` is possible in a folder of this kind.
    pub fn allows(&self, action: Action) -> Result<(), Refusal> {
        use Action::*;
        match (self, action) {
            (FolderKind::Local, _) => Ok(()),
            (FolderKind::Archive(_), Copy | CountSizes | Find | RunCommand) => Ok(()),
            (FolderKind::Archive(_), Pack) => Err(Refusal::PackInArchive),
            (FolderKind::Archive(_), Unpack) => Err(Refusal::UnpackInArchive),
            (FolderKind::Archive(_), EnterArchive) => Err(Refusal::ArchiveInArchive),
            (FolderKind::Archive(_), ContextMenu) => Err(Refusal::NoContextMenu),
            (FolderKind::Archive(_), _) => Err(Refusal::ReadOnly),
            (FolderKind::Servers, CountSizes | Find | RunCommand) => Ok(()),
            (FolderKind::Servers, _) => Err(Refusal::ServerView),
            (FolderKind::Remote, Copy | Move | Delete | Rename | InlineRename | MkDir | Target) => Ok(()),
            (FolderKind::Remote, _) => Err(Refusal::RemoteUnsupported),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sftp::RemotePath;

    #[test]
    fn rules() {
        use Action::*;
        let archive = FolderKind::Archive(r"C:\a.zip".into());
        let ok = Ok(());
        let (read_only, servers, remote) =
            (Err(Refusal::ReadOnly), Err(Refusal::ServerView), Err(Refusal::RemoteUnsupported));
        #[rustfmt::skip]
        let table = [
            //               Local Archive                            Servers  Remote
            (Copy,           [ok, ok,                                 servers, ok]),
            (Move,           [ok, read_only,                          servers, ok]),
            (Delete,         [ok, read_only,                          servers, ok]),
            (Rename,         [ok, read_only,                          servers, ok]),
            (InlineRename,   [ok, read_only,                          servers, ok]),
            (MkDir,          [ok, read_only,                          servers, ok]),
            (NewFile,        [ok, read_only,                          servers, remote]),
            (Edit,           [ok, read_only,                          servers, remote]),
            (EnterArchive,   [ok, Err(Refusal::ArchiveInArchive),     servers, remote]),
            (Pack,           [ok, Err(Refusal::PackInArchive),        servers, remote]),
            (Unpack,         [ok, Err(Refusal::UnpackInArchive),      servers, remote]),
            (ContextMenu,    [ok, Err(Refusal::NoContextMenu),        servers, remote]),
            (DragOut,        [ok, read_only,                          servers, remote]),
            (DropInto,       [ok, read_only,                          servers, remote]),
            (CountSizes,     [ok, ok,                                 ok,      remote]),
            (Find,           [ok, ok,                                 ok,      remote]),
            (RunCommand,     [ok, ok,                                 ok,      remote]),
            (Target,         [ok, read_only,                          servers, ok]),
        ];
        let kinds = [FolderKind::Local, archive, FolderKind::Servers, FolderKind::Remote];
        for (action, expected) in table {
            for (kind, want) in kinds.iter().zip(expected) {
                assert_eq!(kind.allows(action), want, "{action:?} in {kind:?}");
            }
        }
    }

    #[test]
    fn folders() {
        let local = |s: &str| Location::Local(s.into());
        assert_eq!(FolderKind::of_folder(&local(r"C:\x"), None), FolderKind::Local);
        assert_eq!(FolderKind::of_folder(&local(r"\\wsl$"), None), FolderKind::Servers);
        assert_eq!(FolderKind::of_folder(&local(r"\\wsl$\Ubuntu"), None), FolderKind::Local);
        let zip = FolderKind::of_folder(&local(r"C:\dl\a.zip\sub"), Some(r"C:\dl\a.zip".into()));
        assert_eq!(zip, FolderKind::Archive(r"C:\dl\a.zip".into()));
        assert_eq!(zip.working_folder(&local(r"C:\dl\a.zip\sub")), Some(Path::new(r"C:\dl")));
        let remote = Location::Remote(RemotePath::new("h", "/a"));
        assert_eq!(FolderKind::of_folder(&remote, None), FolderKind::Remote);
        assert_eq!(FolderKind::Remote.working_folder(&remote), None);
    }

    #[test]
    fn targets() {
        let dir = std::env::temp_dir().join(format!("rc-folder-kind-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let zip = dir.join("a.zip");
        std::fs::write(&zip, b"").unwrap();
        let at = |p: PathBuf, into| FolderKind::of_target(&Location::Local(p), into);
        let archive = FolderKind::Archive(zip.clone());
        // Inside an archive, whether or not a panel shows it.
        assert_eq!(at(zip.join("sub"), false), archive);
        assert_eq!(at(zip.join("sub"), true), archive);
        // The archive itself: into it with a trailing separator, else a file to replace.
        assert_eq!(at(zip.clone(), true), archive);
        assert_eq!(at(zip.clone(), false), FolderKind::Local);
        // A new archive name is no archive yet.
        assert_eq!(at(dir.join("new.zip"), false), FolderKind::Local);
        assert_eq!(at(dir.clone(), true), FolderKind::Local);
        assert_eq!(at(r"\\wsl$".into(), true), FolderKind::Servers);
        let remote = Location::Remote(RemotePath::new("h", "/a"));
        assert_eq!(FolderKind::of_target(&remote, true), FolderKind::Remote);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
