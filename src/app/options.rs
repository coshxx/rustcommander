//! The options dialog (Ctrl+,): the language (a small drop-down list) and switches. Everything
//! takes effect and is saved right away.

use crate::app::{App, Mode};
use crate::config::Options;
use crate::i18n::{self, Lang};

/// Row of the language choice; the switches follow it.
pub(crate) const LANGUAGE_ROW: usize = 0;
/// Translation keys of the switches (rows 1..).
pub(crate) const SWITCHES: [&str; 5] =
    ["options.exact_sizes", "options.alt_search", "options.sort_dirs", "options.space_stays", "options.command_line"];
/// Rows of the dialog.
pub(crate) const ROWS: usize = 1 + SWITCHES.len();

/// Switch `i` (see `SWITCHES`).
pub(crate) fn flag(o: &mut Options, i: usize) -> Option<&mut bool> {
    match i {
        0 => Some(&mut o.exact_sizes),
        1 => Some(&mut o.alt_search),
        2 => Some(&mut o.sort_dirs),
        3 => Some(&mut o.space_stays),
        4 => Some(&mut o.command_line),
        _ => None,
    }
}

/// Switch "quick search only with Alt+letter": has no effect while the command line is shown.
pub(crate) const ALT_SEARCH: usize = 1;

/// Whether switch `i` is greyed out (and can't be toggled).
pub(crate) fn is_disabled(o: Options, i: usize) -> bool {
    i == ALT_SEARCH && o.command_line
}

/// Whether switch `i` is on.
pub(crate) fn is_on(mut o: Options, i: usize) -> bool {
    flag(&mut o, i).is_some_and(|f| *f)
}

impl App {
    pub(crate) fn open_options(&mut self) {
        self.panels[self.active].search.clear();
        self.mode = Mode::Options { sel: 0, popup: None };
    }

    /// "Options…" in the window menu: like Ctrl+, but only when nothing else is open.
    pub(crate) fn menu_options(&mut self) {
        if let Mode::Normal = self.mode {
            self.open_options();
            self.invalidate();
        }
    }

    /// Space / Enter / click on row `row`: open the language list or flip a switch.
    pub(crate) fn activate_option(&mut self, row: usize) {
        if row == LANGUAGE_ROW {
            let current = Lang::ALL.iter().position(|&l| l == i18n::lang()).unwrap_or(0);
            self.mode = Mode::Options { sel: row, popup: Some(current) };
        } else {
            self.toggle_option(row - 1);
        }
    }

    /// Flip switch `i`, select its row and save the config.
    fn toggle_option(&mut self, i: usize) {
        if is_disabled(self.options, i) {
            return;
        }
        let Some(f) = flag(&mut self.options, i) else { return };
        *f = !*f;
        self.mode = Mode::Options { sel: i + 1, popup: None };
        for p in &mut self.panels {
            p.resort(self.options.sort_dirs);
        }
        // The command line takes room from the file lists.
        self.layout();
        self.config(self.hwnd).save();
    }

    /// Switch the UI language (it is remembered from now on instead of detected).
    pub(crate) fn choose_language(&mut self, lang: Lang) {
        i18n::set(lang);
        self.options.language = Some(lang);
        // Key bar texts are measured once per language.
        self.hint_widths = None;
        self.mode = Mode::Options { sel: LANGUAGE_ROW, popup: None };
        crate::lister::repaint_all();
        crate::window::system_menu(self.hwnd);
        self.config(self.hwnd).save();
    }
}
