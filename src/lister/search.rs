//! Searching the lister's file on a background thread.
//!
//! The search term is turned into byte patterns of the shown encoding once; the file is then
//! scanned as raw bytes – no decoding, so even huge files are searched at memory speed.
//! Ignoring case means: each character may appear as itself, in lower or in upper case.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use crate::lister::text::{CodePages, Encoding};
use crate::sys::mapped::MappedFile;
use crate::sys::win::SendHwnd;

/// Bytes scanned between two looks at the cancel flag.
const CHUNK: usize = 1 << 20;

/// The search term in bytes: per character, every byte sequence that counts as a match.
pub struct Pattern {
    chars: Vec<Vec<Vec<u8>>>,
    /// Possible first bytes, for a quick check.
    first: [bool; 256],
    /// Matches start at `base + k * step` (UTF-16 needs even offsets).
    step: usize,
    base: usize,
}

pub enum Outcome {
    /// Byte range of the match.
    Found(usize, usize),
    NotFound,
    Cancelled,
}

/// The character in `enc`; `None` if the encoding can't express it.
fn encode(c: char, enc: Encoding, pages: &CodePages) -> Option<Vec<u8>> {
    match enc {
        Encoding::Ansi | Encoding::Oem => {
            let u = u16::try_from(c as u32).ok()?;
            // Plain ASCII is the same in every code page; the tables may map 0x01–0x1F to glyphs.
            if u < 0x80 {
                return Some(vec![u as u8]);
            }
            pages.table(enc).iter().position(|&x| x == u).map(|b| vec![b as u8])
        }
        Encoding::Utf8 => Some(c.to_string().into_bytes()),
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let mut buf = [0u16; 2];
            let units = c.encode_utf16(&mut buf);
            let bytes =
                units.iter().flat_map(|u| if enc == Encoding::Utf16Le { u.to_le_bytes() } else { u.to_be_bytes() });
            Some(bytes.collect())
        }
    }
}

impl Pattern {
    /// `None` if the term is empty or can't occur in this encoding.
    pub fn new(term: &str, enc: Encoding, pages: &CodePages, ignore_case: bool, base: usize) -> Option<Pattern> {
        let mut chars = Vec::new();
        for c in term.chars() {
            let mut variants = vec![c];
            if ignore_case {
                for v in c.to_lowercase().chain(c.to_uppercase()) {
                    // Only one-to-one mappings ("ß" → "SS" would need a longer match).
                    if c.to_lowercase().count() == 1 && c.to_uppercase().count() == 1 && !variants.contains(&v) {
                        variants.push(v);
                    }
                }
            }
            let mut alts: Vec<Vec<u8>> = variants.into_iter().filter_map(|v| encode(v, enc, pages)).collect();
            alts.dedup();
            if alts.is_empty() {
                return None;
            }
            chars.push(alts);
        }
        let mut first = [false; 256];
        for alt in chars.first()? {
            first[alt[0] as usize] = true;
        }
        let step = if matches!(enc, Encoding::Utf16Le | Encoding::Utf16Be) { 2 } else { 1 };
        Some(Pattern { chars, first, step, base })
    }

    /// End of the match starting at `p`, if there is one.
    fn match_at(&self, data: &[u8], p: usize) -> Option<usize> {
        let mut q = p;
        for alts in &self.chars {
            let alt = alts.iter().find(|a| data[q..].starts_with(a))?;
            q += alt.len();
        }
        Some(q)
    }

    fn aligned(&self, p: usize) -> bool {
        p >= self.base && (p - self.base).is_multiple_of(self.step)
    }

    /// First match at or after `from` (`forward`), or at or before `from`, within `lo..hi`.
    /// `progress(scanned)` is called every `CHUNK` bytes; returning `false` cancels.
    pub fn find(
        &self,
        data: &[u8],
        from: usize,
        forward: bool,
        (lo, hi): (usize, usize),
        progress: &mut dyn FnMut(usize) -> bool,
    ) -> Outcome {
        let hi = hi.min(data.len());
        let lo = lo.max(self.base);
        if lo >= hi {
            return Outcome::NotFound;
        }
        let check = |p: usize| {
            if self.first[data[p] as usize] && self.aligned(p) {
                return self.match_at(data, p).map(|e| Outcome::Found(p, e));
            }
            None
        };
        if forward {
            let mut p = from.max(lo);
            while p < hi {
                let end = (p + CHUNK).min(hi);
                for q in p..end {
                    if let Some(found) = check(q) {
                        return found;
                    }
                }
                p = end;
                if !progress(p - from.max(lo)) {
                    return Outcome::Cancelled;
                }
            }
        } else {
            let start = from.min(hi - 1);
            let mut p = start + 1;
            while p > lo {
                let begin = p.saturating_sub(CHUNK).max(lo);
                for q in (begin..p).rev() {
                    if let Some(found) = check(q) {
                        return found;
                    }
                }
                p = begin;
                if !progress(start + 1 - p) {
                    return Outcome::Cancelled;
                }
            }
        }
        Outcome::NotFound
    }
}

/// A search running on its own thread.
pub struct Running {
    cancel: Arc<AtomicBool>,
    scanned: Arc<AtomicU64>,
    /// Bytes to scan at most (for the percentage).
    total: u64,
    pub result: Receiver<Outcome>,
}

impl Running {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Progress in percent.
    pub fn percent(&self) -> u64 {
        (self.scanned.load(Ordering::Relaxed) * 100).checked_div(self.total).unwrap_or(0).min(100)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Start searching; `msg` is posted to `notify` now and then while it runs and once at the end
/// (after the outcome was sent).
pub fn start(
    file: Arc<MappedFile>,
    pattern: Pattern,
    from: usize,
    forward: bool,
    range: (usize, usize),
    notify: (SendHwnd, u32),
) -> Running {
    let cancel = Arc::new(AtomicBool::new(false));
    let scanned = Arc::new(AtomicU64::new(0));
    let total = if forward { range.1.saturating_sub(from) } else { from.saturating_sub(range.0) + 1 } as u64;
    let (tx, rx) = channel();
    let (c, s) = (cancel.clone(), scanned.clone());
    std::thread::spawn(move || {
        let mut last_post = Instant::now();
        let mut progress = |n: usize| {
            s.store(n as u64, Ordering::Relaxed);
            if last_post.elapsed() >= Duration::from_millis(100) {
                last_post = Instant::now();
                notify.0.post(notify.1);
            }
            !c.load(Ordering::Relaxed)
        };
        let outcome = pattern.find(file.bytes(), from, forward, range, &mut progress);
        if tx.send(outcome).is_ok() {
            notify.0.post(notify.1);
        }
    });
    Running { cancel, scanned, total, result: rx }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lister::text::tests::pages;

    fn find(data: &[u8], term: &str, enc: Encoding, ignore_case: bool, from: usize, forward: bool) -> Option<usize> {
        let p = pages();
        let pat = Pattern::new(term, enc, &p, ignore_case, 0)?;
        match pat.find(data, from, forward, (0, data.len()), &mut |_| true) {
            Outcome::Found(s, _) => Some(s),
            _ => None,
        }
    }

    #[test]
    fn finds_forward_and_backward() {
        let data = b"abc Hallo abc hallo";
        assert_eq!(find(data, "abc", Encoding::Utf8, false, 0, true), Some(0));
        assert_eq!(find(data, "abc", Encoding::Utf8, false, 1, true), Some(10));
        assert_eq!(find(data, "abc", Encoding::Utf8, false, 9, false), Some(0));
        assert_eq!(find(data, "abc", Encoding::Utf8, false, 18, false), Some(10));
        assert_eq!(find(data, "xyz", Encoding::Utf8, false, 0, true), None);
        assert_eq!(find(data, "hallo", Encoding::Utf8, false, 0, true), Some(14));
        assert_eq!(find(data, "hallo", Encoding::Utf8, true, 0, true), Some(4));
    }

    #[test]
    fn ignores_case_beyond_ascii() {
        assert_eq!(find("xx GRÜSSE grüße".as_bytes(), "Grüße", Encoding::Utf8, true, 0, true), Some(11));
        assert_eq!(find("ÄRGER".as_bytes(), "ärger", Encoding::Utf8, true, 0, true), Some(0));
        assert_eq!(find(b"x \xc4rger", "ärger", Encoding::Ansi, true, 0, true), Some(2));
        assert_eq!(find(b"x \xc4rger", "ärger", Encoding::Ansi, false, 0, true), None);
    }

    #[test]
    fn utf16_matches_only_whole_units() {
        // "aa" in UTF-16 LE is 61 00 61 00; at the odd offset 1 the bytes "00 61" must not count.
        let mut data = vec![0x00];
        for u in "xaa".encode_utf16() {
            data.extend(u.to_le_bytes());
        }
        let p = pages();
        let pat = Pattern::new("a", Encoding::Utf16Le, &p, false, 1).unwrap();
        match pat.find(&data, 0, true, (0, data.len()), &mut |_| true) {
            Outcome::Found(s, e) => assert_eq!((s, e), (3, 5)),
            _ => panic!("not found"),
        }
    }

    #[test]
    fn can_be_cancelled() {
        let data = vec![b'x'; 3 * CHUNK];
        let p = pages();
        let pat = Pattern::new("y", Encoding::Utf8, &p, false, 0).unwrap();
        let outcome = pat.find(&data, 0, true, (0, data.len()), &mut |_| false);
        assert!(matches!(outcome, Outcome::Cancelled));
    }
}
