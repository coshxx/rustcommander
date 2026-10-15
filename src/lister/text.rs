//! How the lister turns bytes into screen lines – pure logic, no windows.
//!
//! The view is described by the byte offset of its first line (`top`), never by a line number:
//! counting lines would mean reading the whole file. Every step therefore works locally from a
//! byte offset: `line` decodes one screen line, `back` finds the lines before an offset, `align`
//! snaps any offset (scrollbar, search hit) to the start of its line.

/// How the bytes are shown.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Lines end at line breaks; long lines wrap at the window edge (or not, see `wrap`).
    Text,
    /// Fixed-width lines of `BINARY_WIDTH` bytes, line breaks ignored.
    Binary,
    /// Offset, `HEX_WIDTH` bytes as hex digits, the same bytes as characters.
    Hex,
}

/// Character encoding in text mode. Binary and hex mode always show single bytes (ANSI or DOS).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Encoding {
    /// The system's Windows code page (e.g. Windows-1252).
    Ansi,
    /// The system's DOS code page (e.g. 850).
    Oem,
    Utf8,
    Utf16Le,
    Utf16Be,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Text => "Text",
            Mode::Binary => crate::i18n::t("lister.binary"),
            Mode::Hex => "Hex",
        }
    }
}

impl Encoding {
    pub fn label(self) -> &'static str {
        match self {
            Encoding::Ansi => "ANSI",
            Encoding::Oem => "DOS",
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf16Le => "UTF-16 LE",
            Encoding::Utf16Be => "UTF-16 BE",
        }
    }

    /// The single-byte encoding used in binary and hex mode (and for searching there).
    pub fn single_byte(self) -> Encoding {
        if self == Encoding::Oem { Encoding::Oem } else { Encoding::Ansi }
    }
}

/// Characters of the two single-byte code pages, indexed by byte.
pub struct CodePages {
    pub ansi: [u16; 256],
    pub oem: [u16; 256],
}

impl CodePages {
    pub fn table(&self, enc: Encoding) -> &[u16; 256] {
        if enc == Encoding::Oem { &self.oem } else { &self.ansi }
    }
}

pub const BINARY_WIDTH: usize = 80;
pub const HEX_WIDTH: usize = 16;
/// Without wrapping, lines are still broken after this many characters (a file without line
/// breaks would otherwise be one endless line).
pub const MAX_LINE: usize = 4096;
/// How far back to look for a line break. Lines longer than this start at an arbitrary point
/// when scrolling up into them.
const MAX_BACK: usize = 64 * 1024;
const TAB: usize = 8;
/// Shown for control characters.
const DOT: u16 = 0x00B7;

/// One screen line.
pub struct Line {
    /// Where the next line starts.
    pub end: usize,
    /// What to draw: one UTF-16 unit per column (tabs already expanded).
    pub text: Vec<u16>,
    /// For each unit of `text`, the byte offset it came from (`usize::MAX` for spacing), so a
    /// search hit can be highlighted.
    pub offs: Vec<usize>,
}

/// The file plus the current way of showing it.
pub struct Doc<'a> {
    pub data: &'a [u8],
    pub mode: Mode,
    pub enc: Encoding,
    pub wrap: bool,
    /// Columns that fit into the window (wrap width).
    pub cols: usize,
    pub pages: &'a CodePages,
}

/// Guess how to show a file from its first 64 KB: byte order marks, UTF-16 without BOM,
/// binary (NUL bytes or many control codes, shown in hex), valid UTF-8, otherwise ANSI.
pub fn detect(data: &[u8]) -> (Mode, Encoding) {
    let head = &data[..data.len().min(64 * 1024)];
    if head.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return (Mode::Text, Encoding::Utf8);
    }
    if head.starts_with(&[0xFF, 0xFE]) {
        return (Mode::Text, Encoding::Utf16Le);
    }
    if head.starts_with(&[0xFE, 0xFF]) {
        return (Mode::Text, Encoding::Utf16Be);
    }
    if head.len() >= 4 {
        let half = head.len() / 2;
        let zeros = |odd: usize| head.iter().skip(odd).step_by(2).filter(|&&b| b == 0).count();
        let (even, odd) = (zeros(0), zeros(1));
        // Mostly-ASCII UTF-16: every other byte is zero.
        if odd * 10 > half * 4 && even * 20 < half {
            return (Mode::Text, Encoding::Utf16Le);
        }
        if even * 10 > half * 4 && odd * 20 < half {
            return (Mode::Text, Encoding::Utf16Be);
        }
    }
    let enc = if valid_utf8(head, head.len() < data.len()) { Encoding::Utf8 } else { Encoding::Ansi };
    let control = head.iter().filter(|&&b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0C | 0x1B)).count();
    if head.contains(&0) || control * 20 > head.len() { (Mode::Hex, enc) } else { (Mode::Text, enc) }
}

/// Valid UTF-8; `cut` = the sample ends mid-file, so a character may be cut off at its end.
fn valid_utf8(head: &[u8], cut: bool) -> bool {
    match std::str::from_utf8(head) {
        Ok(_) => true,
        Err(e) => cut && e.error_len().is_none(),
    }
}

impl Doc<'_> {
    fn len(&self) -> usize {
        self.data.len()
    }

    /// Where text starts: after the byte order mark, if it matches the encoding.
    pub fn base(&self) -> usize {
        if self.mode != Mode::Text {
            return 0;
        }
        let bom: &[u8] = match self.enc {
            Encoding::Utf8 => &[0xEF, 0xBB, 0xBF],
            Encoding::Utf16Le => &[0xFF, 0xFE],
            Encoding::Utf16Be => &[0xFE, 0xFF],
            _ => &[],
        };
        if !bom.is_empty() && self.data.starts_with(bom) { bom.len() } else { 0 }
    }

    /// Bytes per code unit: 2 for UTF-16 text, else 1. Line starts are always unit-aligned.
    pub fn unit(&self) -> usize {
        match (self.mode, self.enc) {
            (Mode::Text, Encoding::Utf16Le | Encoding::Utf16Be) => 2,
            _ => 1,
        }
    }

    /// Bytes per line in binary and hex mode.
    fn record(&self) -> usize {
        if self.mode == Mode::Hex { HEX_WIDTH } else { BINARY_WIDTH }
    }

    /// Hex digits of the offset column.
    fn offset_digits(&self) -> usize {
        let bits = usize::BITS - self.len().leading_zeros();
        (bits as usize).div_ceil(4).max(8)
    }

    /// Widest line in columns (for horizontal scrolling); `None` = unknown (text).
    pub fn content_width(&self) -> Option<usize> {
        match self.mode {
            Mode::Text if self.wrap => Some(self.cols),
            Mode::Text => None,
            Mode::Binary => Some(BINARY_WIDTH),
            Mode::Hex => Some(self.offset_digits() + 2 + HEX_WIDTH * 3 + 2 + HEX_WIDTH),
        }
    }

    /// The character at `pos` in text mode: (code point, length in bytes). Invalid data gives
    /// U+FFFD and moves on by one unit.
    fn decode(&self, pos: usize) -> (u32, usize) {
        let d = &self.data[pos..];
        match self.enc {
            Encoding::Ansi | Encoding::Oem => (self.pages.table(self.enc)[d[0] as usize] as u32, 1),
            Encoding::Utf8 => decode_utf8(d),
            Encoding::Utf16Le | Encoding::Utf16Be => {
                let unit = |i: usize| -> Option<u32> {
                    let b = d.get(i..i + 2)?;
                    Some(if self.enc == Encoding::Utf16Le {
                        u16::from_le_bytes([b[0], b[1]])
                    } else {
                        u16::from_be_bytes([b[0], b[1]])
                    } as u32)
                };
                match unit(0) {
                    None => (0xFFFD, d.len()),
                    Some(hi @ 0xD800..=0xDBFF) => match unit(2) {
                        Some(lo @ 0xDC00..=0xDFFF) => (0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00), 4),
                        _ => (0xFFFD, 2),
                    },
                    Some(0xDC00..=0xDFFF) => (0xFFFD, 2),
                    Some(c) => (c, 2),
                }
            }
        }
    }

    /// The unit just before `q` is a line feed.
    fn newline_before(&self, q: usize) -> bool {
        match (self.unit(), self.enc) {
            (2, Encoding::Utf16Le) => self.data[q - 2] == b'\n' && self.data[q - 1] == 0,
            (2, _) => self.data[q - 2] == 0 && self.data[q - 1] == b'\n',
            _ => self.data[q - 1] == b'\n',
        }
    }

    /// Move `pos` back to the start of the character it is in.
    fn char_align(&self, pos: usize) -> usize {
        let base = self.base();
        let pos = pos.min(self.len()).max(base);
        match (self.mode, self.enc) {
            (Mode::Text, Encoding::Utf8) => {
                let mut q = pos;
                while q > base && pos - q < 3 && self.data.get(q).is_some_and(|b| b & 0xC0 == 0x80) {
                    q -= 1;
                }
                q
            }
            _ => pos - (pos - base) % self.unit(),
        }
    }

    /// Start of the text line (between line breaks) that contains `pos`.
    fn logical_start(&self, pos: usize) -> usize {
        let base = self.base();
        let floor = pos.saturating_sub(MAX_BACK).max(base);
        let mut q = self.char_align(pos);
        while q > floor {
            if self.newline_before(q) {
                return q;
            }
            q -= self.unit();
        }
        self.char_align(floor)
    }

    /// The screen line starting at `start`.
    pub fn line(&self, start: usize) -> Line {
        match self.mode {
            Mode::Text => self.text_line(start),
            Mode::Binary => self.binary_line(start),
            Mode::Hex => self.hex_line(start),
        }
    }

    fn text_line(&self, start: usize) -> Line {
        let limit = if self.wrap { self.cols.max(1) } else { MAX_LINE };
        let mut line = Line { end: start, text: Vec::new(), offs: Vec::new() };
        let mut col = 0;
        // Last place to wrap: after a space (length of `text`, byte offset after the space).
        let mut wrap_at: Option<(usize, usize)> = None;
        let mut p = start;
        while p < self.len() {
            let (c, n) = self.decode(p);
            if c == '\n' as u32 {
                line.end = p + n;
                return line;
            }
            if c == '\r' as u32 {
                p += n;
                continue;
            }
            let width = if c == '\t' as u32 { (TAB - col % TAB).min(limit) } else { 1 };
            if col + width > limit {
                if self.wrap
                    && let Some((len, after)) = wrap_at
                {
                    line.text.truncate(len);
                    line.offs.truncate(len);
                    line.end = after;
                } else {
                    line.end = p;
                }
                return line;
            }
            if c == '\t' as u32 {
                line.text.extend(std::iter::repeat_n(b' ' as u16, width));
                line.offs.extend(std::iter::repeat_n(p, width));
            } else {
                push_char(&mut line, display(c), p);
            }
            col += width;
            p += n;
            if c == ' ' as u32 || c == '\t' as u32 {
                wrap_at = Some((line.text.len(), p));
            }
        }
        line.end = p;
        line
    }

    fn binary_line(&self, start: usize) -> Line {
        let end = (start + BINARY_WIDTH).min(self.len());
        let table = self.pages.table(self.enc);
        let mut line = Line { end, text: Vec::with_capacity(BINARY_WIDTH), offs: Vec::new() };
        for (i, &b) in self.data[start..end].iter().enumerate() {
            line.text.push(display_unit(table[b as usize]));
            line.offs.push(start + i);
        }
        line
    }

    fn hex_line(&self, start: usize) -> Line {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        let end = (start + HEX_WIDTH).min(self.len());
        let table = self.pages.table(self.enc);
        let mut line = Line { end, text: Vec::with_capacity(80), offs: Vec::with_capacity(80) };
        let push = |line: &mut Line, c: u16, off: usize| {
            line.text.push(c);
            line.offs.push(off);
        };
        for c in format!("{start:0w$X}  ", w = self.offset_digits()).encode_utf16() {
            push(&mut line, c, usize::MAX);
        }
        for i in 0..HEX_WIDTH {
            if i == HEX_WIDTH / 2 {
                push(&mut line, b' ' as u16, usize::MAX);
            }
            match self.data.get(start + i).filter(|_| start + i < end) {
                Some(&b) => {
                    push(&mut line, HEX[(b >> 4) as usize] as u16, start + i);
                    push(&mut line, HEX[(b & 15) as usize] as u16, start + i);
                }
                None => {
                    push(&mut line, b' ' as u16, usize::MAX);
                    push(&mut line, b' ' as u16, usize::MAX);
                }
            }
            push(&mut line, b' ' as u16, usize::MAX);
        }
        push(&mut line, b' ' as u16, usize::MAX);
        for (i, &b) in self.data[start..end].iter().enumerate() {
            let c = if (0x20..0x7F).contains(&b) { b as u16 } else { table[b as usize] };
            push(&mut line, if b < 0x20 || b == 0x7F { b'.' as u16 } else { display_unit(c) }, start + i);
        }
        line
    }

    /// Start of the line after the one starting at `pos`.
    pub fn next(&self, pos: usize) -> usize {
        match self.mode {
            Mode::Text => self.text_line(pos).end,
            _ => (pos + self.record()).min(self.len()),
        }
    }

    /// Start of the line `n` lines above the line starting at `pos` (stops at the beginning).
    pub fn back(&self, mut pos: usize, mut n: usize) -> usize {
        let base = self.base();
        if self.mode != Mode::Text {
            // `pos` at the end counts as the start of the line after the last one.
            let pos =
                if pos >= self.len() { self.len().div_ceil(self.record()) * self.record() } else { self.align(pos) };
            return pos.saturating_sub(n * self.record());
        }
        while n > 0 && pos > base {
            // All screen lines of the text line before `pos`, then count back from its end.
            let start = self.logical_start(pos - 1);
            let mut starts = vec![start];
            loop {
                let s = *starts.last().unwrap_or(&start);
                let e = self.next(s);
                if e >= pos || e <= s {
                    break;
                }
                starts.push(e);
            }
            if n <= starts.len() {
                return starts[starts.len() - n];
            }
            n -= starts.len();
            pos = start;
        }
        pos.max(base)
    }

    /// Start of the screen line containing byte `pos`.
    pub fn align(&self, pos: usize) -> usize {
        let base = self.base();
        let len = self.len();
        if self.mode != Mode::Text {
            let last = len.saturating_sub(1);
            let pos = pos.min(last);
            return pos - pos % self.record();
        }
        if pos >= len {
            return if len > base { self.back(len, 1) } else { base };
        }
        let mut s = self.logical_start(pos);
        loop {
            let e = self.next(s);
            if e > pos || e <= s {
                return s;
            }
            s = e;
        }
    }

    /// `top` of the last full page: the view shows the end of the file.
    pub fn last_top(&self, rows: usize) -> usize {
        self.back(self.len(), rows.max(1))
    }

    /// Where the view starting at `top` with `rows` lines ends.
    pub fn view_end(&self, top: usize, rows: usize) -> usize {
        let mut p = top;
        for _ in 0..rows {
            if p >= self.len() {
                break;
            }
            p = self.next(p);
        }
        p
    }
}

/// Code point → what to draw: control characters as a dot.
fn display(c: u32) -> u32 {
    if c < 0x20 || (0x7F..0xA0).contains(&c) { DOT as u32 } else { c }
}

fn display_unit(c: u16) -> u16 {
    display(c as u32) as u16
}

fn push_char(line: &mut Line, c: u32, off: usize) {
    let mut buf = [0u16; 2];
    let s = char::from_u32(c).unwrap_or('\u{FFFD}').encode_utf16(&mut buf);
    for &u in s.iter() {
        line.text.push(u);
        line.offs.push(off);
    }
}

/// One UTF-8 character; invalid bytes become U+FFFD one at a time.
fn decode_utf8(d: &[u8]) -> (u32, usize) {
    let b0 = d[0];
    let (n, init) = match b0 {
        0x00..=0x7F => return (b0 as u32, 1),
        0xC2..=0xDF => (2, (b0 & 0x1F) as u32),
        0xE0..=0xEF => (3, (b0 & 0x0F) as u32),
        0xF0..=0xF4 => (4, (b0 & 0x07) as u32),
        _ => return (0xFFFD, 1),
    };
    if d.len() < n {
        return (0xFFFD, 1);
    }
    let mut c = init;
    for &b in &d[1..n] {
        if b & 0xC0 != 0x80 {
            return (0xFFFD, 1);
        }
        c = (c << 6) | (b & 0x3F) as u32;
    }
    let too_small = match n {
        3 => c < 0x800,
        4 => c < 0x10000,
        _ => false,
    };
    if too_small || (0xD800..0xE000).contains(&c) || c > 0x10FFFF { (0xFFFD, 1) } else { (c, n) }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Latin-1 stands in for the system code pages.
    pub(crate) fn pages() -> CodePages {
        let mut t = [0u16; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            *slot = i as u16;
        }
        CodePages { ansi: t, oem: t }
    }

    fn doc<'a>(data: &'a [u8], pages: &'a CodePages, mode: Mode, enc: Encoding, wrap: bool, cols: usize) -> Doc<'a> {
        Doc { data, mode, enc, wrap, cols, pages }
    }

    fn text(l: &Line) -> String {
        String::from_utf16_lossy(&l.text)
    }

    /// All screen lines, front to back.
    fn lines(d: &Doc) -> Vec<String> {
        let mut out = Vec::new();
        let mut p = d.base();
        while p < d.data.len() {
            let l = d.line(p);
            out.push(text(&l));
            assert!(l.end > p);
            p = l.end;
        }
        out
    }

    #[test]
    fn detects_encodings_and_binary() {
        assert_eq!(detect(b"hello\r\nworld"), (Mode::Text, Encoding::Utf8));
        assert_eq!(detect("grüße".as_bytes()), (Mode::Text, Encoding::Utf8));
        assert_eq!(detect(b"gr\xfc\xdfe"), (Mode::Text, Encoding::Ansi));
        assert_eq!(detect(b"\xef\xbb\xbfx"), (Mode::Text, Encoding::Utf8));
        assert_eq!(detect(b"\xff\xfex\x00"), (Mode::Text, Encoding::Utf16Le));
        assert_eq!(detect(b"h\x00e\x00l\x00l\x00o\x00"), (Mode::Text, Encoding::Utf16Le));
        assert_eq!(detect(b"MZ\x90\x00\x03\x00\x00\x00\x04\x00\x00\x00\xff\xff").0, Mode::Hex);
        assert_eq!(detect(b""), (Mode::Text, Encoding::Utf8));
    }

    #[test]
    fn text_lines_wrap_at_words() {
        let p = pages();
        let data = b"one two three\r\n\r\nfour\tx\nabcdefghijkl";
        let d = doc(data, &p, Mode::Text, Encoding::Ansi, true, 8);
        assert_eq!(lines(&d), ["one two ", "three", "", "four    ", "x", "abcdefgh", "ijkl"]);
        let d = doc(data, &p, Mode::Text, Encoding::Ansi, false, 8);
        assert_eq!(lines(&d), ["one two three", "", "four    x", "abcdefghijkl"]);
    }

    #[test]
    fn back_and_align_agree_with_next() {
        let p = pages();
        let data = "Zeile eins ist lang genug zum Umbrechen\n\nkurz\nnoch eine längere Zeile hier\nEnde".as_bytes();
        for wrap in [true, false] {
            let d = doc(data, &p, Mode::Text, Encoding::Utf8, wrap, 10);
            let mut starts = vec![0];
            while d.next(*starts.last().unwrap()) < data.len() {
                starts.push(d.next(*starts.last().unwrap()));
            }
            for (i, &s) in starts.iter().enumerate() {
                for n in 0..=i {
                    assert_eq!(d.back(s, n), starts[i - n], "wrap={wrap} line {i} back {n}");
                }
                let next = starts.get(i + 1).copied().unwrap_or(data.len());
                for pos in s..next {
                    assert_eq!(d.align(pos), s, "wrap={wrap} pos {pos}");
                }
            }
            assert_eq!(d.last_top(2), starts[starts.len() - 2]);
            assert_eq!(d.last_top(100), 0);
        }
    }

    #[test]
    fn utf16_and_bom() {
        let p = pages();
        let mut data = vec![0xFF, 0xFE];
        for u in "ab\r\nc€".encode_utf16() {
            data.extend(u.to_le_bytes());
        }
        let d = doc(&data, &p, Mode::Text, Encoding::Utf16Le, true, 80);
        assert_eq!(d.base(), 2);
        assert_eq!(lines(&d), ["ab", "c€"]);
        assert_eq!(d.align(9), 2);
        assert_eq!(d.align(11), 10);
        assert_eq!(d.back(data.len(), 1), 10);
        assert_eq!(d.back(10, 1), 2);
    }

    #[test]
    fn hex_and_binary_lines() {
        let p = pages();
        let data: Vec<u8> = (0u8..20).chain(*b"Hi").collect();
        let d = doc(&data, &p, Mode::Hex, Encoding::Ansi, true, 80);
        let first = text(&d.line(0));
        assert!(first.starts_with("00000000  00 01 02 03 04 05 06 07  08 09 0A"), "{first}");
        assert!(first.ends_with("................"), "{first}");
        let second = d.line(16);
        assert!(text(&second).starts_with("00000010  10 11 12 13 48 69"), "{}", text(&second));
        assert!(text(&second).ends_with("....Hi"));
        assert_eq!(d.align(21), 16);
        assert_eq!(d.last_top(1), 16);
        assert_eq!(d.back(16, 5), 0);
        let d = doc(&data, &p, Mode::Binary, Encoding::Ansi, true, 80);
        assert_eq!(d.line(0).end, 22);
        assert_eq!(text(&d.line(0)).chars().filter(|&c| c == '·').count(), 20);
    }

    #[test]
    fn invalid_utf8_shows_replacement() {
        let p = pages();
        let d = doc(b"a\xffb\xe2\x82", &p, Mode::Text, Encoding::Utf8, true, 80);
        assert_eq!(lines(&d), ["a\u{FFFD}b\u{FFFD}\u{FFFD}"]);
    }
}
