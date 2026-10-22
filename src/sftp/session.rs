//! One SFTP connection: `ssh.exe -s host sftp` as a hidden child process, requests written to
//! its stdin, replies read from its stdout by a reader thread and routed to the waiting caller
//! by request id. Any thread may send requests; several can be in flight (transfers keep many).

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::path::split_port;
use super::proto::{self, Attrs, Name, Writer};

/// `CREATE_NO_WINDOW`: ssh.exe gets no console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Bytes per READ / WRITE request (every server takes 32 KB).
const CHUNK: u32 = 32 * 1024;
/// Requests kept in flight during a transfer.
const IN_FLIGHT: usize = 64;
/// Larger replies are treated as a broken stream.
const MAX_PACKET: usize = 1 << 24;
/// So much of ssh's error output is kept for the error message.
const STDERR_KEEP: usize = 4096;

/// A failed request: the SFTP status code (0 if the connection itself failed) and the text.
#[derive(Debug, Clone)]
pub struct Error {
    pub code: u32,
    pub msg: String,
}

impl Error {
    fn new(msg: impl Into<String>) -> Error {
        Error { code: 0, msg: msg.into() }
    }

    pub fn not_found(&self) -> bool {
        self.code == proto::FX_NO_SUCH_FILE
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

struct Reply {
    kind: u8,
    payload: Vec<u8>,
}

/// State shared with the reader thread.
struct Shared {
    waiting: Mutex<HashMap<u32, Sender<Reply>>>,
    closed: AtomicBool,
    /// The tail of ssh's error output (why the connection failed or ended).
    stderr: Arc<Mutex<String>>,
}

pub struct Session {
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    shared: Arc<Shared>,
    next_id: AtomicU32,
    last_used: Mutex<Instant>,
}

/// Windows' own OpenSSH client, else whatever `ssh.exe` is on the PATH.
fn ssh_exe() -> PathBuf {
    let system = std::env::var_os("SystemRoot").map(|r| PathBuf::from(r).join(r"System32\OpenSSH\ssh.exe"));
    system.filter(|p| p.is_file()).unwrap_or_else(|| "ssh.exe".into())
}

/// Read one packet: (type, payload after the type byte).
fn read_packet(r: &mut impl Read) -> std::io::Result<(u8, Vec<u8>)> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 || len > MAX_PACKET {
        return Err(std::io::Error::other("invalid SFTP packet length"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    let kind = body[0];
    body.remove(0);
    Ok((kind, body))
}

/// The last lines ssh printed, without its noise about warnings.
fn stderr_message(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let last = lines.iter().rev().find(|l| !l.starts_with("Warning:")).or(lines.last())?;
    Some(last.trim_end_matches('.').to_string())
}

impl Session {
    /// Start ssh and do the SFTP handshake. Blocks until connected (passwords and host keys are
    /// asked by `ssh.exe` through the askpass window meanwhile).
    pub fn connect(host: &str, askpass: &[(&str, String)]) -> Result<Session> {
        if host.starts_with('-') || host.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(Error::new(format!("invalid host name: {host}")));
        }
        let (dest, port) = split_port(host);
        let mut cmd = Command::new(ssh_exe());
        // No X11/agent forwarding, no port forwardings from the config: just the subsystem.
        cmd.args(["-s", "-x", "-a", "-o", "ClearAllForwardings=yes", "-o", "ConnectTimeout=20"]);
        if let Some(p) = port {
            cmd.args(["-p", &p.to_string()]);
        }
        cmd.args(["--", &dest, "sftp"]);
        for (k, v) in askpass {
            cmd.env(k, v);
        }
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).creation_flags(CREATE_NO_WINDOW);
        let mut child = cmd.spawn().map_err(|e| Error::new(format!("ssh.exe: {e}")))?;
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let stderr_pipe = child.stderr.take().unwrap();

        let stderr = Arc::new(Mutex::new(String::new()));
        let err_text = stderr.clone();
        let stderr_thread = std::thread::spawn(move || {
            let mut pipe = stderr_pipe;
            let mut buf = [0u8; 1024];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let mut s = err_text.lock().unwrap();
                s.push_str(&String::from_utf8_lossy(&buf[..n]));
                if s.len() > STDERR_KEEP {
                    let cut = s.len() - STDERR_KEEP;
                    let cut = (cut..s.len()).find(|&i| s.is_char_boundary(i)).unwrap_or(s.len());
                    s.drain(..cut);
                }
            }
        });

        let fail = |child: &mut Child, stderr_thread: std::thread::JoinHandle<()>, what: String| {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stderr_thread.join();
            Error::new(stderr_message(&stderr.lock().unwrap()).unwrap_or(what))
        };
        let init = proto::packet(proto::INIT, None, &Writer::default().u32(3).0);
        if let Err(e) = stdin.write_all(&init).and_then(|_| stdin.flush()) {
            return Err(fail(&mut child, stderr_thread, e.to_string()));
        }
        match read_packet(&mut stdout) {
            Ok((proto::VERSION, _)) => {}
            Ok((kind, _)) => return Err(fail(&mut child, stderr_thread, format!("unexpected SFTP reply {kind}"))),
            Err(_) => return Err(fail(&mut child, stderr_thread, "connection closed".into())),
        }

        let shared = Arc::new(Shared { waiting: Mutex::new(HashMap::new()), closed: AtomicBool::new(false), stderr });
        let reader = shared.clone();
        std::thread::spawn(move || read_replies(stdout, &reader));
        Ok(Session {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            shared,
            next_id: AtomicU32::new(1),
            last_used: Mutex::new(Instant::now()),
        })
    }

    pub fn alive(&self) -> bool {
        !self.shared.closed.load(Ordering::Relaxed)
    }

    pub fn idle_for(&self) -> std::time::Duration {
        self.last_used.lock().unwrap().elapsed()
    }

    /// End the connection (pending requests fail).
    pub fn close(&self) {
        self.shared.closed.store(true, Ordering::Relaxed);
        let mut child = self.child.lock().unwrap();
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Why requests fail once the connection is gone.
    fn lost(&self) -> Error {
        let reason = stderr_message(&self.shared.stderr.lock().unwrap());
        Error::new(reason.unwrap_or_else(|| crate::i18n::t("sftp.lost").to_string()))
    }

    fn send(&self, kind: u8, payload: &[u8]) -> Result<Receiver<Reply>> {
        if !self.alive() {
            return Err(self.lost());
        }
        *self.last_used.lock().unwrap() = Instant::now();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = channel();
        self.shared.waiting.lock().unwrap().insert(id, tx);
        let packet = proto::packet(kind, Some(id), payload);
        let written = {
            let mut stdin = self.stdin.lock().unwrap();
            stdin.write_all(&packet).and_then(|_| stdin.flush())
        };
        if written.is_err() {
            self.shared.waiting.lock().unwrap().remove(&id);
            return Err(self.lost());
        }
        Ok(rx)
    }

    fn wait(&self, rx: Receiver<Reply>) -> Result<Reply> {
        rx.recv().map_err(|_| self.lost())
    }

    fn call(&self, kind: u8, payload: Writer) -> Result<Reply> {
        let rx = self.send(kind, &payload.0)?;
        self.wait(rx)
    }

    /// `Ok` for a STATUS reply with code OK.
    fn expect_ok(r: Reply) -> Result<()> {
        match r.kind {
            proto::STATUS => match status_error(&r.payload) {
                Some(e) => Err(e),
                None => Ok(()),
            },
            k => Err(Error::new(format!("unexpected SFTP reply {k}"))),
        }
    }

    /// The payload of a reply of type `kind`; a STATUS becomes the error.
    fn expect(r: Reply, kind: u8) -> Result<Vec<u8>> {
        if r.kind == kind {
            return Ok(r.payload);
        }
        if r.kind == proto::STATUS {
            return Err(status_error(&r.payload).unwrap_or_else(|| Error::new("unexpected SFTP reply")));
        }
        Err(Error::new(format!("unexpected SFTP reply {}", r.kind)))
    }

    /// Absolute form of `path` (`""` = the home directory).
    pub fn realpath(&self, path: &str) -> Result<String> {
        let p = if path.is_empty() { "." } else { path };
        let r = self.call(proto::REALPATH, Writer::default().str(p))?;
        let names = proto::names(&Self::expect(r, proto::NAME)?).map_err(Error::new)?;
        names.into_iter().next().map(|n| n.name).ok_or_else(|| Error::new("empty REALPATH reply"))
    }

    /// Attributes, following links.
    pub fn stat(&self, path: &str) -> Result<Attrs> {
        let r = self.call(proto::STAT, Writer::default().str(path))?;
        proto::Reader::new(&Self::expect(r, proto::ATTRS)?).attrs().map_err(Error::new)
    }

    /// Attributes of the link itself; `None` if nothing is there.
    pub fn lstat(&self, path: &str) -> Result<Option<Attrs>> {
        let r = self.call(proto::LSTAT, Writer::default().str(path))?;
        match Self::expect(r, proto::ATTRS) {
            Ok(p) => proto::Reader::new(&p).attrs().map(Some).map_err(Error::new),
            Err(e) if e.not_found() => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// `stat` of many paths at once (all requests in flight together).
    pub fn stat_many(&self, paths: &[String]) -> Vec<Result<Attrs>> {
        let pending: Vec<_> = paths.iter().map(|p| self.send(proto::STAT, &Writer::default().str(p).0)).collect();
        pending
            .into_iter()
            .map(|rx| {
                let r = self.wait(rx?)?;
                proto::Reader::new(&Self::expect(r, proto::ATTRS)?).attrs().map_err(Error::new)
            })
            .collect()
    }

    /// All entries of a directory (without `.` and `..`); `cancelled` is asked between batches.
    pub fn read_dir(&self, path: &str, cancelled: &dyn Fn() -> bool) -> Result<Vec<Name>> {
        let r = self.call(proto::OPENDIR, Writer::default().str(path))?;
        let handle = Self::expect(r, proto::HANDLE)?;
        let handle = proto::Reader::new(&handle).bytes().map_err(Error::new)?.to_vec();
        let mut out = Vec::new();
        let result = loop {
            if cancelled() {
                break Err(Error::new("cancelled"));
            }
            let r = match self.call(proto::READDIR, Writer::default().bytes(&handle)) {
                Ok(r) => r,
                Err(e) => break Err(e),
            };
            if r.kind == proto::STATUS && proto::status(&r.payload).is_ok_and(|(c, _)| c == proto::FX_EOF) {
                break Ok(());
            }
            match Self::expect(r, proto::NAME).and_then(|p| proto::names(&p).map_err(Error::new)) {
                Ok(names) => out.extend(names.into_iter().filter(|n| n.name != "." && n.name != "..")),
                Err(e) => break Err(e),
            }
        };
        let _ = self.close_handle(&handle);
        result.map(|_| out)
    }

    fn open(&self, path: &str, flags: u32) -> Result<Vec<u8>> {
        let r = self.call(proto::OPEN, Writer::default().str(path).u32(flags).attrs(&Attrs::default()))?;
        let handle = Self::expect(r, proto::HANDLE)?;
        Ok(proto::Reader::new(&handle).bytes().map_err(Error::new)?.to_vec())
    }

    fn close_handle(&self, handle: &[u8]) -> Result<()> {
        Self::expect_ok(self.call(proto::CLOSE, Writer::default().bytes(handle))?)
    }

    /// Read the remote file `path` and pass its data in order to `sink`, which returns `false`
    /// to stop (cancelled or a local write error).
    pub fn download(&self, path: &str, sink: &mut dyn FnMut(&[u8]) -> bool) -> Result<bool> {
        let handle = self.open(path, proto::OPEN_READ)?;
        let result = self.read_all(&handle, sink);
        let closed = self.close_handle(&handle);
        let complete = result?;
        closed?;
        Ok(complete)
    }

    /// `Ok(true)` = read up to the end, `Ok(false)` = `sink` stopped.
    fn read_all(&self, handle: &[u8], sink: &mut dyn FnMut(&[u8]) -> bool) -> Result<bool> {
        let read = |offset: u64, len: u32| {
            self.send(proto::READ, &Writer::default().bytes(handle).u64(offset).u32(len).0).map(|rx| (offset, len, rx))
        };
        let mut queue = VecDeque::new();
        let mut next = 0u64;
        let mut eof = false;
        loop {
            while !eof && queue.len() < IN_FLIGHT {
                queue.push_back(read(next, CHUNK)?);
                next += CHUNK as u64;
            }
            let Some((mut offset, mut len, rx)) = queue.pop_front() else { return Ok(true) };
            let mut reply = self.wait(rx)?;
            // A short read that isn't the end: fetch the rest of this block before going on.
            loop {
                if reply.kind == proto::STATUS {
                    match proto::status(&reply.payload) {
                        Ok((proto::FX_EOF, _)) => {
                            eof = true;
                            break;
                        }
                        _ => return Err(status_error(&reply.payload).unwrap_or_else(|| Error::new("read failed"))),
                    }
                }
                let payload = Self::expect(reply, proto::DATA)?;
                let data = proto::Reader::new(&payload).bytes().map_err(Error::new)?;
                if !sink(data) {
                    return Ok(false);
                }
                let got = data.len() as u32;
                if got == 0 || got >= len {
                    break;
                }
                offset += got as u64;
                len -= got;
                reply = self.wait(read(offset, len)?.2)?;
            }
        }
    }

    /// Create (or truncate) the remote file `path` and write what `source` delivers until it
    /// returns an empty block; `source` returns `None` to stop (cancelled or a read error).
    pub fn upload(&self, path: &str, source: &mut dyn FnMut(&mut [u8]) -> Option<usize>) -> Result<bool> {
        let handle = self.open(path, proto::OPEN_WRITE | proto::OPEN_CREAT | proto::OPEN_TRUNC)?;
        let result = self.write_all(&handle, source);
        let closed = self.close_handle(&handle);
        let complete = result?;
        closed?;
        Ok(complete)
    }

    fn write_all(&self, handle: &[u8], source: &mut dyn FnMut(&mut [u8]) -> Option<usize>) -> Result<bool> {
        let mut queue = VecDeque::new();
        let mut offset = 0u64;
        let mut buf = vec![0u8; CHUNK as usize];
        loop {
            if queue.len() >= IN_FLIGHT {
                Self::expect_ok(self.wait(queue.pop_front().unwrap())?)?;
            }
            let Some(n) = source(&mut buf) else { return Ok(false) };
            if n == 0 {
                break;
            }
            let w = Writer::default().bytes(handle).u64(offset).bytes(&buf[..n]);
            queue.push_back(self.send(proto::WRITE, &w.0)?);
            offset += n as u64;
        }
        for rx in queue {
            Self::expect_ok(self.wait(rx)?)?;
        }
        Ok(true)
    }

    pub fn set_times(&self, path: &str, atime: u32, mtime: u32) -> Result<()> {
        Self::expect_ok(self.call(proto::SETSTAT, Writer::default().str(path).attrs(&Attrs::times(atime, mtime)))?)
    }

    pub fn remove(&self, path: &str) -> Result<()> {
        Self::expect_ok(self.call(proto::REMOVE, Writer::default().str(path))?)
    }

    pub fn rmdir(&self, path: &str) -> Result<()> {
        Self::expect_ok(self.call(proto::RMDIR, Writer::default().str(path))?)
    }

    pub fn mkdir(&self, path: &str) -> Result<()> {
        Self::expect_ok(self.call(proto::MKDIR, Writer::default().str(path).attrs(&Attrs::default()))?)
    }

    /// Fails if `to` exists (SFTP v3 semantics).
    pub fn rename(&self, from: &str, to: &str) -> Result<()> {
        Self::expect_ok(self.call(proto::RENAME, Writer::default().str(from).str(to))?)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}

/// The error of a STATUS payload; `None` for OK.
fn status_error(payload: &[u8]) -> Option<Error> {
    match proto::status(payload) {
        Ok((proto::FX_OK, _)) => None,
        Ok((code, msg)) => {
            let msg = if msg.is_empty() { proto::status_text(code).to_string() } else { msg };
            Some(Error { code, msg })
        }
        Err(e) => Some(Error::new(e)),
    }
}

/// Reader thread: route each reply to whoever waits for its id. When the stream ends, every
/// waiting request fails (its sender is dropped).
fn read_replies(mut stdout: ChildStdout, shared: &Shared) {
    while let Ok((kind, mut payload)) = read_packet(&mut stdout) {
        if payload.len() < 4 {
            break;
        }
        let id = u32::from_be_bytes(payload[..4].try_into().unwrap());
        payload.drain(..4);
        if let Some(tx) = shared.waiting.lock().unwrap().remove(&id) {
            let _ = tx.send(Reply { kind, payload });
        }
    }
    shared.closed.store(true, Ordering::Relaxed);
    shared.waiting.lock().unwrap().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_errors() {
        let text = "Warning: Permanently added 'x' (ED25519) to the list of known hosts.\r\n\
                    cosh@x: Permission denied (publickey,password).\r\n";
        assert_eq!(stderr_message(text).as_deref(), Some("cosh@x: Permission denied (publickey,password)"));
        assert_eq!(stderr_message("Warning: only this\n").as_deref(), Some("Warning: only this"));
        assert_eq!(stderr_message("\n"), None);
    }
}
