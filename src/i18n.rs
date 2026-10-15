//! Translations. Every text the UI shows lives in `i18n/<lang>.json`, embedded at compile time
//! and looked up by dotted key (`"dialog.copy"`). Placeholders are named (`{name}`); counts use
//! `<key>_one` / `<key>_other`. A key missing in a language falls back to English, then to the
//! key itself. Number and date formats follow the language too (see `app::format`).

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::config::json::{self, Value};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    En,
    De,
}

impl Lang {
    /// In the order of the options dialog.
    pub const ALL: [Lang; 2] = [Lang::En, Lang::De];

    /// Name in config.json.
    pub fn id(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::De => "de",
        }
    }

    pub fn from_id(s: &str) -> Option<Lang> {
        Lang::ALL.into_iter().find(|l| l.id() == s)
    }

    /// The language's own name (shown untranslated in the options).
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::De => "Deutsch",
        }
    }

    fn source(self) -> &'static str {
        match self {
            Lang::En => include_str!("../i18n/en.json"),
            Lang::De => include_str!("../i18n/de.json"),
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn set(lang: Lang) {
    CURRENT.store(lang as u8, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    if CURRENT.load(Ordering::Relaxed) == Lang::De as u8 { Lang::De } else { Lang::En }
}

/// Windows display language: German → German, everything else → English.
pub fn detect() -> Lang {
    const LANG_GERMAN: u16 = 0x07;
    let id = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
    if id & 0x3ff == LANG_GERMAN { Lang::De } else { Lang::En }
}

/// All texts of `lang`, flattened to dotted keys; parsed on first use.
fn table(lang: Lang) -> &'static HashMap<String, String> {
    static TABLES: [OnceLock<HashMap<String, String>>; 2] = [OnceLock::new(), OnceLock::new()];
    TABLES[lang as usize].get_or_init(|| {
        let mut out = HashMap::new();
        if let Some(root) = json::parse(lang.source()) {
            flatten(&root, String::new(), &mut out);
        }
        out
    })
}

fn flatten(v: &Value, prefix: String, out: &mut HashMap<String, String>) {
    match v {
        Value::Obj(members) => {
            for (k, v) in members {
                let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                flatten(v, key, out);
            }
        }
        Value::Str(s) => {
            out.insert(prefix, s.clone());
        }
        _ => {}
    }
}

/// The text for `key` in the current language (English if it is missing there).
pub fn lookup(key: &str) -> Option<&'static str> {
    let found = table(lang()).get(key).or_else(|| table(Lang::En).get(key));
    debug_assert!(found.is_some(), "missing translation: {key}");
    found.map(String::as_str)
}

/// The text for `key` in the current language; the key itself if there is none.
pub fn t(key: &str) -> &str {
    lookup(key).unwrap_or(key)
}

/// `t(key)` with `{name}` placeholders filled in.
pub fn tf(key: &str, args: &[(&str, &dyn Display)]) -> String {
    fill(t(key), args)
}

/// `<key>_one` or `<key>_other` for `n`, with `{n}` filled in.
pub fn tn(key: &str, n: u64) -> String {
    let form = if n == 1 { "_one" } else { "_other" };
    fill(t(&format!("{key}{form}")), &[("n", &n)])
}

fn fill(text: &str, args: &[(&str, &dyn Display)]) -> String {
    let mut out = text.to_string();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), &value.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_have_the_same_keys() {
        let mut en: Vec<&String> = table(Lang::En).keys().collect();
        let mut de: Vec<&String> = table(Lang::De).keys().collect();
        en.sort();
        de.sort();
        assert!(en.len() > 100, "en.json didn't parse");
        assert_eq!(en, de);
    }

    #[test]
    fn placeholders() {
        assert_eq!(fill("Copy {what} to:", &[("what", &"3 items")]), "Copy 3 items to:");
        let en = table(Lang::En);
        assert_eq!(fill(&en["panel.files_one"], &[("n", &1)]), "1 file");
        assert_eq!(fill(&en["panel.files_other"], &[("n", &2)]), "2 files");
    }
}
