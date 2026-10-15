//! Settings file `%APPDATA%\RustCommander\config.json`. Missing or broken values fall back to
//! the defaults; unknown keys are ignored.

pub mod history;
pub(crate) mod json;

use std::path::PathBuf;

use self::json::Value;
use crate::files::sort::{Sort, SortKey};
use crate::i18n::Lang;
use crate::lister;
use crate::ops::pack::{Format, Level};

#[derive(Default)]
pub struct Config {
    pub paths: [Option<PathBuf>; 2],
    /// Sort column and direction per panel.
    pub sorts: [Sort; 2],
    pub active: usize,
    /// Normal window position (left, top, right, bottom) + maximized.
    pub window: Option<([i32; 4], bool)>,
    /// Last file mask for marking (Num+ / Num−).
    pub mask: Option<String>,
    /// Program for F4; the file is passed as its argument. `None` = Notepad.
    pub editor: Option<String>,
    /// Lister window position and wrapping.
    pub lister: lister::Settings,
    /// What can be switched in the options dialog (Ctrl+,).
    pub options: Options,
    /// Last choices in the pack and unpack dialogs.
    pub pack: PackSettings,
}

/// Choices of the pack (Alt+F5) and unpack (Alt+F9) dialogs, kept for the next time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PackSettings {
    pub format: Format,
    pub level: Level,
    /// Unpack each archive into a folder of its own.
    pub subdir: bool,
}

impl Default for PackSettings {
    fn default() -> PackSettings {
        PackSettings { format: Format::Zip, level: Level::Normal, subdir: true }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Options {
    /// Sizes in the file lists and status bars as exact byte counts (`48.318.382.080`).
    pub exact_sizes: bool,
    /// Quick search starts only with Alt+letter; plain letters then continue it.
    pub alt_search: bool,
    /// Folders are sorted by the chosen column too (by size: only counted ones).
    pub sort_dirs: bool,
    /// Space marks without moving the cursor down (Insert still moves).
    pub space_stays: bool,
    /// Command line above the key bar; typing goes there, quick search needs Alt.
    pub command_line: bool,
    /// UI language chosen in the options; `None` = follow the Windows language.
    pub language: Option<Lang>,
}

/// `{"left", "top", "right", "bottom", "maximized"}` → window placement.
fn parse_window(v: &Value) -> Option<([i32; 4], bool)> {
    let n = ["left", "top", "right", "bottom"].map(|k| v.get(k).and_then(Value::as_i32));
    let [Some(l), Some(t), Some(r), Some(b)] = n else { return None };
    let max = v.get("maximized").and_then(Value::as_bool).unwrap_or(false);
    (r > l && b > t).then_some(([l, t, r, b], max))
}

fn window_json((r, max): ([i32; 4], bool)) -> Value {
    let mut members: Vec<(String, Value)> =
        ["left", "top", "right", "bottom"].iter().zip(r).map(|(k, n)| (k.to_string(), Value::Num(n as f64))).collect();
    members.push(("maximized".into(), Value::Bool(max)));
    Value::Obj(members)
}

/// F4 editor unless config.json names another one.
pub const DEFAULT_EDITOR: &str = "notepad.exe";

/// `{"by": "size", "desc": true}` → sort order.
fn parse_sort(v: &Value) -> Option<Sort> {
    let key = SortKey::from_id(v.get("by")?.as_str()?)?;
    Some(Sort { key, desc: v.get("desc").and_then(Value::as_bool).unwrap_or(false) })
}

fn sort_json(s: Sort) -> Value {
    obj(vec![("by", Value::Str(s.key.id().into())), ("desc", Value::Bool(s.desc))])
}

fn obj(members: Vec<(&str, Value)>) -> Value {
    Value::Obj(members.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

/// `RC_CONFIG_DIR` overrides the location (used for tests, keeps the real config untouched).
pub fn dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("RC_CONFIG_DIR") {
        return Some(PathBuf::from(d));
    }
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("RustCommander"))
}

fn file() -> Option<PathBuf> {
    dir().map(|d| d.join("config.json"))
}

impl Config {
    pub fn load() -> Config {
        file().and_then(|f| std::fs::read_to_string(f).ok()).map(|t| Config::parse(&t)).unwrap_or_default()
    }

    fn parse(text: &str) -> Config {
        let mut cfg = Config::default();
        // Some editors (and PowerShell 5.1) start UTF-8 files with a byte order mark.
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let Some(root) = json::parse(text) else { return cfg };
        let str_at = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let bool_at = |v: &Value, k: &str| v.get(k).and_then(Value::as_bool);
        if let Some(p) = root.get("panels") {
            cfg.paths = [str_at(p, "left").map(PathBuf::from), str_at(p, "right").map(PathBuf::from)];
            cfg.active = (p.get("active").and_then(Value::as_i32) == Some(1)) as usize;
            for (i, k) in ["left_sort", "right_sort"].iter().enumerate() {
                cfg.sorts[i] = p.get(k).and_then(parse_sort).unwrap_or_default();
            }
        }
        cfg.window = root.get("window").and_then(parse_window);
        cfg.mask = str_at(&root, "mask");
        cfg.editor = str_at(&root, "editor").filter(|e| !e.trim().is_empty());
        if let Some(l) = root.get("lister") {
            cfg.lister.window = l.get("window").and_then(parse_window);
            cfg.lister.wrap = bool_at(l, "wrap").unwrap_or(cfg.lister.wrap);
        }
        if let Some(p) = root.get("pack") {
            let d = PackSettings::default();
            cfg.pack = PackSettings {
                format: str_at(p, "format").and_then(|f| Format::from_id(&f)).unwrap_or(d.format),
                level: str_at(p, "level").and_then(|l| Level::from_id(&l)).unwrap_or(d.level),
                subdir: bool_at(p, "subdir").unwrap_or(d.subdir),
            };
        }
        if let Some(o) = root.get("options") {
            let d = Options::default();
            cfg.options = Options {
                exact_sizes: bool_at(o, "exact_sizes").unwrap_or(d.exact_sizes),
                alt_search: bool_at(o, "alt_search").unwrap_or(d.alt_search),
                sort_dirs: bool_at(o, "sort_dirs").unwrap_or(d.sort_dirs),
                space_stays: bool_at(o, "space_stays").unwrap_or(d.space_stays),
                command_line: bool_at(o, "command_line").unwrap_or(d.command_line),
                language: str_at(o, "language").and_then(|l| Lang::from_id(&l)),
            };
        }
        cfg
    }

    fn to_json(&self) -> Value {
        let path = |p: &Option<PathBuf>| p.as_ref().map_or(Value::Null, |p| Value::Str(p.display().to_string()));
        let mut members = vec![(
            "panels",
            obj(vec![
                ("left", path(&self.paths[0])),
                ("right", path(&self.paths[1])),
                ("left_sort", sort_json(self.sorts[0])),
                ("right_sort", sort_json(self.sorts[1])),
                ("active", Value::Num(self.active as f64)),
            ]),
        )];
        if let Some(w) = self.window {
            members.push(("window", window_json(w)));
        }
        if let Some(m) = &self.mask {
            members.push(("mask", Value::Str(m.clone())));
        }
        // Always written, so it can be found and changed in the file.
        members.push(("editor", Value::Str(self.editor.clone().unwrap_or_else(|| DEFAULT_EDITOR.into()))));
        let mut lister = Vec::new();
        if let Some(w) = self.lister.window {
            lister.push(("window", window_json(w)));
        }
        lister.push(("wrap", Value::Bool(self.lister.wrap)));
        members.push(("lister", obj(lister)));
        let o = self.options;
        let mut options = vec![
            ("exact_sizes", Value::Bool(o.exact_sizes)),
            ("alt_search", Value::Bool(o.alt_search)),
            ("sort_dirs", Value::Bool(o.sort_dirs)),
            ("space_stays", Value::Bool(o.space_stays)),
            ("command_line", Value::Bool(o.command_line)),
        ];
        // Only once chosen; until then the Windows language is used.
        if let Some(l) = o.language {
            options.push(("language", Value::Str(l.id().into())));
        }
        members.push(("options", obj(options)));
        let p = self.pack;
        members.push((
            "pack",
            obj(vec![
                ("format", Value::Str(p.format.id().into())),
                ("level", Value::Str(p.level.id().into())),
                ("subdir", Value::Bool(p.subdir)),
            ]),
        ));
        obj(members)
    }

    pub fn save(&self) {
        let (Some(dir), Some(file)) = (dir(), file()) else { return };
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(file, self.to_json().to_pretty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let cfg = Config {
            paths: [Some(PathBuf::from(r"C:\a")), None],
            active: 1,
            window: Some(([10, 20, 1610, 920], true)),
            mask: Some("*.rs".into()),
            editor: Some(r"C:\Tools\npp\notepad++.exe".into()),
            lister: lister::Settings { window: Some(([-5, 0, 800, 600], false)), wrap: false },
            sorts: [Sort { key: SortKey::Size, desc: true }, Sort::default()],
            options: Options {
                exact_sizes: true,
                alt_search: false,
                sort_dirs: true,
                space_stays: true,
                command_line: true,
                language: Some(Lang::De),
            },
            pack: PackSettings { format: Format::SevenZip, level: Level::Max, subdir: false },
        };
        let back = Config::parse(&cfg.to_json().to_pretty());
        assert_eq!(back.paths, cfg.paths);
        assert_eq!(back.active, 1);
        assert_eq!(back.window, cfg.window);
        assert_eq!(back.mask, cfg.mask);
        assert_eq!(back.editor, cfg.editor);
        assert_eq!(back.lister.window, cfg.lister.window);
        assert!(!back.lister.wrap);
        assert_eq!(back.options, cfg.options);
        assert_eq!(back.sorts, cfg.sorts);
        assert_eq!(back.pack, cfg.pack);
    }

    #[test]
    fn bom_and_partial() {
        let cfg = Config::parse(
            "\u{feff}{\"panels\": {\"left\": \"C:\\\\a\"}, \"options\": {\"alt_search\": true}, \"x\": 1}",
        );
        assert_eq!(cfg.paths[0], Some(PathBuf::from(r"C:\a")));
        assert_eq!(cfg.paths[1], None);
        assert_eq!(cfg.active, 0);
        assert!(cfg.lister.wrap);
        assert_eq!(cfg.options, Options { alt_search: true, ..Options::default() });
        assert_eq!(cfg.sorts, [Sort::default(); 2]);
        assert_eq!(cfg.pack, PackSettings::default());
    }

    #[test]
    fn broken_file_gives_defaults() {
        let cfg = Config::parse("{\"panels\": {\"left\": ");
        assert_eq!(cfg.paths, [None, None]);
        assert_eq!(cfg.options, Options::default());
        // Invalid window rectangles are dropped.
        let cfg = Config::parse(r#"{"window": {"left": 100, "top": 0, "right": 50, "bottom": 10}}"#);
        assert_eq!(cfg.window, None);
    }
}
