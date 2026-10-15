//! The options dialog (Ctrl+,): a list of switches that take effect and are saved right away.

use crate::app::{App, Mode};
use crate::config::Options;

/// Label of each option, in dialog order.
pub(crate) const LABELS: [&str; 3] =
    ["Genaue Größen in Bytes anzeigen", "Schnellsuche nur mit Alt+Buchstabe", "Ordner in die Sortierung einschließen"];

/// Option `i` of the dialog (see `LABELS`).
pub(crate) fn flag(o: &mut Options, i: usize) -> Option<&mut bool> {
    match i {
        0 => Some(&mut o.exact_sizes),
        1 => Some(&mut o.alt_search),
        2 => Some(&mut o.sort_dirs),
        _ => None,
    }
}

/// Whether option `i` is on.
pub(crate) fn is_on(mut o: Options, i: usize) -> bool {
    flag(&mut o, i).is_some_and(|f| *f)
}

impl App {
    pub(crate) fn open_options(&mut self) {
        self.panels[self.active].search.clear();
        self.mode = Mode::Options { sel: 0 };
    }

    /// Switch option `i`, select it and save the config.
    pub(crate) fn toggle_option(&mut self, i: usize) {
        let Some(f) = flag(&mut self.options, i) else { return };
        *f = !*f;
        self.mode = Mode::Options { sel: i };
        for p in &mut self.panels {
            p.resort(self.options.sort_dirs);
        }
        self.config(self.hwnd).save();
    }
}
