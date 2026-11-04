// SPDX-License-Identifier: GPL-3.0-or-later
//! Sorting, matching Dolphin's defaults: natural ("file2" before "file10"),
//! case-insensitive, folders first.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::entry::EntryMeta;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SortKey {
    #[default]
    Name,
    Size,
    Modified,
    Type,
    Extension,
}

impl SortKey {
    pub const ALL: [SortKey; 5] = [
        SortKey::Name,
        SortKey::Size,
        SortKey::Modified,
        SortKey::Type,
        SortKey::Extension,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SortKey::Name => "Name",
            SortKey::Size => "Size",
            SortKey::Modified => "Date Modified",
            SortKey::Type => "Kind",
            SortKey::Extension => "Extension",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            SortKey::Name => "name",
            SortKey::Size => "size",
            SortKey::Modified => "modified",
            SortKey::Type => "type",
            SortKey::Extension => "extension",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.id() == id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortOptions {
    pub key: SortKey,
    pub descending: bool,
    pub folders_first: bool,
}

impl Default for SortOptions {
    fn default() -> Self {
        Self { key: SortKey::Name, descending: false, folders_first: true }
    }
}

/// Full comparison used by every view.
pub fn compare(a: &EntryMeta, b: &EntryMeta, opts: &SortOptions) -> Ordering {
    if opts.folders_first && a.is_dir() != b.is_dir() {
        // Folders stay on top regardless of direction, like Dolphin.
        return if a.is_dir() { Ordering::Less } else { Ordering::Greater };
    }
    let primary = match opts.key {
        SortKey::Name => Ordering::Equal,
        SortKey::Size => {
            // Folder "size" is item count elsewhere; compare files by bytes only.
            a.size.cmp(&b.size)
        }
        SortKey::Modified => a.modified.cmp(&b.modified),
        SortKey::Type => natural_cmp(&a.type_name, &b.type_name),
        SortKey::Extension => a.extension().cmp(&b.extension()),
    };
    let ord = primary.then_with(|| name_cmp(a, b));
    if opts.descending { ord.reverse() } else { ord }
}

fn name_cmp(a: &EntryMeta, b: &EntryMeta) -> Ordering {
    if !a.name_key.is_empty() && !b.name_key.is_empty() {
        a.name_key.cmp(&b.name_key).then_with(|| a.name.cmp(&b.name))
    } else {
        natural_cmp(&a.name, &b.name)
    }
}

/// A byte string whose plain ordering equals [`natural_cmp`] up to ties, so
/// sorting 100k names is a memcmp per comparison instead of a char walk.
///
/// Encoding: each char is lowercased and UTF-8 encoded (UTF-8 byte order is
/// code point order). A digit run becomes `'0'`, its significant length as 4
/// big-endian bytes, the significant digits, then the leading-zero count as
/// 4 bytes. Using `'0'` as the marker keeps digit runs ordered against other
/// characters exactly as a single digit would be.
pub fn name_key(name: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len() + 8);
    let bytes = name.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let run = &bytes[start..i];
            let zeros = run.iter().take_while(|&&b| b == b'0').count();
            let sig = &run[zeros..];
            out.push(b'0');
            out.extend_from_slice(&(sig.len() as u32).to_be_bytes());
            out.extend_from_slice(sig);
            out.extend_from_slice(&(zeros as u32).to_be_bytes());
            continue;
        }
        let c = name[i..].chars().next().expect("char boundary");
        i += c.len_utf8();
        let mut buf = [0u8; 4];
        // natural_cmp compares only the first char of a lowercase expansion.
        let lower = c.to_lowercase().next().unwrap_or(c);
        out.extend_from_slice(lower.encode_utf8(&mut buf).as_bytes());
    }
    out
}

/// Case-insensitive natural comparison: digit runs compare by value.
/// Ties are broken case-sensitively so the order is total and stable.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let ord = natural_cmp_ci(a, b);
    if ord != Ordering::Equal {
        return ord;
    }
    a.cmp(b)
}

fn natural_cmp_ci(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) if ca.is_ascii_digit() && cb.is_ascii_digit() => {
                let na = take_digits(&mut ai);
                let nb = take_digits(&mut bi);
                let ta = na.trim_start_matches('0');
                let tb = nb.trim_start_matches('0');
                let ord = ta
                    .len()
                    .cmp(&tb.len())
                    .then_with(|| ta.cmp(tb))
                    // "01" after "1" so equal values still have an order.
                    .then_with(|| na.len().cmp(&nb.len()));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(ca), Some(cb)) => {
                let la = ca.to_lowercase().next().unwrap_or(ca);
                let lb = cb.to_lowercase().next().unwrap_or(cb);
                let ord = la.cmp(&lb);
                if ord != Ordering::Equal {
                    return ord;
                }
                ai.next();
                bi.next();
            }
        }
    }
}

fn take_digits(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut s = String::new();
    while let Some(&c) = it.peek() {
        if !c.is_ascii_digit() {
            break;
        }
        s.push(c);
        it.next();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::EntryKind;

    fn e(name: &str, kind: EntryKind, size: u64, modified: i64) -> EntryMeta {
        EntryMeta::new(name.into(), kind, size, modified, String::new())
    }

    fn sorted(mut v: Vec<EntryMeta>, opts: SortOptions) -> Vec<String> {
        v.sort_by(|a, b| compare(a, b, &opts));
        v.into_iter().map(|e| e.name).collect()
    }

    #[test]
    fn natural_order() {
        let mut names = vec!["file10", "File2", "file1", "file02", "apple", "Banana"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, ["apple", "Banana", "file1", "File2", "file02", "file10"]);
    }

    #[test]
    fn natural_is_total() {
        assert_eq!(natural_cmp("a", "A"), "a".cmp("A"));
        assert_ne!(natural_cmp("a", "A"), Ordering::Equal);
        assert_eq!(natural_cmp("x", "x"), Ordering::Equal);
    }

    #[test]
    fn huge_numbers_dont_overflow() {
        let a = "v99999999999999999999999999";
        let b = "v100000000000000000000000000";
        assert_eq!(natural_cmp(a, b), Ordering::Less);
    }

    #[test]
    fn folders_first_in_both_directions() {
        let v = vec![
            e("b.txt", EntryKind::File, 1, 0),
            e("zdir", EntryKind::Directory, 0, 0),
            e("a.txt", EntryKind::File, 1, 0),
            e("adir", EntryKind::Directory, 0, 0),
        ];
        let asc = SortOptions::default();
        assert_eq!(sorted(v.clone(), asc), ["adir", "zdir", "a.txt", "b.txt"]);
        let desc = SortOptions { descending: true, ..asc };
        assert_eq!(sorted(v, desc), ["zdir", "adir", "b.txt", "a.txt"]);
    }

    #[test]
    fn by_size_then_name() {
        let v = vec![
            e("big", EntryKind::File, 100, 0),
            e("b-small", EntryKind::File, 1, 0),
            e("a-small", EntryKind::File, 1, 0),
        ];
        let opts = SortOptions { key: SortKey::Size, ..Default::default() };
        assert_eq!(sorted(v, opts), ["a-small", "b-small", "big"]);
    }

    #[test]
    fn by_modified_descending() {
        let v = vec![
            e("old", EntryKind::File, 0, 10),
            e("new", EntryKind::File, 0, 30),
            e("mid", EntryKind::File, 0, 20),
        ];
        let opts = SortOptions { key: SortKey::Modified, descending: true, folders_first: false };
        assert_eq!(sorted(v, opts), ["new", "mid", "old"]);
    }

    #[test]
    fn name_key_matches_natural_cmp() {
        // Deterministic pseudo-random names over a nasty alphabet.
        let alphabet: Vec<char> = "aAbZ09 1-._éÉß2007ü".chars().collect();
        let mut seed: u64 = 0x9e3779b97f4a7c15;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let names: Vec<String> = (0..600)
            .map(|_| {
                let len = (next() % 8) as usize;
                (0..len).map(|_| alphabet[(next() % alphabet.len() as u64) as usize]).collect()
            })
            .collect();
        for a in &names {
            for b in names.iter().take(120) {
                let by_key = name_key(a).cmp(&name_key(b)).then_with(|| a.cmp(b));
                assert_eq!(by_key, natural_cmp(a, b), "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn sort_key_ids_roundtrip() {
        for k in SortKey::ALL {
            assert_eq!(SortKey::from_id(k.id()), Some(k));
        }
    }
}
