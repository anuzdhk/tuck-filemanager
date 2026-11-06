// SPDX-License-Identifier: GPL-3.0-or-later
//! User places, kept in GTK's bookmarks file (`~/.config/gtk-3.0/bookmarks`)
//! so they are shared with GTK file dialogs, Nautilus and others.
//! Each line is `URI [label]`.

use std::io;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub uri: String,
    /// Custom name; `None` shows the folder's own name.
    pub label: Option<String>,
}

pub fn file() -> PathBuf {
    crate::paths::config_home().join("gtk-3.0/bookmarks")
}

pub fn parse(text: &str) -> Vec<Bookmark> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| match l.split_once(' ') {
            Some((u, n)) if !n.trim().is_empty() => Bookmark { uri: u.into(), label: Some(n.trim().into()) },
            Some((u, _)) => Bookmark { uri: u.into(), label: None },
            None => Bookmark { uri: l.into(), label: None },
        })
        .collect()
}

pub fn format(list: &[Bookmark]) -> String {
    list.iter()
        .map(|b| match &b.label {
            Some(l) => format!("{} {}\n", b.uri, l.replace('\n', " ")),
            None => format!("{}\n", b.uri),
        })
        .collect()
}

pub fn load() -> Vec<Bookmark> {
    std::fs::read_to_string(file()).map(|t| parse(&t)).unwrap_or_default()
}

/// Save atomically (write a temporary file, then rename).
pub fn save(list: &[Bookmark]) -> io::Result<()> {
    let f = file();
    if let Some(d) = f.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = f.with_extension("tuck-tmp");
    std::fs::write(&tmp, format(list))?;
    std::fs::rename(tmp, f)
}

/// Add `uri` unless it's already there.
pub fn add(list: &mut Vec<Bookmark>, uri: &str) -> bool {
    if list.iter().any(|b| b.uri == uri) {
        return false;
    }
    list.push(Bookmark { uri: uri.into(), label: None });
    true
}

pub fn remove(list: &mut Vec<Bookmark>, uri: &str) {
    list.retain(|b| b.uri != uri);
}

pub fn rename(list: &mut [Bookmark], uri: &str, label: &str) {
    if let Some(b) = list.iter_mut().find(|b| b.uri == uri) {
        let l = label.trim();
        b.label = (!l.is_empty()).then(|| l.to_string());
    }
}

/// Move `uri` to position `to` (for drag-reordering).
pub fn move_to(list: &mut Vec<Bookmark>, uri: &str, to: usize) {
    if let Some(i) = list.iter().position(|b| b.uri == uri) {
        let b = list.remove(i);
        list.insert(to.min(list.len()), b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let text = "file:///home/u/Work\nfile:///home/u/My%20Stuff Stuff and more\n\nsftp://host/srv Server\n";
        let mut l = parse(text);
        assert_eq!(l.len(), 3);
        assert_eq!(l[1].label.as_deref(), Some("Stuff and more"));
        assert_eq!(format(&l), "file:///home/u/Work\nfile:///home/u/My%20Stuff Stuff and more\nsftp://host/srv Server\n");
        assert!(!add(&mut l, "file:///home/u/Work"));
        assert!(add(&mut l, "file:///tmp"));
        rename(&mut l, "file:///tmp", "  Scratch ");
        assert_eq!(l[3].label.as_deref(), Some("Scratch"));
        rename(&mut l, "file:///tmp", "");
        assert_eq!(l[3].label, None);
        move_to(&mut l, "file:///tmp", 0);
        assert_eq!(l[0].uri, "file:///tmp");
        remove(&mut l, "file:///tmp");
        assert_eq!(l.len(), 3);
    }
}
