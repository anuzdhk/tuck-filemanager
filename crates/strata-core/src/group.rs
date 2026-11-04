// SPDX-License-Identifier: GPL-3.0-or-later
//! Grouping ("Show in Groups"), keyed on the active sort role like Dolphin.

use chrono::{Datelike, Local, NaiveDate};

use crate::entry::EntryMeta;
use crate::format::local_date;
use crate::sort::SortKey;

/// A group: `rank` orders groups consistently with an ascending sort on the
/// same key, `label` is the heading. Groups with equal rank are ordered by label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub rank: i64,
    pub label: String,
}

/// Group for `e` when sorted by `key`. `today` is passed in for tests.
pub fn group(e: &EntryMeta, key: SortKey, today: NaiveDate) -> Group {
    let g = |rank: i64, label: &str| Group { rank, label: label.to_string() };
    match key {
        SortKey::Name => name_group(&e.name),
        SortKey::Size => {
            if e.is_dir() {
                g(-1, "Folders")
            } else {
                let (rank, label) = size_group(e.size);
                g(rank, label)
            }
        }
        SortKey::Modified => match local_date(e.modified) {
            Some(d) => date_group(d, today),
            None => g(i64::MIN, "Unknown"),
        },
        SortKey::Type => {
            if e.type_name.is_empty() { g(1, "Unknown") } else { g(0, &e.type_name) }
        }
        SortKey::Extension => {
            if e.is_dir() {
                g(-1, "Folders")
            } else {
                match e.extension() {
                    ext if ext.is_empty() => g(1, "No Extension"),
                    ext => g(0, &ext.to_uppercase()),
                }
            }
        }
    }
}

/// Order two groups (ascending). Callers reverse it for descending sorts.
pub fn compare_groups(a: &Group, b: &Group) -> std::cmp::Ordering {
    a.rank.cmp(&b.rank).then_with(|| crate::sort::natural_cmp(&a.label, &b.label))
}

pub fn group_label(e: &EntryMeta, key: SortKey, today: NaiveDate) -> String {
    group(e, key, today).label
}

pub fn group_now(e: &EntryMeta, key: SortKey) -> Group {
    group(e, key, Local::now().date_naive())
}

fn name_group(name: &str) -> Group {
    let first = name.trim_start_matches('.').chars().next();
    match first {
        Some(c) if c.is_ascii_digit() => Group { rank: 0, label: "0–9".into() },
        Some(c) if c.is_alphabetic() => {
            let lower = c.to_lowercase().next().unwrap_or(c);
            Group { rank: 1 + lower as i64, label: c.to_uppercase().collect() }
        }
        _ => Group { rank: i64::MAX, label: "Others".into() },
    }
}

fn size_group(size: u64) -> (i64, &'static str) {
    match size {
        0 => (0, "Empty"),
        1..=99_999 => (1, "Tiny (under 100 KB)"),
        100_000..=999_999 => (2, "Small (under 1 MB)"),
        1_000_000..=127_999_999 => (3, "Medium (under 128 MB)"),
        128_000_000..=999_999_999 => (4, "Large (under 1 GB)"),
        _ => (5, "Huge (1 GB and over)"),
    }
}

fn date_group(d: NaiveDate, today: NaiveDate) -> Group {
    // Ranks grow with recency so an ascending (oldest first) sort lists
    // older groups first. Month groups use year*100+month.
    const RECENT: i64 = 1_000_000_000;
    let g = |rank: i64, label: String| Group { rank, label };
    let days = (today - d).num_days();
    if days < 0 {
        return g(RECENT + 6, "In the Future".into());
    }
    if days == 0 {
        return g(RECENT + 5, "Today".into());
    }
    if days == 1 {
        return g(RECENT + 4, "Yesterday".into());
    }
    let week_start = today - chrono::Days::new(today.weekday().num_days_from_monday() as u64);
    if d >= week_start {
        return g(RECENT + 3, "Earlier This Week".into());
    }
    if d >= week_start - chrono::Days::new(7) {
        return g(RECENT + 2, "Last Week".into());
    }
    if d.year() == today.year() && d.month() == today.month() {
        return g(RECENT + 1, "Earlier This Month".into());
    }
    let rank = d.year() as i64 * 100 + d.month() as i64;
    if d.year() == today.year() {
        return g(rank, d.format("%B").to_string());
    }
    g(rank, d.format("%B %Y").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::EntryKind;
    use chrono::TimeZone;

    fn file(name: &str, size: u64, modified: i64) -> EntryMeta {
        EntryMeta::new(name.into(), EntryKind::File, size, modified, "Text".into())
    }

    #[test]
    fn by_name() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        let g = |n| group_label(&file(n, 0, 0), SortKey::Name, today);
        assert_eq!(g("apple"), "A");
        assert_eq!(g(".hidden"), "H");
        assert_eq!(g("2024 taxes"), "0–9");
        assert_eq!(g("_x"), "Others");
        assert_eq!(g("éclair"), "É");
    }

    #[test]
    fn by_size() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        assert_eq!(group_label(&file("a", 0, 0), SortKey::Size, today), "Empty");
        assert_eq!(group_label(&file("a", 500_000, 0), SortKey::Size, today), "Small (under 1 MB)");
        let dir = EntryMeta { kind: EntryKind::Directory, ..file("d", 0, 0) };
        assert_eq!(group_label(&dir, SortKey::Size, today), "Folders");
    }

    #[test]
    fn by_date() {
        // Wednesday 2026-11-04.
        let today = NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        let at = |y, m, d| Local.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap().timestamp();
        let g = |t| group_label(&file("a", 0, t), SortKey::Modified, today);
        assert_eq!(g(at(2026, 11, 4)), "Today");
        assert_eq!(g(at(2026, 11, 3)), "Yesterday");
        assert_eq!(g(at(2026, 11, 2)), "Earlier This Week");
        assert_eq!(g(at(2026, 10, 28)), "Last Week");
        assert_eq!(g(at(2026, 10, 1)), "October");
        assert_eq!(g(at(2025, 6, 1)), "June 2025");
        assert_eq!(g(at(2027, 1, 1)), "In the Future");
    }

    #[test]
    fn group_order_matches_sort_order() {
        use crate::sort::{SortOptions, compare};
        let today = NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        let at = |y, m, d| Local.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap().timestamp();
        let mut v: Vec<EntryMeta> = [
            ("b", at(2026, 11, 4)),
            ("_x", at(2025, 1, 1)),
            ("A", at(2026, 10, 28)),
            ("1", at(2026, 11, 3)),
            ("!y", at(2026, 6, 1)),
            ("a", at(2026, 11, 2)),
        ]
        .into_iter()
        .map(|(n, t)| file(n, 0, t))
        .collect();
        for key in [SortKey::Name, SortKey::Modified] {
            let opts = SortOptions { key, folders_first: false, ..Default::default() };
            // Sort by (group, then item) — what the UI does.
            v.sort_by(|a, b| {
                compare_groups(&group(a, key, today), &group(b, key, today))
                    .then_with(|| compare(a, b, &opts))
            });
            // Every group must be one contiguous run.
            let labels: Vec<String> = v.iter().map(|e| group(e, key, today).label).collect();
            let mut seen = std::collections::HashSet::new();
            let mut prev: Option<&String> = None;
            for l in &labels {
                if prev != Some(l) {
                    assert!(seen.insert(l.clone()), "{key:?}: group {l} split: {labels:?}");
                }
                prev = Some(l);
            }
        }
    }

    #[test]
    fn by_extension() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        assert_eq!(group_label(&file("a.pdf", 0, 0), SortKey::Extension, today), "PDF");
        assert_eq!(group_label(&file("Makefile", 0, 0), SortKey::Extension, today), "No Extension");
    }
}
