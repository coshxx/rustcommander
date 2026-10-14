//! Temporary files: entries opened from inside an archive are extracted to
//! `%TEMP%\RustCommander\<session>\<n>\`. Nothing outside `%TEMP%\RustCommander` is ever deleted.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

/// Leftovers of earlier sessions older than this are removed at startup.
const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

static SESSION: OnceLock<PathBuf> = OnceLock::new();

fn root() -> PathBuf {
    std::env::temp_dir().join("RustCommander")
}

/// This session's directory, unique per process start.
fn session() -> &'static PathBuf {
    SESSION.get_or_init(|| {
        let nanos = UNIX_EPOCH.elapsed().map(|d| d.as_nanos()).unwrap_or(0);
        root().join(format!("{:x}-{:x}", std::process::id(), nanos))
    })
}

/// A new, empty directory for one extraction (so equal file names never collide).
pub fn new_dir() -> std::io::Result<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let dir = session().join(NEXT.fetch_add(1, Ordering::Relaxed).to_string());
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// On exit: delete this session's files. Files still open in another program stay.
pub fn cleanup_session() {
    if let Some(dir) = SESSION.get() {
        remove_best_effort(dir);
    }
}

/// On startup (background thread): delete session directories older than `MAX_AGE`.
pub fn sweep_old() {
    let Ok(entries) = std::fs::read_dir(root()) else { return };
    for e in entries.flatten() {
        let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
        let old = e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()) > Some(MAX_AGE);
        if is_dir && old {
            remove_best_effort(&e.path());
        }
    }
}

/// Delete whatever can be deleted and keep going on errors. Links (symlinks, junctions) are
/// removed themselves, never followed.
fn remove_best_effort(path: &Path) {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return };
    let kind = meta.file_type();
    if kind.is_symlink() {
        let _ = std::fs::remove_dir(path).or_else(|_| std::fs::remove_file(path));
    } else if kind.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            for e in entries.flatten() {
                remove_best_effort(&e.path());
            }
        }
        let _ = std::fs::remove_dir(path);
    } else {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_dirs_are_created_and_removed() {
        let a = new_dir().unwrap();
        let b = new_dir().unwrap();
        assert_ne!(a, b);
        assert!(a.starts_with(root()) && a.is_dir());
        std::fs::write(a.join("x.txt"), "x").unwrap();
        cleanup_session();
        assert!(!a.exists() && !b.exists());
        assert!(!session().exists());
    }
}
