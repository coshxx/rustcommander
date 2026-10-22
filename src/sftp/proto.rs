//! SFTP version 3 (draft-ietf-secsh-filexfer-02) as OpenSSH speaks it: packet types, building
//! requests and reading replies. Pure, no I/O.

pub const INIT: u8 = 1;
pub const VERSION: u8 = 2;
pub const OPEN: u8 = 3;
pub const CLOSE: u8 = 4;
pub const READ: u8 = 5;
pub const WRITE: u8 = 6;
pub const LSTAT: u8 = 7;
pub const SETSTAT: u8 = 9;
pub const OPENDIR: u8 = 11;
pub const READDIR: u8 = 12;
pub const REMOVE: u8 = 13;
pub const MKDIR: u8 = 14;
pub const RMDIR: u8 = 15;
pub const REALPATH: u8 = 16;
pub const STAT: u8 = 17;
pub const RENAME: u8 = 18;

pub const STATUS: u8 = 101;
pub const HANDLE: u8 = 102;
pub const DATA: u8 = 103;
pub const NAME: u8 = 104;
pub const ATTRS: u8 = 105;

pub const FX_OK: u32 = 0;
pub const FX_EOF: u32 = 1;
pub const FX_NO_SUCH_FILE: u32 = 2;

pub const OPEN_READ: u32 = 0x01;
pub const OPEN_WRITE: u32 = 0x02;
pub const OPEN_CREAT: u32 = 0x08;
pub const OPEN_TRUNC: u32 = 0x10;

const ATTR_SIZE: u32 = 0x01;
const ATTR_UIDGID: u32 = 0x02;
const ATTR_PERMISSIONS: u32 = 0x04;
const ATTR_ACMODTIME: u32 = 0x08;
const ATTR_EXTENDED: u32 = 0x8000_0000;

const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFLNK: u32 = 0o120000;

/// Seconds between 1601-01-01 (FILETIME) and 1970-01-01 (Unix time).
const EPOCH_DIFF: u64 = 11_644_473_600;

/// File attributes; fields the server did not send are `None`.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Attrs {
    pub size: Option<u64>,
    pub permissions: Option<u32>,
    /// Unix seconds.
    pub atime: Option<u32>,
    pub mtime: Option<u32>,
}

impl Attrs {
    pub fn is_dir(&self) -> bool {
        self.permissions.is_some_and(|p| p & S_IFMT == S_IFDIR)
    }

    pub fn is_link(&self) -> bool {
        self.permissions.is_some_and(|p| p & S_IFMT == S_IFLNK)
    }

    /// Modification time as FILETIME (100 ns ticks since 1601), 0 if unknown.
    pub fn filetime(&self) -> u64 {
        self.mtime.map_or(0, |s| (s as u64 + EPOCH_DIFF) * 10_000_000)
    }

    /// Only the modification (and access) time, for SETSTAT after an upload.
    pub fn times(atime: u32, mtime: u32) -> Attrs {
        Attrs { atime: Some(atime), mtime: Some(mtime), ..Attrs::default() }
    }
}

/// Builds the payload of a packet.
#[derive(Default)]
pub struct Writer(pub Vec<u8>);

impl Writer {
    pub fn u32(mut self, v: u32) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn u64(mut self, v: u64) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn bytes(mut self, b: &[u8]) -> Self {
        self = self.u32(b.len() as u32);
        self.0.extend_from_slice(b);
        self
    }

    pub fn str(self, s: &str) -> Self {
        self.bytes(s.as_bytes())
    }

    pub fn attrs(mut self, a: &Attrs) -> Self {
        let mut flags = 0;
        if a.size.is_some() {
            flags |= ATTR_SIZE;
        }
        if a.permissions.is_some() {
            flags |= ATTR_PERMISSIONS;
        }
        if a.mtime.is_some() {
            flags |= ATTR_ACMODTIME;
        }
        self = self.u32(flags);
        if let Some(s) = a.size {
            self = self.u64(s);
        }
        if let Some(p) = a.permissions {
            self = self.u32(p);
        }
        if let Some(m) = a.mtime {
            self = self.u32(a.atime.unwrap_or(m)).u32(m);
        }
        self
    }
}

/// A whole packet on the wire: length, type, request id (except for INIT), payload.
pub fn packet(kind: u8, id: Option<u32>, payload: &[u8]) -> Vec<u8> {
    let len = 1 + id.map_or(0, |_| 4) + payload.len();
    let mut out = Vec::with_capacity(4 + len);
    out.extend_from_slice(&(len as u32).to_be_bytes());
    out.push(kind);
    if let Some(id) = id {
        out.extend_from_slice(&id.to_be_bytes());
    }
    out.extend_from_slice(payload);
    out
}

/// Reads the fields of a reply; every read fails on truncated data.
pub struct Reader<'a> {
    data: &'a [u8],
}

pub type Parse<T> = Result<T, &'static str>;

const TRUNCATED: &str = "truncated SFTP packet";

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data }
    }

    fn take(&mut self, n: usize) -> Parse<&'a [u8]> {
        if self.data.len() < n {
            return Err(TRUNCATED);
        }
        let (head, rest) = self.data.split_at(n);
        self.data = rest;
        Ok(head)
    }

    pub fn u32(&mut self) -> Parse<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn u64(&mut self) -> Parse<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn bytes(&mut self) -> Parse<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }

    pub fn string(&mut self) -> Parse<String> {
        Ok(String::from_utf8_lossy(self.bytes()?).into_owned())
    }

    pub fn attrs(&mut self) -> Parse<Attrs> {
        let flags = self.u32()?;
        let mut a = Attrs::default();
        if flags & ATTR_SIZE != 0 {
            a.size = Some(self.u64()?);
        }
        if flags & ATTR_UIDGID != 0 {
            self.take(8)?;
        }
        if flags & ATTR_PERMISSIONS != 0 {
            a.permissions = Some(self.u32()?);
        }
        if flags & ATTR_ACMODTIME != 0 {
            a.atime = Some(self.u32()?);
            a.mtime = Some(self.u32()?);
        }
        if flags & ATTR_EXTENDED != 0 {
            for _ in 0..self.u32()? {
                self.bytes()?;
                self.bytes()?;
            }
        }
        Ok(a)
    }

    pub fn rest(&self) -> &'a [u8] {
        self.data
    }
}

/// One entry of a NAME reply.
pub struct Name {
    pub name: String,
    pub attrs: Attrs,
}

/// The entries of a NAME reply (READDIR, REALPATH).
pub fn names(payload: &[u8]) -> Parse<Vec<Name>> {
    let mut r = Reader::new(payload);
    let n = r.u32()?;
    let mut out = Vec::with_capacity(n.min(4096) as usize);
    for _ in 0..n {
        let name = r.string()?;
        r.bytes()?; // long name (`ls -l` line)
        let attrs = r.attrs()?;
        out.push(Name { name, attrs });
    }
    Ok(out)
}

/// Code and message of a STATUS reply.
pub fn status(payload: &[u8]) -> Parse<(u32, String)> {
    let mut r = Reader::new(payload);
    let code = r.u32()?;
    // Some old servers leave out the message.
    let msg = if r.rest().is_empty() { String::new() } else { r.string()? };
    Ok((code, msg))
}

/// Text for a status code when the server sent no message.
pub fn status_text(code: u32) -> &'static str {
    match code {
        FX_EOF => "End of file",
        FX_NO_SUCH_FILE => "No such file or directory",
        3 => "Permission denied",
        4 => "Failure",
        5 => "Bad message",
        6 => "No connection",
        7 => "Connection lost",
        8 => "Operation not supported",
        _ => "Unknown error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets() {
        assert_eq!(packet(INIT, None, &Writer::default().u32(3).0), [0, 0, 0, 5, 1, 0, 0, 0, 3]);
        let p = packet(OPENDIR, Some(7), &Writer::default().str("/x").0);
        assert_eq!(p, [0, 0, 0, 11, 11, 0, 0, 0, 7, 0, 0, 0, 2, b'/', b'x']);
    }

    #[test]
    fn attrs_round_trip() {
        let a = Attrs { size: Some(1 << 40), permissions: Some(0o100644), atime: Some(5), mtime: Some(1_700_000_000) };
        let w = Writer::default().attrs(&a);
        assert_eq!(Reader::new(&w.0).attrs().unwrap(), a);
        assert!(!a.is_dir() && !a.is_link());
        assert!(Attrs { permissions: Some(0o40755), ..a }.is_dir());
        assert!(Attrs { permissions: Some(0o120777), ..a }.is_link());
    }

    #[test]
    fn name_reply_skips_uid_and_extended() {
        let entry = Writer::default()
            .str("file.txt")
            .str("-rw-r--r-- 1 u g 3 Jan 1 file.txt")
            .u32(ATTR_SIZE | ATTR_UIDGID | ATTR_EXTENDED)
            .u64(3)
            .u32(1000)
            .u32(1000)
            .u32(1)
            .str("k")
            .str("v");
        let mut payload = Writer::default().u32(1).0;
        payload.extend_from_slice(&entry.0);
        let n = names(&payload).unwrap();
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].name, "file.txt");
        assert_eq!(n[0].attrs.size, Some(3));
        assert!(names(&payload[..payload.len() - 1]).is_err());
    }

    #[test]
    fn times() {
        // 2023-11-14 22:13:20 UTC = 133_444_736_000_000_000 in FILETIME ticks.
        let a = Attrs { mtime: Some(1_700_000_000), ..Attrs::default() };
        assert_eq!(a.filetime(), 133_444_736_000_000_000);
        assert_eq!(Attrs::default().filetime(), 0);
    }

    #[test]
    fn status_without_message() {
        assert_eq!(status(&Writer::default().u32(FX_EOF).0).unwrap(), (FX_EOF, String::new()));
        assert_eq!(status(&Writer::default().u32(2).str("nope").str("").0).unwrap(), (2, "nope".into()));
    }
}
