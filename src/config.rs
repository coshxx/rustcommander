//! Tiny `key=value` file at `%APPDATA%\RustCommander\config.ini`.

use std::path::PathBuf;

use crate::lister;

#[derive(Default)]
pub struct Config {
    pub paths: [Option<PathBuf>; 2],
    pub active: usize,
    /// Normal window position (left, top, right, bottom) + maximized.
    pub window: Option<([i32; 4], bool)>,
    /// Last file mask for marking (Num+ / Num−).
    pub mask: Option<String>,
    /// Lister window position and wrapping.
    pub lister: lister::Settings,
}

/// `left,top,right,bottom,maximized` → window placement.
fn parse_window(v: &str) -> Option<([i32; 4], bool)> {
    let n: Vec<i32> = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
    (n.len() == 5 && n[2] > n[0] && n[3] > n[1]).then(|| ([n[0], n[1], n[2], n[3]], n[4] != 0))
}

fn format_window((r, max): ([i32; 4], bool)) -> String {
    format!("{},{},{},{},{}", r[0], r[1], r[2], r[3], max as i32)
}

/// `RC_CONFIG_DIR` overrides the location (used for tests, keeps the real config untouched).
pub fn dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("RC_CONFIG_DIR") {
        return Some(PathBuf::from(d));
    }
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("RustCommander"))
}

fn file() -> Option<PathBuf> {
    dir().map(|d| d.join("config.ini"))
}

impl Config {
    pub fn load() -> Config {
        file().and_then(|f| std::fs::read_to_string(f).ok()).map(|t| Config::parse(&t)).unwrap_or_default()
    }

    fn parse(text: &str) -> Config {
        let mut cfg = Config::default();
        // Some editors (and PowerShell 5.1) start UTF-8 files with a byte order mark; it would
        // otherwise become part of the first key.
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match k.trim() {
                "left" => cfg.paths[0] = Some(PathBuf::from(v.trim())),
                "right" => cfg.paths[1] = Some(PathBuf::from(v.trim())),
                "mask" => cfg.mask = Some(v.trim().to_string()),
                "active" => cfg.active = (v.trim() == "1") as usize,
                "window" => cfg.window = parse_window(v),
                "lister_window" => cfg.lister.window = parse_window(v),
                "lister_wrap" => cfg.lister.wrap = v.trim() != "0",
                _ => {}
            }
        }
        cfg
    }

    pub fn save(&self) {
        let (Some(dir), Some(file)) = (dir(), file()) else { return };
        let mut out = String::new();
        for (key, p) in ["left", "right"].iter().zip(&self.paths) {
            if let Some(p) = p {
                out += &format!("{key}={}\n", p.display());
            }
        }
        out += &format!("active={}\n", self.active);
        if let Some(w) = self.window {
            out += &format!("window={}\n", format_window(w));
        }
        if let Some(w) = self.lister.window {
            out += &format!("lister_window={}\n", format_window(w));
        }
        out += &format!("lister_wrap={}\n", self.lister.wrap as i32);
        if let Some(m) = &self.mask {
            out += &format!("mask={m}\n");
        }
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(file, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_with_and_without_bom() {
        for text in ["left=C:\\a\nright=D:\\b\nactive=1\n", "\u{feff}left=C:\\a\nright=D:\\b\nactive=1\n"] {
            let cfg = Config::parse(text);
            assert_eq!(cfg.paths[0], Some(PathBuf::from(r"C:\a")));
            assert_eq!(cfg.paths[1], Some(PathBuf::from(r"D:\b")));
            assert_eq!(cfg.active, 1);
        }
    }
}
