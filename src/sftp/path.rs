//! Remote paths: `sftp://[user@]host[:port]/path`. The path part is POSIX (`/`-separated,
//! UTF-8 on the wire); an empty path means the home directory.

use std::fmt;

const SCHEME: &str = "sftp://";

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct RemotePath {
    /// `[user@]host[:port]` as typed; also the key of the connection.
    pub host: String,
    /// Absolute and normalized (`/`, `/home/x`), or empty for the home directory.
    pub path: String,
}

impl RemotePath {
    pub fn new(host: &str, path: &str) -> RemotePath {
        RemotePath { host: host.to_string(), path: path.to_string() }
    }

    /// `sftp://…` (scheme in any case, `\` taken as `/`); `None` for anything else.
    pub fn parse(s: &str) -> Option<RemotePath> {
        let s = s.trim().trim_matches('"');
        if s.len() < SCHEME.len() || !s[..SCHEME.len()].eq_ignore_ascii_case(SCHEME) {
            return None;
        }
        let rest = s[SCHEME.len()..].replace('\\', "/");
        let (host, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest.as_str(), ""),
        };
        if host.is_empty() {
            return None;
        }
        let path = if path.is_empty() { String::new() } else { normalize("/", path) };
        Some(RemotePath::new(host, &path))
    }

    /// `input` relative to this folder (`..`, `sub/dir`), or absolute (`/etc`) on the same host.
    pub fn resolve(&self, input: &str) -> RemotePath {
        let input = input.replace('\\', "/");
        let base = if input.starts_with('/') { "/" } else { &self.path };
        RemotePath::new(&self.host, &normalize(base, &input))
    }

    pub fn join(&self, name: &str) -> RemotePath {
        let path = match self.path.as_str() {
            "" => name.to_string(),
            "/" => format!("/{name}"),
            p => format!("{p}/{name}"),
        };
        RemotePath::new(&self.host, &path)
    }

    /// The folder above; `None` at `/` (and for the unresolved home directory).
    pub fn parent(&self) -> Option<RemotePath> {
        let i = self.path.rfind('/')?;
        if self.path == "/" {
            return None;
        }
        Some(RemotePath::new(&self.host, if i == 0 { "/" } else { &self.path[..i] }))
    }

    /// Last component (`""` at `/`).
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or("")
    }

    /// The host's root folder (`sftp://host/`).
    pub fn root(&self) -> RemotePath {
        RemotePath::new(&self.host, "/")
    }

    /// Whether `self` is `other` or lies below it (same host).
    pub fn starts_with(&self, other: &RemotePath) -> bool {
        self.host == other.host
            && (self.path == other.path
                || other.path == "/"
                || self.path.strip_prefix(&other.path).is_some_and(|r| r.starts_with('/')))
    }
}

impl fmt::Display for RemotePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{SCHEME}{}{}", self.host, self.path)
    }
}

/// `rel` applied to the absolute `base`; `.` and `..` resolved, never above `/`.
fn normalize(base: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = base.split('/').filter(|p| !p.is_empty()).collect();
    for p in rel.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    format!("/{}", parts.join("/"))
}

/// `[user@]host[:port]` → (`[user@]host` for ssh, port). `[v6]:port` keeps the brackets off.
pub fn split_port(host: &str) -> (String, Option<u16>) {
    let (user, rest) = match host.rfind('@') {
        Some(i) => (&host[..=i], &host[i + 1..]),
        None => ("", host),
    };
    if let Some(v6) = rest.strip_prefix('[')
        && let Some((addr, after)) = v6.split_once(']')
    {
        return (format!("{user}{addr}"), after.strip_prefix(':').and_then(|p| p.parse().ok()));
    }
    match rest.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => match p.parse() {
            Ok(port) => (format!("{user}{h}"), Some(port)),
            Err(_) => (host.to_string(), None),
        },
        _ => (host.to_string(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing() {
        assert_eq!(RemotePath::parse("sftp://u@h/home/u/"), Some(RemotePath::new("u@h", "/home/u")));
        assert_eq!(RemotePath::parse("SFTP://h"), Some(RemotePath::new("h", "")));
        assert_eq!(RemotePath::parse(r"sftp://h:2222\var\.\log\..\www"), Some(RemotePath::new("h:2222", "/var/www")));
        assert_eq!(RemotePath::parse("sftp:///x"), None);
        assert_eq!(RemotePath::parse(r"C:\x"), None);
        assert_eq!(RemotePath::parse("sftp://h/../.."), Some(RemotePath::new("h", "/")));
    }

    #[test]
    fn navigating() {
        let p = RemotePath::new("h", "/home/u");
        assert_eq!(p.join("a b").path, "/home/u/a b");
        assert_eq!(p.root().join("etc").path, "/etc");
        assert_eq!(p.parent(), Some(RemotePath::new("h", "/home")));
        assert_eq!(RemotePath::new("h", "/home").parent(), Some(RemotePath::new("h", "/")));
        assert_eq!(p.root().parent(), None);
        assert_eq!(p.name(), "u");
        assert_eq!(p.resolve("../x/./y").path, "/home/x/y");
        assert_eq!(p.resolve("/etc").path, "/etc");
        assert_eq!(p.resolve(r"sub\dir").path, "/home/u/sub/dir");
        assert_eq!(p.to_string(), "sftp://h/home/u");
    }

    #[test]
    fn inside() {
        let p = RemotePath::new("h", "/home/u");
        assert!(p.join("x").starts_with(&p));
        assert!(p.starts_with(&p));
        assert!(p.starts_with(&p.root()));
        assert!(!RemotePath::new("h", "/home/user").starts_with(&p));
        assert!(!RemotePath::new("g", "/home/u").starts_with(&p));
    }

    #[test]
    fn ports() {
        assert_eq!(split_port("u@h:2222"), ("u@h".into(), Some(2222)));
        assert_eq!(split_port("h"), ("h".into(), None));
        assert_eq!(split_port("[::1]:22"), ("::1".into(), Some(22)));
        assert_eq!(split_port("u@[fe80::1]"), ("u@fe80::1".into(), None));
        assert_eq!(split_port("::1"), ("::1".into(), None));
    }
}
