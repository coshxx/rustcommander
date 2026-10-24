//! Files dropped onto a panel, from the other panel or from another program: the copy (Shift:
//! move) dialog opens with the target filled in. While dragging, the target folder row or panel
//! is highlighted.

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::app::dialogs::{Dialog, DialogKind, Extra, transfer_prompt};
use crate::app::layout::Hit;
use crate::app::{App, Mode};
use crate::files::folder_kind::Action;
use crate::files::listing::Kind;
use crate::files::location::Location;
use crate::files::names::fold_case;

/// Where a drop would go: into `panel`'s directory, or into the folder in its row `row`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct DropHover {
    pub(crate) panel: usize,
    pub(crate) row: Option<usize>,
}

impl App {
    /// The mouse is over the window with `paths`: highlight the target. `false` = can't drop here.
    pub fn on_drop_over(&mut self, x: f32, y: f32, paths: &[PathBuf]) -> bool {
        let target = self.drop_target(x, y, paths);
        self.set_drop_hover(target.as_ref().map(|t| t.0));
        target.is_some()
    }

    /// The drag left the window (or was cancelled).
    pub fn on_drop_leave(&mut self) {
        self.set_drop_hover(None);
    }

    /// Dropped: open the copy / move dialog. `false` = can't drop here.
    pub fn on_drop(&mut self, x: f32, y: f32, paths: Vec<PathBuf>, mov: bool) -> bool {
        self.set_drop_hover(None);
        let Some((hover, dest)) = self.drop_target(x, y, &paths) else { return false };
        let kind = if mov { DialogKind::Move } else { DialogKind::Copy };
        let sources: Vec<Location> = paths.into_iter().map(Location::Local).collect();
        let (message, input) = transfer_prompt(kind, &sources, &dest.into());
        // Dragged from a panel: like F5 there. From outside: the dialog sits on the target panel.
        let (panel, keep_marks) = match self.drag_source {
            Some(p) => (p, false),
            None => (hover.panel, true),
        };
        self.cancel_pending_rename();
        self.mode = Mode::Dialog(Dialog {
            kind,
            panel,
            sources,
            message,
            input: Some(input),
            extra: Extra::None,
            focus: 0,
            keep_marks,
        });
        self.invalidate();
        true
    }

    fn set_drop_hover(&mut self, hover: Option<DropHover>) {
        if self.drop_hover != hover {
            self.drop_hover = hover;
            self.invalidate();
        }
    }

    /// The target of dropping `paths` at (`x`, `y`): a folder row, else the panel's directory.
    /// `None` while busy, over archives, outside the panels, or where it makes no sense (the
    /// directory they are in, one of them or a folder inside them).
    fn drop_target(&self, x: f32, y: f32, paths: &[PathBuf]) -> Option<(DropHover, PathBuf)> {
        if !matches!(self.mode, Mode::Normal) || self.job.is_some() {
            return None;
        }
        let (_, h) = self.gfx.size_dips(self.hwnd);
        let (top, lh) = self.list_area(h);
        let (panel, row) = match self.hit(x, y) {
            Hit::Row(p, row) => (p, Some(row)),
            // Below the list: the status bar.
            Hit::Panel(p) if y < top + lh => (p, None),
            Hit::Drive(p) | Hit::Path(p) | Hit::Header(p, _) | Hit::Scrollbar(p) => (p, None),
            _ => return None,
        };
        let p = &self.panels[panel];
        p.kind.allows(Action::DropInto).ok()?;
        let dir = p.dir.local()?;
        let folder = row.and_then(|r| p.entries.get(r).filter(|e| e.kind == Kind::Dir).map(|e| (r, e.os_name())));
        let (row, dest) = match folder {
            Some((r, name)) => (Some(r), dir.join(name)),
            None => (None, dir.to_path_buf()),
        };
        (!pointless(paths, &dest)).then_some((DropHover { panel, row }, dest))
    }
}

/// Dropping `sources` into `dest` makes no sense: it is the directory one of them is in, one of
/// them, or inside one of them.
fn pointless(sources: &[PathBuf], dest: &Path) -> bool {
    let d = path_key(dest);
    sources.iter().any(|s| {
        let s_key = path_key(s);
        let inside = d.starts_with(&s_key) && (d.len() == s_key.len() || d[s_key.len()] == b'\\' as u32);
        inside || s.parent().is_some_and(|parent| path_key(parent) == d)
    })
}

/// `p` for comparing: case folded, without a trailing backslash (`C:\` → `c:`).
fn path_key(p: &Path) -> Vec<u32> {
    let mut key: Vec<u32> = p.as_os_str().encode_wide().map(fold_case).collect();
    while key.last() == Some(&(b'\\' as u32)) {
        key.pop();
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(p: &[&str]) -> Vec<PathBuf> {
        p.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn into_their_own_directory() {
        assert!(pointless(&paths(&[r"C:\a\x.txt"]), Path::new(r"C:\a")));
        assert!(pointless(&paths(&[r"C:\a\x.txt"]), Path::new(r"c:\A\")));
        assert!(pointless(&paths(&[r"C:\x.txt"]), Path::new(r"C:\")));
    }

    #[test]
    fn into_themselves() {
        assert!(pointless(&paths(&[r"C:\a\dir"]), Path::new(r"C:\a\dir")));
        assert!(pointless(&paths(&[r"C:\a\dir"]), Path::new(r"C:\a\Dir\sub")));
        assert!(pointless(&paths(&[r"C:\b\x", r"C:\a\dir"]), Path::new(r"C:\a\dir\sub")));
    }

    #[test]
    fn elsewhere() {
        assert!(!pointless(&paths(&[r"C:\a\dir"]), Path::new(r"C:\a\dir2")));
        assert!(!pointless(&paths(&[r"C:\a\x.txt"]), Path::new(r"C:\a\sub")));
        assert!(!pointless(&paths(&[r"C:\a\x.txt"]), Path::new(r"D:\a")));
        assert!(!pointless(&paths(&[r"C:\a\b\x.txt"]), Path::new(r"C:\a")));
    }
}
