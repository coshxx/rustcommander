//! Tiny `key=value` file at `%APPDATA%\RustCommander\config.ini`.

use std::path::PathBuf;

#[derive(Default)]
pub struct Config {
    pub paths: [Option<PathBuf>; 2],
    pub active: usize,
    /// Normal window position (left, top, right, bottom) + maximized.
    pub window: Option<([i32; 4], bool)>,
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
        let mut cfg = Config::default();
        let Some(text) = file().and_then(|f| std::fs::read_to_string(f).ok()) else {
            return cfg;
        };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match k.trim() {
                "left" => cfg.paths[0] = Some(PathBuf::from(v.trim())),
                "right" => cfg.paths[1] = Some(PathBuf::from(v.trim())),
                "active" => cfg.active = (v.trim() == "1") as usize,
                "window" => {
                    let n: Vec<i32> = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                    if n.len() == 5 && n[2] > n[0] && n[3] > n[1] {
                        cfg.window = Some(([n[0], n[1], n[2], n[3]], n[4] != 0));
                    }
                }
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
        if let Some((r, max)) = self.window {
            out += &format!("window={},{},{},{},{}\n", r[0], r[1], r[2], r[3], max as i32);
        }
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(file, out);
    }
}
