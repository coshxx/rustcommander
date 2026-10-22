//! SFTP: folders on SSH servers. `ssh.exe` (Windows' OpenSSH client) does the SSH part; this
//! module speaks SFTP over its stdin/stdout (`session`, `proto`), keeps one connection per host
//! (`session()`), lists remote folders for the panels (`load`) and reads the host list of
//! `~/.ssh/config` for the drive popup.

pub mod path;
pub mod proto;
mod session;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub use path::RemotePath;
pub use session::{Error, Session};

use crate::files::listing::{Entry, Kind, Listing, kind_from_extension};
use crate::files::location::Location;
use crate::files::names::natural_cmp;

/// Set in the environment of ssh.exe; `rustcommander.exe` started with it is the askpass helper.
pub const ASKPASS_ENV: &str = "RC_ASKPASS";
/// The main window, so the askpass window can sit in front of it.
pub const ASKPASS_OWNER_ENV: &str = "RC_ASKPASS_OWNER";

/// Main window handle (as `isize`), for the askpass window.
static OWNER: AtomicIsize = AtomicIsize::new(0);

pub fn set_owner(hwnd: isize) {
    OWNER.store(hwnd, Ordering::Relaxed);
}

/// One host: `connecting` is held while a connection is being made, so a second user of the
/// same host waits for it instead of opening another; `session` is only locked briefly.
#[derive(Default)]
struct Slot {
    connecting: Mutex<()>,
    session: Mutex<Option<Arc<Session>>>,
}

static POOL: Mutex<Vec<(String, Arc<Slot>)>> = Mutex::new(Vec::new());

fn slot(host: &str) -> Arc<Slot> {
    let mut pool = POOL.lock().unwrap();
    if let Some((_, s)) = pool.iter().find(|(h, _)| h == host) {
        return s.clone();
    }
    let s = Arc::new(Slot::default());
    pool.push((host.to_string(), s.clone()));
    s
}

/// Environment for ssh.exe: passwords and host key questions go to our askpass window.
fn askpass_env() -> Vec<(&'static str, String)> {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    let mut env = vec![
        ("SSH_ASKPASS", exe),
        ("SSH_ASKPASS_REQUIRE", "force".to_string()),
        (ASKPASS_ENV, "1".to_string()),
        (ASKPASS_OWNER_ENV, OWNER.load(Ordering::Relaxed).to_string()),
    ];
    if std::env::var_os("DISPLAY").is_none() {
        // Older OpenSSH builds only use askpass when DISPLAY is set.
        env.push(("DISPLAY", "localhost:0".to_string()));
    }
    env
}

/// The connection to `host`, connecting first if there is none (or it broke). Blocks; never call
/// it on the UI thread.
pub fn session(host: &str) -> Result<Arc<Session>, Error> {
    let slot = slot(host);
    let _connecting = slot.connecting.lock().unwrap();
    if let Some(s) = slot.session.lock().unwrap().as_ref().filter(|s| s.alive()) {
        return Ok(s.clone());
    }
    // The askpass window may need to come to the front.
    crate::sys::askpass::allow_foreground();
    let s = Arc::new(Session::connect(host, &askpass_env())?);
    *slot.session.lock().unwrap() = Some(s.clone());
    Ok(s)
}

/// Hosts with an open connection.
pub fn connected() -> Vec<String> {
    let pool = POOL.lock().unwrap();
    pool.iter()
        .filter(|(_, s)| s.session.try_lock().is_ok_and(|s| s.as_ref().is_some_and(|s| s.alive())))
        .map(|(h, _)| h.clone())
        .collect()
}

/// Close the connection to `host` (requests still running fail).
pub fn disconnect(host: &str) {
    let s = POOL.lock().unwrap().iter().find(|(h, _)| h == host).map(|(_, s)| s.clone());
    if let Some(s) = s.and_then(|s| s.session.lock().unwrap().take()) {
        s.close();
    }
}

/// Close connections unused for `idle`, except to the hosts in `keep`. `true` if any remain.
pub fn close_idle(keep: &[&str], idle: Duration) -> bool {
    let slots: Vec<(String, Arc<Slot>)> = POOL.lock().unwrap().clone();
    let mut remaining = false;
    for (host, slot) in slots {
        let mut guard = slot.session.lock().unwrap();
        let Some(s) = guard.as_ref() else { continue };
        if !s.alive() || (!keep.contains(&host.as_str()) && s.idle_for() >= idle) {
            s.close();
            *guard = None;
        } else {
            remaining = true;
        }
    }
    remaining
}

/// List a remote folder (loader thread). A path to a file lists its folder with the file
/// selected; the home directory (empty path) is resolved first.
pub fn load(path: &RemotePath, cancel: &AtomicBool) -> Result<Listing, String> {
    let start = Instant::now();
    let s = session(&path.host).map_err(|e| e.msg)?;
    let mut dir = path.clone();
    if dir.path.is_empty() {
        dir.path = s.realpath("").map_err(|e| e.msg)?;
    }
    let cancelled = || cancel.load(Ordering::Relaxed);
    let mut select = None;
    let names = match s.read_dir(&dir.path, &cancelled) {
        Ok(n) => n,
        Err(e) => {
            // A file: list its folder instead.
            let is_file = s.stat(&dir.path).is_ok_and(|a| !a.is_dir());
            match dir.parent().filter(|_| is_file) {
                Some(parent) => {
                    select = Some(dir.name().encode_utf16().collect());
                    dir = parent;
                    s.read_dir(&dir.path, &cancelled).map_err(|e| e.msg)?
                }
                None => return Err(e.msg),
            }
        }
    };
    // Links: what they point to decides folder or file; broken ones stay (dimmed).
    let links: Vec<String> = names.iter().filter(|n| n.attrs.is_link()).map(|n| dir.join(&n.name).path).collect();
    let mut targets = s.stat_many(&links).into_iter();
    let mut entries = Vec::with_capacity(names.len() + 1);
    let (mut dirs, mut files, mut bytes) = (0, 0, 0);
    for n in names {
        let mut attrs = n.attrs;
        let mut broken = false;
        if attrs.is_link() {
            match targets.next() {
                Some(Ok(t)) => attrs = t,
                _ => broken = true,
            }
        }
        let name: Box<[u16]> = n.name.encode_utf16().collect();
        let is_dir = attrs.is_dir();
        let size = if is_dir { 0 } else { attrs.size.unwrap_or(0) };
        if is_dir {
            dirs += 1;
        } else {
            files += 1;
            bytes += size;
        }
        entries.push(Entry {
            kind: if is_dir { Kind::Dir } else { kind_from_extension(&name) },
            hidden: broken || n.name.starts_with('.'),
            size,
            mtime: attrs.filetime(),
            marked: false,
            name,
        });
    }
    entries.sort_unstable_by(|a, b| b.is_dir().cmp(&a.is_dir()).then_with(|| natural_cmp(&a.name, &b.name)));
    if dir.parent().is_some() {
        entries.insert(0, Entry::parent());
    }
    Ok(Listing {
        dir: Location::Remote(dir),
        entries,
        select,
        dirs,
        files,
        bytes,
        elapsed: start.elapsed(),
        archive: None,
    })
}

/// `Host` names of `~/.ssh/config` (without patterns like `*` or `!x`), in file order.
pub fn config_hosts() -> Vec<String> {
    let Some(home) = std::env::var_os("USERPROFILE") else { return Vec::new() };
    let file = PathBuf::from(home).join(r".ssh\config");
    std::fs::read_to_string(file).map(|text| parse_hosts(&text)).unwrap_or_default()
}

fn parse_hosts(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((key, rest)) = line.split_once(|c: char| c.is_whitespace() || c == '=') else { continue };
        if !key.eq_ignore_ascii_case("host") {
            continue;
        }
        for h in rest.split_whitespace().map(|h| h.trim_matches('"')) {
            if !h.is_empty() && !h.contains(['*', '?', '!']) && !out.iter().any(|o| o == h) {
                out.push(h.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_from_config() {
        let text = "Host *\n  User x\nHost web web2 !bad\n  HostName 1.2.3.4\n# Host commented\n\
                    host=\"lower\"\nHost web\nMatch host foo\n";
        assert_eq!(parse_hosts(text), ["web", "web2", "lower"]);
    }
}
