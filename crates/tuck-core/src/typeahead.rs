// SPDX-License-Identifier: GPL-3.0-or-later
//! Type-ahead selection: typing a few letters jumps to the next matching name.

/// Index of the first name, searching from `start` and wrapping, that starts
/// with `prefix` (case-insensitive). Falls back to a substring match.
pub fn find<S: AsRef<str>>(names: &[S], prefix: &str, start: usize) -> Option<usize> {
    if names.is_empty() || prefix.is_empty() {
        return None;
    }
    let needle = prefix.to_lowercase();
    let n = names.len();
    let start = start.min(n - 1);
    let order = (0..n).map(|i| (start + i) % n);
    let lowered: Vec<String> = names.iter().map(|s| s.as_ref().to_lowercase()).collect();
    order
        .clone()
        .find(|&i| lowered[i].starts_with(&needle))
        .or_else(|| order.into_iter().find(|&i| lowered[i].contains(&needle)))
}

/// Accumulates keystrokes and resets after a pause, like Dolphin.
#[derive(Debug, Default)]
pub struct Buffer {
    text: String,
    last_ms: u64,
}

impl Buffer {
    pub const TIMEOUT_MS: u64 = 1000;

    /// Feed one character typed at time `now_ms`; returns the current query.
    pub fn push(&mut self, c: char, now_ms: u64) -> &str {
        if now_ms.saturating_sub(self.last_ms) > Self::TIMEOUT_MS {
            self.text.clear();
        }
        self.last_ms = now_ms;
        self.text.push(c);
        &self.text
    }

    pub fn clear(&mut self) {
        self.text.clear();
    }

    /// Is the user in the middle of typing a name? Then Space is part of
    /// the name, not "Quick Look".
    pub fn active(&self, now_ms: u64) -> bool {
        !self.text.is_empty() && now_ms.saturating_sub(self.last_ms) <= Self::TIMEOUT_MS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_prefix_from_start_and_wraps() {
        let names = ["Apple", "banana", "Berry", "cherry"];
        assert_eq!(find(&names, "b", 0), Some(1));
        assert_eq!(find(&names, "b", 2), Some(2));
        assert_eq!(find(&names, "a", 2), Some(0));
        assert_eq!(find(&names, "ERR", 0), Some(2), "substring fallback");
        assert_eq!(find(&names, "zz", 0), None);
        assert_eq!(find::<&str>(&[], "a", 0), None);
    }

    #[test]
    fn buffer_resets_after_timeout() {
        let mut b = Buffer::default();
        b.push('a', 10_000);
        assert_eq!(b.push('b', 10_500), "ab");
        assert_eq!(b.push('c', 12_000), "c");
        assert!(b.active(12_500));
        assert!(!b.active(13_500));
        assert!(!Buffer::default().active(0));
    }
}
