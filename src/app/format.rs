//! Small pure helpers: formatting sizes and dates, checking names, parsing paths.

use std::path::{Path, PathBuf};

use crate::sys::win;

pub(crate) fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

pub(crate) fn format_size(b: u64) -> String {
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
    format!("{} {}", s.replace('.', ","), UNITS[u])
}

pub(crate) fn format_date(t: u64) -> String {
    if t == 0 {
        return String::new();
    }
    match win::local_time(t) {
        Some(lt) => format!("{:02}.{:02}.{:04}  {:02}:{:02}", lt.wDay, lt.wMonth, lt.wYear, lt.wHour, lt.wMinute),
        None => String::new(),
    }
}

/// „name“ for a single item, otherwise "N Elemente".
pub(crate) fn describe(sources: &[PathBuf]) -> String {
    match sources {
        [one] => format!("„{}“", one.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()),
        many => format!("{} Elemente", many.len()),
    }
}

/// File name check; `allow_sep` permits `\` for nested new directories.
pub(crate) fn valid_name(name: &str, allow_sep: bool) -> bool {
    let bad = |c: char| matches!(c, '<' | '>' | ':' | '"' | '/' | '|' | '?' | '*') || (c == '\\' && !allow_sep);
    !name.is_empty() && name != "." && name != ".." && !name.chars().any(|c| bad(c) || (c as u32) < 0x20)
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
}
