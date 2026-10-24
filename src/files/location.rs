//! Where a panel is: a local folder (archives included, see `files::archive`) or a folder on an
//! SFTP server. Code that only works locally asks for `local()` and gets `None` on a server, so
//! a remote path can never reach `std::fs` by accident.

use std::ffi::OsStr;
use std::fmt;
use std::path::{Component, Path, PathBuf, Prefix};

use crate::sftp::RemotePath;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Location {
    Local(PathBuf),
    Remote(RemotePath),
}

impl Default for Location {
    fn default() -> Self {
        Location::Local(PathBuf::new())
    }
}

impl From<PathBuf> for Location {
    fn from(p: PathBuf) -> Self {
        Location::Local(p)
    }
}

impl Location {
    pub fn local(&self) -> Option<&Path> {
        match self {
            Location::Local(p) => Some(p),
            Location::Remote(_) => None,
        }
    }

    pub fn remote(&self) -> Option<&RemotePath> {
        match self {
            Location::Remote(r) => Some(r),
            Location::Local(_) => None,
        }
    }

    pub fn is_remote(&self) -> bool {
        matches!(self, Location::Remote(_))
    }

    /// `\\server`: the list of its shares, only for navigating.
    pub fn is_server(&self) -> bool {
        self.local().is_some_and(|p| server_root(p).is_some())
    }

    /// Nothing loaded yet.
    pub fn is_empty(&self) -> bool {
        matches!(self, Location::Local(p) if p.as_os_str().is_empty())
    }

    pub fn join(&self, name: impl AsRef<OsStr>) -> Location {
        let name = name.as_ref();
        match self {
            Location::Local(p) => Location::Local(p.join(name)),
            Location::Remote(r) => Location::Remote(r.join(&name.to_string_lossy())),
        }
    }

    pub fn parent(&self) -> Option<Location> {
        match self {
            Location::Local(p) if server_root(p).is_some() => None,
            Location::Local(p) => match share_root(p) {
                Some((server, _)) => Some(Location::Local(server_path(server))),
                None => p.parent().map(|p| Location::Local(p.to_path_buf())),
            },
            Location::Remote(r) => r.parent().map(Location::Remote),
        }
    }

    /// The last component, if any.
    pub fn file_name(&self) -> Option<String> {
        match self {
            Location::Local(p) => match share_root(p) {
                Some((_, share)) => Some(share.to_string()),
                None => p.file_name().map(|n| n.to_string_lossy().into_owned()),
            },
            Location::Remote(r) => Some(r.name().to_string()).filter(|n| !n.is_empty()),
        }
    }

    /// Drive root, or `/` on the server.
    pub fn root(&self) -> Location {
        match self {
            Location::Local(p) if server_root(p).is_some() => self.clone(),
            Location::Local(p) => Location::Local(p.ancestors().last().unwrap_or(p).to_path_buf()),
            Location::Remote(r) => Location::Remote(r.root()),
        }
    }

    /// The host if remote.
    pub fn host(&self) -> Option<&str> {
        self.remote().map(|r| r.host.as_str())
    }

    /// The entry of `dir` that `self` is or lies in (`C:\a\b\c` in `C:\a` → `b`).
    pub fn entry_in(&self, dir: &Location) -> Option<String> {
        match (self, dir) {
            (Location::Local(p), Location::Local(d)) if let Some(server) = server_root(d) => match share_of(p) {
                Some((s, share)) if s.eq_ignore_ascii_case(server) => Some(share.to_string()),
                _ => None,
            },
            (Location::Local(p), Location::Local(d)) => {
                p.strip_prefix(d).ok()?.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned())
            }
            (Location::Remote(p), Location::Remote(d)) if p != d && p.starts_with(d) => {
                let rest = p.path.strip_prefix(&d.path)?.trim_start_matches('/');
                rest.split('/').next().map(str::to_string)
            }
            _ => None,
        }
    }
}

/// The server of `\\server` (also `\\server\`); `None` for every other path.
pub fn server_root(p: &Path) -> Option<&str> {
    let rest = p.to_str()?.strip_prefix(r"\\")?;
    let name = rest.strip_suffix('\\').unwrap_or(rest);
    (!name.is_empty() && name != "?" && name != "." && !name.contains(['\\', '/'])).then_some(name)
}

pub fn server_path(server: &str) -> PathBuf {
    PathBuf::from(format!(r"\\{server}"))
}

/// Server and share of a path on a share (`\\server\share\…`).
fn share_of(p: &Path) -> Option<(&str, &str)> {
    match p.components().next()? {
        Component::Prefix(pre) => match pre.kind() {
            Prefix::UNC(server, share) => Some((server.to_str()?, share.to_str()?)),
            _ => None,
        },
        _ => None,
    }
}

/// Server and share of the top of a share (`\\server\share`, also with `\`).
fn share_root(p: &Path) -> Option<(&str, &str)> {
    share_of(p).filter(|_| p.components().all(|c| matches!(c, Component::Prefix(_) | Component::RootDir)))
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Location::Local(p) => write!(f, "{}", p.display()),
            Location::Remote(r) => write!(f, "{r}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_below() {
        let local = |s: &str| Location::Local(s.into());
        let remote = |s: &str| Location::Remote(RemotePath::new("h", s));
        assert_eq!(local(r"C:\a\b\c").entry_in(&local(r"C:\a")).as_deref(), Some("b"));
        assert_eq!(local(r"C:\x").entry_in(&local(r"C:\a")), None);
        assert_eq!(remote("/a/b/c").entry_in(&remote("/a")).as_deref(), Some("b"));
        assert_eq!(remote("/etc").entry_in(&remote("/")).as_deref(), Some("etc"));
        assert_eq!(remote("/a").entry_in(&remote("/a")), None);
        assert_eq!(remote("/a").entry_in(&local(r"C:\a")), None);
        assert_eq!(local(r"\\wsl$\Ubuntu\home").entry_in(&local(r"\\wsl$")).as_deref(), Some("Ubuntu"));
        assert_eq!(local(r"\\WSL$\Ubuntu").entry_in(&local(r"\\wsl$")).as_deref(), Some("Ubuntu"));
        assert_eq!(local(r"\\nas\x").entry_in(&local(r"\\wsl$")), None);
    }

    #[test]
    fn servers() {
        let local = |s: &str| Location::Local(s.into());
        assert!(local(r"\\wsl$").is_server());
        assert!(local(r"\\wsl.localhost\").is_server());
        assert!(!local(r"\\wsl$\Ubuntu").is_server());
        assert!(!local(r"\\?\C:\x").is_server());
        assert!(!local(r"C:\").is_server());
        assert_eq!(local(r"\\wsl$\Ubuntu").parent(), Some(local(r"\\wsl$")));
        assert_eq!(local(r"\\wsl$\Ubuntu\").parent(), Some(local(r"\\wsl$")));
        assert_eq!(local(r"\\wsl$\Ubuntu\home").parent(), Some(local(r"\\wsl$\Ubuntu\")));
        assert_eq!(local(r"\\wsl$").parent(), None);
        assert_eq!(local(r"\\wsl$\Ubuntu").file_name().as_deref(), Some("Ubuntu"));
        assert_eq!(local(r"\\wsl$").root(), local(r"\\wsl$"));
    }
}
