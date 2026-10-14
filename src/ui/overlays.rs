//! Drawing what floats above the panels: dialogs, conflict question, drive popup,
//! progress and notice cards.

use std::path::Path;

use crate::app::dialogs::{Dialog, DialogKind, DrivePopup};
use crate::app::format::format_size;
use crate::app::jobs::JOB_CARD_DELAY_MS;
use crate::app::layout::{CARD_W, ICON_W, KEYBAR_H, ROW_H, STATUS_H};
use crate::app::{App, Tone};
use crate::sys::gfx::{Canvas, rect};

fn drive_info(kind: u32) -> (u16, &'static str) {
    match kind {
        2 => (0xE88E, "Wechseldatenträger"),
        4 => (0xE8CE, "Netzwerk"),
        5 => (0xE958, "CD/DVD"),
        6 => (0xEDA2, "RAM-Disk"),
        _ => (0xEDA2, "Lokaler Datenträger"),
    }
}

impl App {
    pub(crate) fn draw_dialog(&self, c: &Canvas, d: &Dialog, w: f32, h: f32, caret_x: Option<f32>) {
        let t = self.theme;
        c.fill(rect(0.0, 0.0, w, h), crate::theme::rgba(0x000000, 0.35));
        let dw = (w - 48.0).clamp(200.0, 600.0);
        let input_h = if d.input.is_some() { 44.0 } else { 0.0 };
        let dh = 20.0 + 28.0 + 26.0 + input_h + 44.0;
        let r = rect(((w - dw) / 2.0).floor(), ((h - dh) / 2.5).floor(), dw, dh);
        self.draw_card(c, r);
        let x = r.left + 20.0;
        let iw = dw - 40.0;
        let mut y = r.top + 16.0;
        let danger = d.kind == DialogKind::Delete { recycle: false };
        c.text_str(d.kind.title(), &c.fonts.path, rect(x, y, iw, 28.0), if danger { t.error } else { t.text });
        y += 28.0;
        c.text_str(&d.message, &c.fonts.name, rect(x, y, iw, 26.0), t.text_dim);
        y += 30.0;
        if let (Some(input), Some(cx)) = (&d.input, caret_x) {
            self.draw_input(c, rect(x, y, iw, 32.0), input, cx);
            y += input_h;
        }
        let ok = if matches!(d.kind, DialogKind::Delete { .. }) { "Löschen" } else { "OK" };
        self.draw_chips(c, x, y, 28.0, &[("Enter", ok), ("Esc", "Abbrechen")]);
    }

    pub(crate) fn draw_conflict(&self, c: &Canvas, path: &Path, w: f32, h: f32) {
        let t = self.theme;
        c.fill(rect(0.0, 0.0, w, h), crate::theme::rgba(0x000000, 0.35));
        let dw = (w - 48.0).clamp(200.0, 720.0);
        let dh = 20.0 + 28.0 + 26.0 + 44.0;
        let r = rect(((w - dw) / 2.0).floor(), ((h - dh) / 2.5).floor(), dw, dh);
        self.draw_card(c, r);
        let x = r.left + 20.0;
        let iw = dw - 40.0;
        c.text_str("Ziel existiert bereits", &c.fonts.path, rect(x, r.top + 16.0, iw, 28.0), t.marked);
        c.text_str(&path.display().to_string(), &c.fonts.name, rect(x, r.top + 44.0, iw, 26.0), t.text_dim);
        let chips = [
            ("Enter", "Überschreiben"),
            ("S", "Überspringen"),
            ("A", "Alle überschreiben"),
            ("K", "Keine überschreiben"),
            ("Esc", "Abbrechen"),
        ];
        self.draw_chips(c, x, r.top + 74.0, 28.0, &chips);
    }

    pub(crate) fn draw_job_or_notice(&self, c: &Canvas, w: f32, h: f32) {
        let t = self.theme;
        let cw = CARD_W.min(w - 32.0);
        let x = w - cw - 16.0;
        let bottom = h - KEYBAR_H - STATUS_H - 12.0;
        if let Some(job) = &self.job {
            if job.started.elapsed().as_millis() < JOB_CARD_DELAY_MS as u128 {
                return;
            }
            let p = &job.progress;
            let r = rect(x, bottom - 92.0, cw, 92.0);
            self.draw_card(c, r);
            let ix = r.left + 16.0;
            let iw = cw - 32.0;
            let frac = if p.bytes_total > 0 {
                p.bytes_done as f64 / p.bytes_total as f64
            } else if p.files_total > 0 {
                p.files_done as f64 / p.files_total as f64
            } else {
                0.0
            }
            .clamp(0.0, 1.0) as f32;
            c.text_str(&format!("{} …", job.title), &c.fonts.path, rect(ix, r.top + 8.0, iw, 24.0), t.text);
            c.text_str(
                &format!("{:.0} %", frac * 100.0),
                &c.fonts.small_right,
                rect(ix, r.top + 8.0, iw, 24.0),
                t.accent,
            );
            let current = p.current.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            c.text_str(&current, &c.fonts.small, rect(ix, r.top + 32.0, iw, 18.0), t.text_dim);
            let bar = rect(ix, r.top + 54.0, iw, 6.0);
            c.fill_round(bar, 3.0, t.edit_bg);
            if frac > 0.0 {
                c.fill_round(rect(ix, bar.top, (iw * frac).max(6.0), 6.0), 3.0, t.accent);
            }
            let mut stats = format!("{} / {} Dateien", p.files_done, p.files_total);
            if p.bytes_total > 0 {
                stats += &format!("  ·  {} / {}", format_size(p.bytes_done), format_size(p.bytes_total));
            }
            c.text_str(&stats, &c.fonts.small, rect(ix, r.top + 64.0, iw, 22.0), t.text_dim);
            c.text_str("Esc bricht ab", &c.fonts.small_right, rect(ix, r.top + 64.0, iw, 22.0), t.text_hidden);
        } else if let Some(n) = &self.notice {
            let r = rect(x, bottom - 40.0, cw, 40.0);
            self.draw_card(c, r);
            let (glyph, col) = match n.tone {
                Tone::Success => (0xE73E, t.success),
                Tone::Info => (0xE946, t.text_dim),
                Tone::Error => (0xE783, t.error),
            };
            c.text(&[glyph], &c.fonts.icon, rect(r.left + 10.0, r.top, 24.0, 40.0), col);
            c.text_str(&n.text, &c.fonts.name, rect(r.left + 40.0, r.top, cw - 52.0, 40.0), t.text);
        }
    }

    pub(crate) fn draw_drives(&self, c: &Canvas, d: &DrivePopup, w: f32) {
        let t = self.theme;
        let (px, _) = self.panel_x(d.panel, w);
        let r = self.popup_rect(px, d.drives.len());
        let shadow = rect(r.left + 2.0, r.top + 4.0, r.right - r.left, r.bottom - r.top);
        c.fill_round(shadow, 8.0, crate::theme::rgba(0x000000, 0.35));
        c.fill_round(r, 8.0, t.popup_bg);
        c.stroke_round(r, 8.0, 1.0, t.popup_border);
        for (i, drive) in d.drives.iter().enumerate() {
            let y = r.top + 4.0 + i as f32 * ROW_H;
            if i == d.sel {
                c.fill_round(rect(r.left + 4.0, y, r.right - r.left - 8.0, ROW_H), 4.0, t.cursor_active);
            }
            let (glyph, label) = drive_info(drive.kind);
            c.text(&[glyph], &c.fonts.icon, rect(r.left + 8.0, y, ICON_W, ROW_H), t.accent);
            c.text(&drive.root[..2], &c.fonts.path, rect(r.left + 38.0, y, 30.0, ROW_H), t.text);
            c.text_str(label, &c.fonts.small, rect(r.left + 72.0, y, r.right - r.left - 80.0, ROW_H), t.text_dim);
        }
    }
}
