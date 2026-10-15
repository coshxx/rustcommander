//! Minimal JSON: parse into a `Value` tree and write it back indented. Enough for the config
//! file (objects, arrays, strings, numbers, bools, null); keeps the crate free of serde.

use std::fmt::Write;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    /// Keys in file order.
    Obj(Vec<(String, Value)>),
}

impl Value {
    /// Member `key` of an object.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        if let Value::Str(s) = self { Some(s) } else { None }
    }

    pub fn as_bool(&self) -> Option<bool> {
        if let Value::Bool(b) = self { Some(*b) } else { None }
    }

    /// Whole numbers that fit into an `i32`.
    pub fn as_i32(&self) -> Option<i32> {
        match self {
            Value::Num(n) if n.fract() == 0.0 && *n >= i32::MIN as f64 && *n <= i32::MAX as f64 => Some(*n as i32),
            _ => None,
        }
    }

    /// Indented with two spaces, ends with a newline.
    pub fn to_pretty(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out.push('\n');
        out
    }

    fn write(&self, out: &mut String, indent: usize) {
        let pad = |out: &mut String, n: usize| out.extend(std::iter::repeat_n("  ", n));
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Num(n) if n.fract() == 0.0 && n.abs() < 1e15 => {
                let _ = write!(out, "{}", *n as i64);
            }
            Value::Num(n) if n.is_finite() => {
                let _ = write!(out, "{n}");
            }
            Value::Num(_) => out.push_str("null"),
            Value::Str(s) => write_str(out, s),
            Value::Arr(items) if items.is_empty() => out.push_str("[]"),
            Value::Arr(items) => {
                out.push_str("[\n");
                for (i, v) in items.iter().enumerate() {
                    pad(out, indent + 1);
                    v.write(out, indent + 1);
                    out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
                }
                pad(out, indent);
                out.push(']');
            }
            Value::Obj(members) if members.is_empty() => out.push_str("{}"),
            Value::Obj(members) => {
                out.push_str("{\n");
                for (i, (k, v)) in members.iter().enumerate() {
                    pad(out, indent + 1);
                    write_str(out, k);
                    out.push_str(": ");
                    v.write(out, indent + 1);
                    out.push_str(if i + 1 < members.len() { ",\n" } else { "\n" });
                }
                pad(out, indent);
                out.push('}');
            }
        }
    }
}

fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The whole text must be one value (whitespace around it is fine).
pub fn parse(text: &str) -> Option<Value> {
    let mut p = Parser { s: text.as_bytes(), i: 0, depth: 0 };
    let v = p.value()?;
    p.ws();
    (p.i == p.s.len()).then_some(v)
}

/// Nesting limit, so a broken file can't overflow the stack.
const MAX_DEPTH: usize = 64;

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    depth: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, b: u8) -> bool {
        self.ws();
        let hit = self.peek() == Some(b);
        if hit {
            self.i += 1;
        }
        hit
    }

    fn literal(&mut self, word: &str, v: Value) -> Option<Value> {
        self.s[self.i..].starts_with(word.as_bytes()).then(|| {
            self.i += word.len();
            v
        })
    }

    fn value(&mut self) -> Option<Value> {
        self.ws();
        match self.peek()? {
            b'{' => self.nested(Self::object),
            b'[' => self.nested(Self::array),
            b'"' => self.string().map(Value::Str),
            b't' => self.literal("true", Value::Bool(true)),
            b'f' => self.literal("false", Value::Bool(false)),
            b'n' => self.literal("null", Value::Null),
            _ => self.number(),
        }
    }

    fn nested(&mut self, f: fn(&mut Self) -> Option<Value>) -> Option<Value> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return None;
        }
        let v = f(self);
        self.depth -= 1;
        v
    }

    fn object(&mut self) -> Option<Value> {
        self.i += 1;
        let mut members = Vec::new();
        if self.eat(b'}') {
            return Some(Value::Obj(members));
        }
        loop {
            self.ws();
            let k = self.string()?;
            if !self.eat(b':') {
                return None;
            }
            members.push((k, self.value()?));
            if self.eat(b'}') {
                return Some(Value::Obj(members));
            }
            if !self.eat(b',') {
                return None;
            }
        }
    }

    fn array(&mut self) -> Option<Value> {
        self.i += 1;
        let mut items = Vec::new();
        if self.eat(b']') {
            return Some(Value::Arr(items));
        }
        loop {
            items.push(self.value()?);
            if self.eat(b']') {
                return Some(Value::Arr(items));
            }
            if !self.eat(b',') {
                return None;
            }
        }
    }

    fn string(&mut self) -> Option<String> {
        if self.peek() != Some(b'"') {
            return None;
        }
        self.i += 1;
        let mut out = String::new();
        loop {
            // Copy the run up to the next quote or escape in one piece (keeps UTF-8 intact).
            let start = self.i;
            while !matches!(self.peek()?, b'"' | b'\\') {
                if self.s[self.i] < 0x20 {
                    return None;
                }
                self.i += 1;
            }
            out.push_str(std::str::from_utf8(&self.s[start..self.i]).ok()?);
            if self.s[self.i] == b'"' {
                self.i += 1;
                return Some(out);
            }
            self.i += 1;
            let c = match self.peek()? {
                b'"' => '"',
                b'\\' => '\\',
                b'/' => '/',
                b'b' => '\u{8}',
                b'f' => '\u{c}',
                b'n' => '\n',
                b'r' => '\r',
                b't' => '\t',
                b'u' => {
                    let hi = self.hex4()?;
                    let code = if (0xD800..0xDC00).contains(&hi) {
                        // Surrogate pair: must be followed by \uDC00–\uDFFF.
                        if !self.s[self.i + 1..].starts_with(b"\\u") {
                            return None;
                        }
                        self.i += 2;
                        let lo = self.hex4()?;
                        if !(0xDC00..0xE000).contains(&lo) {
                            return None;
                        }
                        0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                    } else {
                        hi
                    };
                    char::from_u32(code)?
                }
                _ => return None,
            };
            out.push(c);
            self.i += 1;
        }
    }

    /// The four hex digits after `\u`; leaves `i` on the last one.
    fn hex4(&mut self) -> Option<u32> {
        let digits = self.s.get(self.i + 1..self.i + 5)?;
        let v = u32::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()?;
        self.i += 4;
        Some(v)
    }

    fn number(&mut self) -> Option<Value> {
        let start = self.i;
        while matches!(self.peek(), Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')) {
            self.i += 1;
        }
        std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok().filter(|n: &f64| n.is_finite()).map(Value::Num)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let v = Value::Obj(vec![
            ("path".into(), Value::Str(r#"C:\Users\"x"\ä 😀"#.into())),
            ("n".into(), Value::Num(-12.0)),
            ("f".into(), Value::Num(1.5)),
            ("b".into(), Value::Bool(true)),
            ("none".into(), Value::Null),
            ("arr".into(), Value::Arr(vec![Value::Num(1.0), Value::Arr(vec![]), Value::Obj(vec![])])),
        ]);
        let text = v.to_pretty();
        assert!(text.contains("\"n\": -12,"));
        assert_eq!(parse(&text), Some(v));
    }

    #[test]
    fn escapes() {
        let v = parse(r#"{"a": "x\u00e4\ud83d\ude00\n\/"}"#).unwrap();
        assert_eq!(v.get("a").and_then(Value::as_str), Some("xä😀\n/"));
    }

    #[test]
    fn broken_input() {
        for text in ["", "{", "{\"a\" 1}", "{\"a\": 1,}", "[1 2]", "\"abc", "tru", "{} x", r#""\ud83d""#, "1e999"] {
            assert_eq!(parse(text), None, "{text}");
        }
        assert_eq!(parse(&"[".repeat(1000)), None);
    }

    #[test]
    fn accessors() {
        let v = parse(r#"{"i": 1920, "f": 1.5, "big": 1e12}"#).unwrap();
        assert_eq!(v.get("i").and_then(Value::as_i32), Some(1920));
        assert_eq!(v.get("f").and_then(Value::as_i32), None);
        assert_eq!(v.get("big").and_then(Value::as_i32), None);
        assert_eq!(v.get("missing"), None);
    }
}
