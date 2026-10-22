//! Where things are on screen: sizes in DIPs, panel geometry and hit-testing.

use crate::app::dialogs::{Dialog, PopupItem};
use crate::app::format::utf16;
use crate::app::{App, Mode};
use crate::files::sort::SortKey;
use crate::sys::gfx::{Rect, rect};

// Layout in DIPs.
pub(crate) const PATH_H: f32 = 38.0;
pub(crate) const COLS_H: f32 = 24.0;
pub(crate) const ROW_H: f32 = 24.0;
pub(crate) const STATUS_H: f32 = 26.0;
pub(crate) const KEYBAR_H: f32 = 30.0;
/// The command line (option) between the status bars and the key bar.
pub(crate) const CMD_H: f32 = 34.0;
const SIZE_W: f32 = 84.0;
/// Size column with exact byte counts (option); fits `≥ 99.999.999.999.999`.
const SIZE_W_EXACT: f32 = 140.0;
pub(crate) const DATE_W: f32 = 118.0;
pub(crate) const PAD: f32 = 12.0;
pub(crate) const ICON_W: f32 = 24.0;
pub(crate) const CHIP_W: f32 = 40.0;
const POPUP_W: f32 = 260.0;
/// The ✕ (disconnect) at the right end of a connected host in the drive popup.
pub(crate) const POPUP_CLOSE_W: f32 = 28.0;
/// Options dialog: width, height of one option row, rows start this far below the top.
const OPTIONS_W: f32 = 460.0;
pub(crate) const OPTION_H: f32 = 32.0;
pub(crate) const OPTIONS_ROWS_Y: f32 = 56.0;
/// Width of the language list in the options dialog.
const LANGUAGE_POPUP_W: f32 = 180.0;
pub(crate) const CARD_W: f32 = 400.0;
/// Gear button at the right end of the key bar (opens the options).
pub(crate) const GEAR_W: f32 = 34.0;
/// Width of the scrollbar's mouse zone at the panel's right edge.
const SCROLL_ZONE_W: f32 = 14.0;
const THUMB_MIN_H: f32 = 24.0;

/// The gear button in the key bar.
pub(crate) fn gear_rect(w: f32, h: f32) -> Rect {
    rect(w - GEAR_W - 6.0, h - KEYBAR_H + 4.0, GEAR_W, KEYBAR_H - 8.0)
}

/// What is under the mouse.
pub(crate) enum Hit {
    /// Drive chip of a panel.
    Drive(usize),
    /// Path bar of a panel.
    Path(usize),
    /// Column header of a panel (click sorts).
    Header(usize, SortKey),
    /// (panel, entry index)
    Row(usize, usize),
    Scrollbar(usize),
    /// Somewhere else in a panel (e.g. below the last entry).
    Panel(usize),
    /// Entry of the drive popup.
    Popup(usize),
    /// Gear button in the key bar.
    Gear,
    /// Entry of the key bar (its id, see `HINTS`).
    Hint(&'static str),
    /// The command line.
    CommandLine,
    Nothing,
}

/// Height of a choice row in a dialog and the width of its label.
pub(crate) const DIALOG_ROW_H: f32 = 34.0;
pub(crate) const DIALOG_LABEL_W: f32 = 96.0;

/// Parts of a small dialog (DIPs).
pub(crate) struct DialogLayout {
    pub(crate) card: Rect,
    pub(crate) input: Option<Rect>,
    /// Choice rows below the input.
    pub(crate) rows: Vec<Rect>,
    /// Top of the key chip row.
    pub(crate) chips_y: f32,
}

/// Parts of the search dialog (DIPs).
pub(crate) struct FindLayout {
    pub(crate) card: Rect,
    /// Name, folder, text.
    pub(crate) inputs: [Rect; 3],
    pub(crate) status: Rect,
    pub(crate) list: Rect,
    /// Top of the key chip row.
    pub(crate) chips_y: f32,
}

/// Width of the labels left of the search dialog's inputs.
pub(crate) const FIND_LABEL_W: f32 = 110.0;

/// Left edges of a panel's columns and the width of the size column (DIPs).
pub(crate) struct Columns {
    pub(crate) name_x: f32,
    pub(crate) size_x: f32,
    pub(crate) size_w: f32,
    pub(crate) date_x: f32,
}

/// Scrollbar geometry of one panel (DIPs).
pub(crate) struct ScrollBar {
    /// Mouse-sensitive strip at the right edge of the list.
    zone: Rect,
    pub(crate) thumb_y: f32,
    pub(crate) thumb_h: f32,
}

impl App {
    /// Left edge and width of panel `i` in a window `w` DIPs wide.
    pub(crate) fn panel_x(&self, i: usize, w: f32) -> (f32, f32) {
        let half = (w / 2.0).floor();
        if i == 0 { (0.0, half) } else { (half + 1.0, w - half - 1.0) }
    }

    /// Top and height of the file list area in a window `h` DIPs high.
    pub(crate) fn list_area(&self, h: f32) -> (f32, f32) {
        let top = PATH_H + COLS_H;
        (top, (h - self.footer_h() - STATUS_H - top).max(0.0))
    }

    /// Height below the status bars: the key bar and, if shown, the command line.
    pub(crate) fn footer_h(&self) -> f32 {
        KEYBAR_H + if self.options.command_line { CMD_H } else { 0.0 }
    }

    /// The command line prompt: the active panel's path and `>`.
    pub(crate) fn prompt(&self) -> String {
        format!("{}>", self.panels[self.active].shown_path())
    }

    /// The command line: (whole bar, prompt, input box).
    pub(crate) fn command_line_layout(&self, w: f32, h: f32) -> (Rect, Rect, Rect) {
        let y = h - KEYBAR_H - CMD_H;
        let pw = (self.gfx.measure(&utf16(&self.prompt()), &self.gfx.fonts.name) + 4.0).min(w * 0.45);
        let input_x = PAD + pw + 4.0;
        (
            rect(0.0, y, w, CMD_H),
            rect(PAD, y, pw, CMD_H),
            rect(input_x, y + 4.0, (w - PAD - input_x).max(0.0), CMD_H - 8.0),
        )
    }

    /// After a resize: rows per page; keeps the scroll position valid.
    pub fn layout(&mut self) {
        let (_, h) = self.gfx.size_dips(self.hwnd);
        let rows = (self.list_area(h).1 / ROW_H).floor().max(1.0) as usize;
        for p in &mut self.panels {
            p.rows_visible = rows;
            // Only clamp: the user may have scrolled the cursor out of view on purpose.
            p.clamp_top();
        }
    }

    /// What is at (`x`, `y`).
    pub(crate) fn hit(&self, x: f32, y: f32) -> Hit {
        let (w, h) = self.gfx.size_dips(self.hwnd);
        if let Mode::Drives(d) = &self.mode {
            let (px, _) = self.panel_x(d.panel, w);
            let r = self.popup_rect(px, d.items.len());
            if x >= r.left && x < r.right && y >= r.top + 4.0 && y < r.bottom - 4.0 {
                return Hit::Popup(((y - r.top - 4.0) / ROW_H) as usize);
            }
        }
        if self.options.command_line && y >= h - KEYBAR_H - CMD_H && y < h - KEYBAR_H {
            return Hit::CommandLine;
        }
        if y >= h - KEYBAR_H {
            let g = gear_rect(w, h);
            if x >= g.left && x < g.right {
                return Hit::Gear;
            }
            let hint = self.hint_rects(w, h).into_iter().find(|(_, chip, label)| x >= chip.left && x < label.right);
            return hint.map_or(Hit::Nothing, |(id, ..)| Hit::Hint(id));
        }
        let p = if x < self.panel_x(1, w).0 { 0 } else { 1 };
        let (px, _) = self.panel_x(p, w);
        if y < PATH_H {
            return if x < px + PAD + CHIP_W { Hit::Drive(p) } else { Hit::Path(p) };
        }
        if y < PATH_H + COLS_H {
            // Each header reaches halfway into the gap before the next column.
            let c = self.columns(p, w);
            let key = if x >= c.date_x - 7.0 {
                SortKey::Date
            } else if x >= c.size_x - 4.0 {
                SortKey::Size
            } else {
                SortKey::Name
            };
            return Hit::Header(p, key);
        }
        let (top, lh) = self.list_area(h);
        if y >= top && y < top + lh {
            if self.scrollbar(p, w, h).is_some_and(|sb| x >= sb.zone.left) {
                return Hit::Scrollbar(p);
            }
            let row = self.panels[p].top + ((y - top) / ROW_H) as usize;
            if row < self.panels[p].entries.len() {
                return Hit::Row(p, row);
            }
        }
        Hit::Panel(p)
    }

    /// Scrollbar geometry; `None` if everything fits.
    pub(crate) fn scrollbar(&self, i: usize, w: f32, h: f32) -> Option<ScrollBar> {
        let p = &self.panels[i];
        let (top, lh) = self.list_area(h);
        if p.entries.len() <= p.rows_visible || lh <= 0.0 {
            return None;
        }
        let (x, pw) = self.panel_x(i, w);
        let thumb_h = (lh * p.rows_visible as f32 / p.entries.len() as f32).max(THUMB_MIN_H).min(lh);
        let thumb_y = top + (lh - thumb_h) * p.top as f32 / p.max_top() as f32;
        Some(ScrollBar { zone: rect(x + pw - SCROLL_ZONE_W, top, SCROLL_ZONE_W, lh), thumb_y, thumb_h })
    }

    /// Title, message, input, choice rows and key chips of dialog `d`.
    pub(crate) fn dialog_layout(&self, d: &Dialog, w: f32, h: f32) -> DialogLayout {
        let dw = (w - 48.0).clamp(200.0, 600.0);
        let input_h = if d.input.is_some() { 44.0 } else { 0.0 };
        let n = d.extra.rows();
        let dh = 20.0 + 28.0 + 26.0 + input_h + n as f32 * DIALOG_ROW_H + 44.0;
        let card = rect(((w - dw) / 2.0).floor(), ((h - dh) / 2.5).floor(), dw, dh);
        let x = card.left + 20.0;
        let iw = dw - 40.0;
        let mut y = card.top + 16.0 + 28.0 + 30.0;
        let input = d.input.as_ref().map(|_| rect(x, y, iw, 32.0));
        y += input_h;
        let rows = (0..n).map(|i| rect(x, y + i as f32 * DIALOG_ROW_H, iw, DIALOG_ROW_H)).collect();
        DialogLayout { card, input, rows, chips_y: y + n as f32 * DIALOG_ROW_H }
    }

    /// The choice buttons of a dialog row, right of its label.
    pub(crate) fn segment_rects(&self, row: Rect, labels: &[&str]) -> Vec<Rect> {
        let mut x = row.left + DIALOG_LABEL_W;
        labels
            .iter()
            .map(|l| {
                let sw = self.gfx.measure(&utf16(l), &self.gfx.fonts.small_center) + 24.0;
                let r = rect(x, row.top + 4.0, sw, row.bottom - row.top - 8.0);
                x += sw + 4.0;
                r
            })
            .collect()
    }

    /// Key bar entries that fit (id, key chip, label), left to right: the Alt keys while Alt is
    /// held.
    pub(crate) fn hint_rects(&self, w: f32, h: f32) -> Vec<(&'static str, Rect, Rect)> {
        let Some(widths) = &self.hint_widths else { return Vec::new() };
        let (ids, widths) = if self.alt_down {
            (&crate::ui::widgets::ALT_HINTS[..], &widths[1])
        } else {
            (&crate::ui::widgets::HINTS[..], &widths[0])
        };
        let y = h - KEYBAR_H;
        let mut x = PAD;
        let mut out = Vec::new();
        for (id, (kw, lw)) in ids.iter().zip(widths) {
            let chip_w = kw + 12.0;
            if x + chip_w + lw + 6.0 > w - GEAR_W - 12.0 {
                break;
            }
            let chip = rect(x, y + 6.0, chip_w, KEYBAR_H - 12.0);
            x += chip_w + 6.0;
            out.push((*id, chip, rect(x, y, lw + 2.0, KEYBAR_H)));
            x += lw + 18.0;
        }
        out
    }

    /// The search dialog in a `w` × `h` window: nearly the whole window.
    pub(crate) fn find_layout(w: f32, h: f32) -> FindLayout {
        let cw = (w - 48.0).clamp(300.0, 1100.0);
        let ch = (h - 48.0).max(320.0);
        let card = rect(((w - cw) / 2.0).floor(), ((h - ch) / 2.0).floor(), cw, ch);
        let x = card.left + 20.0 + FIND_LABEL_W;
        let iw = cw - 40.0 - FIND_LABEL_W;
        let inputs = [0.0, 1.0, 2.0].map(|i| rect(x, card.top + 52.0 + i * 40.0, iw, 32.0));
        let status = rect(card.left + 20.0, inputs[2].bottom + 8.0, cw - 40.0, 24.0);
        let chips_y = card.bottom - 40.0;
        let list =
            rect(card.left + 12.0, status.bottom + 4.0, cw - 24.0, (chips_y - 4.0 - status.bottom - 4.0).max(ROW_H));
        FindLayout { card, inputs, status, list, chips_y }
    }

    /// Columns of panel `i` in a window `w` DIPs wide; the size column is wider with exact
    /// byte counts.
    pub(crate) fn columns(&self, i: usize, w: f32) -> Columns {
        let (x, width) = self.panel_x(i, w);
        let size_w = if self.options.exact_sizes { SIZE_W_EXACT } else { SIZE_W };
        let date_x = x + width - PAD - DATE_W;
        Columns { name_x: x + PAD + ICON_W + 6.0, size_x: date_x - 14.0 - size_w, size_w, date_x }
    }

    /// The options dialog card in a `w` × `h` window: title, one row per option, key chips.
    pub(crate) fn options_rect(w: f32, h: f32) -> Rect {
        let dw = (w - 48.0).clamp(200.0, OPTIONS_W);
        let dh = OPTIONS_ROWS_Y + OPTION_H * crate::app::options::ROWS as f32 + 52.0;
        rect(((w - dw) / 2.0).floor(), ((h - dh) / 2.5).floor(), dw, dh)
    }

    /// Option row at (`x`, `y`): `Some(Some(i))` = row `i`, `Some(None)` = elsewhere on the
    /// card, `None` = outside the card.
    pub(crate) fn option_at(&self, x: f32, y: f32) -> Option<Option<usize>> {
        let (w, h) = self.gfx.size_dips(self.hwnd);
        let r = Self::options_rect(w, h);
        if x < r.left || x >= r.right || y < r.top || y >= r.bottom {
            return None;
        }
        let row = (y - r.top - OPTIONS_ROWS_Y) / OPTION_H;
        Some((row >= 0.0 && (row as usize) < crate::app::options::ROWS).then_some(row as usize))
    }

    /// The language list below the language row, right-aligned in the options card.
    pub(crate) fn language_popup_rect(w: f32, h: f32) -> Rect {
        let r = Self::options_rect(w, h);
        let n = crate::i18n::Lang::ALL.len() as f32;
        let top = r.top + OPTIONS_ROWS_Y + (crate::app::options::LANGUAGE_ROW as f32 + 1.0) * OPTION_H;
        rect(r.right - 20.0 - LANGUAGE_POPUP_W, top, LANGUAGE_POPUP_W, n * ROW_H + 8.0)
    }

    /// Entry of the open language list at (`x`, `y`).
    pub(crate) fn language_at(&self, x: f32, y: f32) -> Option<usize> {
        let (w, h) = self.gfx.size_dips(self.hwnd);
        let r = Self::language_popup_rect(w, h);
        let inside = x >= r.left && x < r.right && y >= r.top + 4.0 && y < r.bottom - 4.0;
        inside.then(|| ((y - r.top - 4.0) / ROW_H) as usize).filter(|&i| i < crate::i18n::Lang::ALL.len())
    }

    /// Drive popup below the drive chip of a panel at `px`.
    pub(crate) fn popup_rect(&self, px: f32, n: usize) -> Rect {
        rect(px + PAD - 4.0, PATH_H - 2.0, POPUP_W, n as f32 * ROW_H + 8.0)
    }

    /// Whether `x` is on the ✕ of popup row `i` (a connected host).
    pub(crate) fn on_popup_close(&self, i: usize, x: f32) -> bool {
        let Mode::Drives(d) = &self.mode else { return false };
        if !matches!(d.items.get(i), Some(PopupItem::Host { connected: true, .. })) {
            return false;
        }
        let (w, _) = self.gfx.size_dips(self.hwnd);
        let r = self.popup_rect(self.panel_x(d.panel, w).0, d.items.len());
        x >= r.right - POPUP_CLOSE_W - 4.0
    }
}
