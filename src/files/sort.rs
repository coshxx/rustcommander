//! Sort order of a file list: by name, size or date, ascending or descending. `..` stays first
//! and folders stay above files; whether folders follow the chosen column is an option.

use std::cmp::Ordering;

use crate::files::listing::{Entry, Kind};
use crate::files::names::natural_cmp;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SortKey {
    #[default]
    Name,
    Size,
    Date,
}

impl SortKey {
    /// Name in config.json.
    pub fn id(self) -> &'static str {
        match self {
            SortKey::Name => "name",
            SortKey::Size => "size",
            SortKey::Date => "date",
        }
    }

    pub fn from_id(s: &str) -> Option<SortKey> {
        [SortKey::Name, SortKey::Size, SortKey::Date].into_iter().find(|k| k.id() == s)
    }

    /// Direction when switching to this column: A→Z, but largest and newest first.
    fn starts_descending(self) -> bool {
        self != SortKey::Name
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Sort {
    pub key: SortKey,
    pub desc: bool,
}

impl Sort {
    /// Choosing `key` again flips the direction; another column starts in its own direction.
    pub fn choose(self, key: SortKey) -> Sort {
        if key == self.key { Sort { key, desc: !self.desc } } else { Sort { key, desc: key.starts_descending() } }
    }
}

/// Sort `entries`. `with_dirs` = folders are sorted by the chosen column as well (otherwise by
/// name; descending only when sorting by name). `dir_size` = counted size of a folder; folders
/// without one go to the end of the folder block when sorting by size, by name.
pub fn sort(entries: &mut [Entry], sort: Sort, with_dirs: bool, dir_size: impl Fn(&Entry) -> Option<u64>) {
    let by_name = |a: &Entry, b: &Entry| natural_cmp(&a.name, &b.name);
    let flip = |o: Ordering| if sort.desc { o.reverse() } else { o };
    entries.sort_by(|a, b| {
        let rank = |e: &Entry| match e.kind {
            Kind::Parent => 0,
            Kind::Dir => 1,
            _ => 2,
        };
        let (ra, rb) = (rank(a), rank(b));
        if ra != rb || ra == 0 {
            return ra.cmp(&rb);
        }
        let dirs = ra == 1;
        if dirs && !with_dirs && sort.key != SortKey::Name {
            return by_name(a, b);
        }
        match sort.key {
            SortKey::Name => flip(by_name(a, b)),
            SortKey::Date => flip(a.mtime.cmp(&b.mtime)).then_with(|| by_name(a, b)),
            SortKey::Size if dirs => match (dir_size(a), dir_size(b)) {
                (Some(x), Some(y)) => flip(x.cmp(&y)).then_with(|| by_name(a, b)),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => by_name(a, b),
            },
            SortKey::Size => flip(a.size.cmp(&b.size)).then_with(|| by_name(a, b)),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: Kind, size: u64, mtime: u64) -> Entry {
        Entry { name: name.encode_utf16().collect(), kind, hidden: false, size, mtime, marked: false }
    }

    fn names(entries: &[Entry]) -> Vec<String> {
        entries.iter().map(|e| String::from_utf16_lossy(&e.name)).collect()
    }

    /// `..`, folders b (counted 50), a (not counted), c (counted 900), files.
    fn sample() -> Vec<Entry> {
        vec![
            entry("y.txt", Kind::File, 10, 3),
            entry("b", Kind::Dir, 0, 9),
            entry("x2.txt", Kind::File, 300, 1),
            entry("a", Kind::Dir, 0, 1),
            entry("..", Kind::Parent, 0, 0),
            entry("x10.txt", Kind::File, 10, 2),
            entry("c", Kind::Dir, 0, 5),
        ]
    }

    fn sorted(sort: Sort, with_dirs: bool) -> Vec<String> {
        let mut v = sample();
        super::sort(&mut v, sort, with_dirs, |e| match &*String::from_utf16_lossy(&e.name) {
            "b" => Some(50),
            "c" => Some(900),
            _ => None,
        });
        names(&v)
    }

    #[test]
    fn name() {
        assert_eq!(sorted(Sort::default(), false), ["..", "a", "b", "c", "x2.txt", "x10.txt", "y.txt"]);
        let desc = Sort { key: SortKey::Name, desc: true };
        // Folders follow the direction when sorting by name, with or without the option.
        assert_eq!(sorted(desc, false), ["..", "c", "b", "a", "y.txt", "x10.txt", "x2.txt"]);
        assert_eq!(sorted(desc, true), sorted(desc, false));
    }

    #[test]
    fn size() {
        let desc = Sort { key: SortKey::Size, desc: true };
        // Ties by name, ascending.
        assert_eq!(sorted(desc, false), ["..", "a", "b", "c", "x2.txt", "x10.txt", "y.txt"]);
        // Uncounted folders last, in both directions.
        assert_eq!(sorted(desc, true), ["..", "c", "b", "a", "x2.txt", "x10.txt", "y.txt"]);
        let asc = Sort { key: SortKey::Size, desc: false };
        assert_eq!(sorted(asc, true), ["..", "b", "c", "a", "x10.txt", "y.txt", "x2.txt"]);
    }

    #[test]
    fn date() {
        let desc = Sort { key: SortKey::Date, desc: true };
        assert_eq!(sorted(desc, false), ["..", "a", "b", "c", "y.txt", "x10.txt", "x2.txt"]);
        assert_eq!(sorted(desc, true), ["..", "b", "c", "a", "y.txt", "x10.txt", "x2.txt"]);
    }

    #[test]
    fn choose() {
        let s = Sort::default().choose(SortKey::Size);
        assert_eq!(s, Sort { key: SortKey::Size, desc: true });
        assert_eq!(s.choose(SortKey::Size), Sort { key: SortKey::Size, desc: false });
        assert_eq!(s.choose(SortKey::Name), Sort { key: SortKey::Name, desc: false });
        assert_eq!(Sort::default().choose(SortKey::Name), Sort { key: SortKey::Name, desc: true });
    }
}
