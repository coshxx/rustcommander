//! Where a panel is: a local folder (archives included, see `files::archive`) or a folder on an
//! SFTP server. Code that only works locally asks for `local()` and gets `None` on a server, so
//! a remote path can never reach `std::fs` by accident.

use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

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
            Location::Local(p) => p.parent().map(|p| Location::Local(p.to_path_buf())),
            Location::Remote(r) => r.parent().map(Location::Remote),
        }
    }

    /// The last component, if any.
    pub fn file_name(&self) -> Option<String> {
        match self {
            Location::Local(p) => p.file_name().map(|n| n.to_string_lossy().into_owned()),
            Location::Remote(r) => Some(r.name().to_string()).filter(|n| !n.is_empty()),
        }
    }

    /// Drive root, or `/` on the server.
    pub fn root(&self) -> Location {
        match self {
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
    }
}
