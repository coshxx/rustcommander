//! File names as UTF-16: conversion, case folding, natural sort order, prefix match.

use std::cmp::Ordering;
use std::os::windows::ffi::OsStrExt;

pub fn to_wide_z(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

#[inline]
fn is_digit(c: u16) -> bool {
    (b'0' as u16..=b'9' as u16).contains(&c)
}

/// Lowercase folding; fast path for ASCII, the rest via `char::to_lowercase`.
#[inline]
pub fn fold(c: u16) -> u32 {
    if c < 128 {
        (c as u8).to_ascii_lowercase() as u32
    } else {
        match char::from_u32(c as u32) {
            Some(ch) => ch.to_lowercase().next().map_or(c as u32, |l| l as u32),
            None => c as u32,
        }
    }
}

/// Natural, case-insensitive ordering: "file2" < "file10".
pub fn natural_cmp(a: &[u16], b: &[u16]) -> Ordering {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if is_digit(a[i]) && is_digit(b[j]) {
            while i < a.len() && a[i] == b'0' as u16 {
                i += 1;
            }
            while j < b.len() && b[j] == b'0' as u16 {
                j += 1;
            }
            let (si, sj) = (i, j);
            while i < a.len() && is_digit(a[i]) {
                i += 1;
            }
            while j < b.len() && is_digit(b[j]) {
                j += 1;
            }
            let ord = (i - si).cmp(&(j - sj)).then_with(|| a[si..i].cmp(&b[sj..j]));
            if ord != Ordering::Equal {
                return ord;
            }
            continue;
        }
        let (ca, cb) = (fold(a[i]), fold(b[j]));
        if ca != cb {
            return ca.cmp(&cb);
        }
        i += 1;
        j += 1;
    }
    (a.len() - i).cmp(&(b.len() - j)).then_with(|| a.cmp(b))
}

pub fn starts_with_ci(name: &[u16], prefix: &[u16]) -> bool {
    name.len() >= prefix.len() && name.iter().zip(prefix).all(|(&a, &b)| fold(a) == fold(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn natural_order() {
        let mut v = vec!["file10", "File2", "file1", "a", "B", "file02b", "file2a"];
        v.sort_by(|a, b| natural_cmp(&w(a), &w(b)));
        assert_eq!(v, ["a", "B", "file1", "File2", "file2a", "file02b", "file10"]);
    }

    #[test]
    fn prefix_ci() {
        assert!(starts_with_ci(&w("Programme"), &w("prog")));
        assert!(!starts_with_ci(&w("Pr"), &w("prog")));
    }
}
