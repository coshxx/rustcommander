//! Command line history in `history.json` next to `config.json`:
//! `{ "commands": [ newest, …, oldest ] }`.

use std::path::PathBuf;

use crate::config::dir;
use crate::config::json::{self, Value};

/// At most this many commands are kept.
pub const MAX: usize = 50;

fn file() -> Option<PathBuf> {
    dir().map(|d| d.join("history.json"))
}

/// The saved commands, newest first; empty if there is no (readable) file.
pub fn load() -> Vec<String> {
    file().and_then(|f| std::fs::read_to_string(f).ok()).map(|t| parse(&t)).unwrap_or_default()
}

fn parse(text: &str) -> Vec<String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(Value::Arr(items)) = json::parse(text).and_then(|v| v.get("commands").cloned()) else {
        return Vec::new();
    };
    items.iter().filter_map(Value::as_str).map(str::to_string).take(MAX).collect()
}

fn to_json(commands: &[String]) -> Value {
    let items = commands.iter().map(|c| Value::Str(c.clone())).collect();
    Value::Obj(vec![("commands".to_string(), Value::Arr(items))])
}

pub fn save(commands: &[String]) {
    let (Some(dir), Some(file)) = (dir(), file()) else { return };
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(file, to_json(commands).to_pretty());
}

/// Put `command` first (moving it up if it is there already) and drop the oldest beyond `MAX`.
pub fn push(commands: &mut Vec<String>, command: &str) {
    commands.retain(|c| c != command);
    commands.insert(0, command.to_string());
    commands.truncate(MAX);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_push() {
        let mut h = vec!["dir".to_string(), "git status".to_string()];
        push(&mut h, "git status");
        assert_eq!(h, ["git status", "dir"]);
        assert_eq!(parse(&to_json(&h).to_pretty()), h);
        for i in 0..60 {
            push(&mut h, &format!("echo {i}"));
        }
        assert_eq!(h.len(), MAX);
        assert_eq!(h[0], "echo 59");
        assert!(parse("{ broken").is_empty());
    }
}
