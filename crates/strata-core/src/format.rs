// SPDX-License-Identifier: GPL-3.0-or-later
//! Human-readable sizes and dates.

use chrono::{DateTime, Local, NaiveDate, TimeZone};

/// Decimal units ("12.4 MB"), like macOS and GNOME.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["KB", "MB", "GB", "TB", "PB", "EB"];
    if bytes < 1000 {
        return if bytes == 1 { "1 byte".into() } else { format!("{bytes} bytes") };
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 999.95 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn item_count(n: u64) -> String {
    if n == 1 { "1 item".into() } else { format!("{n} items") }
}

/// "Today, 10:42", "Yesterday, 09:00", "Mar 3, 14:05", "Mar 3, 2023".
pub fn date_relative(secs: i64, now: DateTime<Local>) -> String {
    let Some(t) = Local.timestamp_opt(secs, 0).single() else {
        return "Unknown".into();
    };
    let today = now.date_naive();
    let day = t.date_naive();
    let time = t.format("%H:%M");
    if day == today {
        format!("Today, {time}")
    } else if Some(day) == today.pred_opt() {
        format!("Yesterday, {time}")
    } else if day.format("%Y").to_string() == today.format("%Y").to_string() {
        format!("{}, {time}", t.format("%b %-d"))
    } else {
        t.format("%b %-d, %Y").to_string()
    }
}

pub fn date_full(secs: i64) -> String {
    match Local.timestamp_opt(secs, 0).single() {
        Some(t) => t.format("%A, %B %-d, %Y at %H:%M").to_string(),
        None => "Unknown".into(),
    }
}

pub(crate) fn local_date(secs: i64) -> Option<NaiveDate> {
    Local.timestamp_opt(secs, 0).single().map(|t| t.date_naive())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(size(0), "0 bytes");
        assert_eq!(size(1), "1 byte");
        assert_eq!(size(999), "999 bytes");
        assert_eq!(size(1000), "1.0 KB");
        assert_eq!(size(12_400_000), "12.4 MB");
        assert_eq!(size(143_800_000_000), "144 GB");
        assert_eq!(size(999_999), "1.0 MB");
        assert_eq!(size(u64::MAX), "18.4 EB");
    }

    #[test]
    fn counts() {
        assert_eq!(item_count(1), "1 item");
        assert_eq!(item_count(0), "0 items");
    }

    #[test]
    fn relative_dates() {
        let now = Local.with_ymd_and_hms(2026, 11, 4, 15, 0, 0).unwrap();
        let ts = |y, m, d, h, mi| Local.with_ymd_and_hms(y, m, d, h, mi, 0).unwrap().timestamp();
        assert_eq!(date_relative(ts(2026, 11, 4, 10, 42), now), "Today, 10:42");
        assert_eq!(date_relative(ts(2026, 11, 3, 9, 5), now), "Yesterday, 09:05");
        assert_eq!(date_relative(ts(2026, 3, 3, 14, 5), now), "Mar 3, 14:05");
        assert_eq!(date_relative(ts(2023, 3, 3, 14, 5), now), "Mar 3, 2023");
    }
}
