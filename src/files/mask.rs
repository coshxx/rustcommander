//! File masks for marking by pattern (Num+ / Num−), Total Commander style:
//!
//! - `*` and `?` wildcards, case-insensitive.
//! - Several masks separated by `;` or spaces; `"…"` keeps spaces in a mask.
//! - `|` starts the exclude list: `*.* | *.bak`.
//! - A mask ending in `\` applies to directories, all others to files only.
//! - A mask without wildcards means "contains": `foto` = `*foto*`.
//! - `*.*` also matches names without a dot.

use crate::files::names::fold;

struct Pattern {
    chars: Vec<u32>,
    dirs: bool,
}

pub struct Mask {
    include: Vec<Pattern>,
    exclude: Vec<Pattern>,
}

fn split(s: &str) -> Vec<Pattern> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut push = |cur: &mut String| {
        let t = std::mem::take(cur);
        if !t.is_empty() {
            out.push(pattern(&t));
        }
    };
    for c in s.chars() {
        match c {
            '"' => quoted = !quoted,
            ';' | ' ' | '\t' if !quoted => push(&mut cur),
            _ => cur.push(c),
        }
    }
    push(&mut cur);
    out
}

fn pattern(s: &str) -> Pattern {
    let (s, dirs) = match s.strip_suffix('\\') {
        Some(rest) => (rest, true),
        None => (s, false),
    };
    let s = if s.is_empty() || s == "*.*" {
        "*".to_string()
    } else if !s.contains(['*', '?']) {
        format!("*{s}*")
    } else {
        s.to_string()
    };
    Pattern { chars: s.encode_utf16().map(fold).collect(), dirs }
}

/// Glob match with `*` and `?`; `pat` and `name` are already case-folded.
fn glob(pat: &[u32], name: &[u32]) -> bool {
    const STAR: u32 = '*' as u32;
    const ANY: u32 = '?' as u32;
    let (mut p, mut n) = (0, 0);
    // Position after the last `*` and the name position it was tried at.
    let mut back: Option<(usize, usize)> = None;
    while n < name.len() {
        if p < pat.len() && pat[p] == STAR {
            p += 1;
            back = Some((p, n));
        } else if p < pat.len() && (pat[p] == ANY || pat[p] == name[n]) {
            p += 1;
            n += 1;
        } else if let Some((bp, bn)) = back {
            p = bp;
            n = bn + 1;
            back = Some((bp, bn + 1));
        } else {
            return false;
        }
    }
    pat[p..].iter().all(|&c| c == STAR)
}

impl Mask {
    pub fn parse(s: &str) -> Mask {
        let (inc, exc) = s.split_once('|').unwrap_or((s, ""));
        let mut include = split(inc);
        if include.is_empty() {
            include.push(pattern("*"));
        }
        Mask { include, exclude: split(exc) }
    }

    pub fn matches(&self, name: &[u16], is_dir: bool) -> bool {
        let name: Vec<u32> = name.iter().map(|&c| fold(c)).collect();
        let hit = |ps: &[Pattern]| ps.iter().any(|p| p.dirs == is_dir && glob(&p.chars, &name));
        hit(&self.include) && !hit(&self.exclude)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(mask: &str, name: &str) -> bool {
        Mask::parse(mask).matches(&name.encode_utf16().collect::<Vec<_>>(), false)
    }

    fn d(mask: &str, name: &str) -> bool {
        Mask::parse(mask).matches(&name.encode_utf16().collect::<Vec<_>>(), true)
    }

    #[test]
    fn wildcards() {
        assert!(m("*.bmp", "Bild.BMP"));
        assert!(!m("*.bmp", "bild.bmp.txt"));
        assert!(m("bild?.png", "bild1.png"));
        assert!(!m("bild?.png", "bild12.png"));
        assert!(m("a*b*c", "aXXbYYc"));
        assert!(!m("a*b*c", "aXXbYY"));
        assert!(m("*", "x"));
    }

    #[test]
    fn star_dot_star_matches_names_without_dot() {
        assert!(m("*.*", "Makefile"));
        assert!(m("*.*", "a.txt"));
    }

    #[test]
    fn lists_and_excludes() {
        assert!(m("*.bmp;*.png", "a.png"));
        assert!(m("*.bmp *.png", "a.bmp"));
        assert!(!m("*.bmp;*.png", "a.jpg"));
        assert!(m("*.* | *.bak", "a.txt"));
        assert!(!m("*.* | *.bak", "a.bak"));
        assert!(!m("| *.bak", "a.bak"));
        assert!(m("| *.bak", "a.txt"));
    }

    #[test]
    fn contains_without_wildcards() {
        assert!(m("foto", "Urlaub-Foto-3.jpg"));
        assert!(!m("foto", "bild.jpg"));
    }

    #[test]
    fn quotes_keep_spaces() {
        assert!(m("\"mein bild*\"", "Mein Bild 1.png"));
        assert!(!m("\"mein bild*\"", "bild.png"));
    }

    #[test]
    fn dirs_only_with_backslash() {
        assert!(!d("*.*", "fotos"));
        assert!(!m("*\\", "a.txt"));
        assert!(d("*\\", "fotos"));
        assert!(d("temp*\\", "Temp2"));
        assert!(!d("temp*\\", "fotos"));
        assert!(m("*.txt;*\\", "a.txt") && d("*.txt;*\\", "x"));
    }

    #[test]
    fn umlauts_fold() {
        assert!(m("*ÄRGER*", "großer ärger.txt"));
    }
}
