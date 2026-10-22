//! Small pure helpers: formatting sizes and dates (in the format of the UI language), checking
//! names, parsing paths.

use std::path::{Path, PathBuf};

use crate::files::location::Location;
use crate::i18n::{self, Lang, tf, tn};
use crate::sftp::RemotePath;
use crate::sys::win;

pub(crate) fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// Rounded size: `1.2 GB` (English) / `1,2 GB` (German).
pub(crate) fn format_size(b: u64) -> String {
    format_size_in(b, i18n::lang())
}

fn format_size_in(b: u64, lang: Lang) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if b < 1024 {
        return format!("{b} B");
    }
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    let s = if v < 10.0 { format!("{v:.1}") } else { format!("{v:.0}") };
    format!("{} {}", decimal_in(&s, lang), UNITS[u])
}

/// A number formatted with `.` as decimal point, in the UI language's notation.
pub(crate) fn decimal(s: &str) -> String {
    decimal_in(s, i18n::lang())
}

fn decimal_in(s: &str, lang: Lang) -> String {
    if lang == Lang::De { s.replace('.', ",") } else { s.to_string() }
}

/// Exact byte count with thousands separators: `48,318,382,080` / `48.318.382.080`.
pub(crate) fn format_bytes(b: u64) -> String {
    format_bytes_in(b, i18n::lang())
}

fn format_bytes_in(b: u64, lang: Lang) -> String {
    let sep = if lang == Lang::De { '.' } else { ',' };
    let digits = b.to_string();
    let mut out = String::with_capacity(digits.len() * 4 / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(sep);
        }
        out.push(c);
    }
    out
}

/// Local time of a FILETIME: `2026-10-15  09:21` (English, ISO) / `15.10.2026  09:21` (German).
pub(crate) fn format_date(t: u64) -> String {
    if t == 0 {
        return String::new();
    }
    match win::local_time(t) {
        Some(lt) => format_date_in((lt.wYear, lt.wMonth, lt.wDay, lt.wHour, lt.wMinute), i18n::lang()),
        None => String::new(),
    }
}

fn format_date_in((y, mo, d, h, mi): (u16, u16, u16, u16, u16), lang: Lang) -> String {
    match lang {
        Lang::De => format!("{d:02}.{mo:02}.{y:04}  {h:02}:{mi:02}"),
        Lang::En => format!("{y:04}-{mo:02}-{d:02}  {h:02}:{mi:02}"),
    }
}

/// The quoted name of a single item, otherwise "N items".
pub(crate) fn describe(sources: &[Location]) -> String {
    match sources {
        [one] => quoted(&one.file_name().unwrap_or_default()),
        many => tn("dialog.items", many.len() as u64),
    }
}

/// `“name”` / `„name“`.
pub(crate) fn quoted(name: &str) -> String {
    tf("dialog.quoted", &[("name", &name)])
}

/// File name check; `allow_sep` permits `\` for nested new directories.
pub(crate) fn valid_name(name: &str, allow_sep: bool) -> bool {
    let bad = |c: char| matches!(c, '<' | '>' | ':' | '"' | '/' | '|' | '?' | '*') || (c == '\\' && !allow_sep);
    !name.is_empty() && name != "." && name != ".." && !name.chars().any(|c| bad(c) || (c as u32) < 0x20)
}

/// Name check on a server: anything except `/` and `\`, `.`, `..` and control characters;
/// `allow_sep` permits `/` and `\` between the parts of nested new folders.
pub(crate) fn valid_remote_name(name: &str, allow_sep: bool) -> bool {
    let ok = |p: &str| !p.is_empty() && p != "." && p != ".." && !p.chars().any(|c| (c as u32) < 0x20);
    if allow_sep { name.split(['/', '\\']).all(ok) } else { ok(name) && !name.contains(['/', '\\']) }
}

/// Turn path bar input into an absolute path.
pub(crate) fn parse_path(input: &[u16], base: &Path) -> Option<PathBuf> {
    let expanded = win::expand_env(input);
    let mut s = String::from_utf16_lossy(&expanded).trim().trim_matches('"').to_string();
    if s.is_empty() {
        return None;
    }
    if s.len() == 2 && s.ends_with(':') {
        s.push('\\');
    }
    let p = PathBuf::from(s);
    let p = if p.is_absolute() { p } else { base.join(p) };
    std::path::absolute(p).ok()
}

/// Like `parse_path`, but also `sftp://…`; on a server (`base` remote) anything that isn't a
/// Windows path (`C:\…`, `\\server`, `%VAR%`) is a path on that server, relative to `base`.
pub(crate) fn parse_location(input: &[u16], base: &Location) -> Option<Location> {
    let text = String::from_utf16_lossy(input);
    let text = text.trim().trim_matches('"');
    if let Some(r) = RemotePath::parse(text) {
        return Some(Location::Remote(r));
    }
    match base {
        Location::Remote(r) if !text.is_empty() && !is_windows_path(text) => Some(Location::Remote(r.resolve(text))),
        Location::Remote(_) => parse_path(input, Path::new("")).map(Location::Local),
        Location::Local(d) => parse_path(input, d).map(Location::Local),
    }
}

fn is_windows_path(s: &str) -> bool {
    let b = s.as_bytes();
    s.starts_with(r"\\") || s.contains('%') || (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':')
}

/// Typed with a trailing `\` or `/`: means "into this folder".
pub(crate) fn ends_with_separator(input: &str) -> bool {
    input.trim().trim_matches('"').ends_with(['\\', '/'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_name("neu.txt", false));
        assert!(!valid_name("a\\b", false));
        assert!(valid_name("a\\b", true));
        assert!(!valid_name("", true));
        assert!(!valid_name("..", false));
        assert!(!valid_name("x?.txt", false));
    }

    #[test]
    fn locations() {
        let remote = Location::Remote(RemotePath::new("h", "/home/u"));
        let at = |s: &str, base: &Location| parse_location(&utf16(s), base);
        assert_eq!(at("..", &remote), Some(Location::Remote(RemotePath::new("h", "/home"))));
        assert_eq!(at("/etc", &remote), Some(Location::Remote(RemotePath::new("h", "/etc"))));
        assert_eq!(at(r"D:\x", &remote), Some(Location::Local(r"D:\x".into())));
        assert_eq!(
            at("sftp://g/x", &Location::Local(r"C:\".into())),
            Some(Location::Remote(RemotePath::new("g", "/x")))
        );
        assert_eq!(at("sub", &Location::Local(r"C:\a".into())), Some(Location::Local(r"C:\a\sub".into())));
        assert!(ends_with_separator("sftp://h/x/ "));
        assert!(!ends_with_separator(r"C:\x"));
    }

    #[test]
    fn bytes() {
        assert_eq!(format_bytes_in(0, Lang::De), "0");
        assert_eq!(format_bytes_in(999, Lang::De), "999");
        assert_eq!(format_bytes_in(1000, Lang::De), "1.000");
        assert_eq!(format_bytes_in(48_318_382_080, Lang::De), "48.318.382.080");
        assert_eq!(format_bytes_in(u64::MAX, Lang::De), "18.446.744.073.709.551.615");
        assert_eq!(format_bytes_in(48_318_382_080, Lang::En), "48,318,382,080");
    }

    #[test]
    fn sizes_and_dates() {
        assert_eq!(format_size_in(1_288_490_189, Lang::De), "1,2 GB");
        assert_eq!(format_size_in(1_288_490_189, Lang::En), "1.2 GB");
        assert_eq!(format_size_in(512, Lang::En), "512 B");
        assert_eq!(format_date_in((2026, 10, 5, 9, 1), Lang::De), "05.10.2026  09:01");
        assert_eq!(format_date_in((2026, 10, 5, 9, 1), Lang::En), "2026-10-05  09:01");
    }
}
