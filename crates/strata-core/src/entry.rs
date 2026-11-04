// SPDX-License-Identifier: GPL-3.0-or-later
//! The minimal description of a directory entry that sorting, grouping and
//! filtering need. The UI builds it from whatever its listing backend returns.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EntryKind {
    Directory,
    #[default]
    File,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EntryMeta {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    /// Modification time, seconds since the Unix epoch.
    pub modified: i64,
    /// Human-readable type, e.g. "PDF document".
    pub type_name: String,
    /// Precomputed natural-sort key for `name` (see [`crate::sort::name_key`]).
    /// Empty means "not computed"; comparisons then fall back to the name.
    pub name_key: Box<[u8]>,
}

impl EntryMeta {
    pub fn new(name: String, kind: EntryKind, size: u64, modified: i64, type_name: String) -> Self {
        let name_key = crate::sort::name_key(&name).into_boxed_slice();
        Self { name, kind, size, modified, type_name, name_key }
    }

    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Directory
    }

    pub fn is_hidden(&self) -> bool {
        is_hidden_name(&self.name)
    }

    /// Lowercased extension, or empty. Leading-dot names have no extension.
    pub fn extension(&self) -> String {
        extension_of(&self.name)
    }
}

pub fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.') || name.ends_with('~')
}

pub fn extension_of(name: &str) -> String {
    let trimmed = name.trim_start_matches('.');
    match trimmed.rfind('.') {
        Some(i) if i + 1 < trimmed.len() => trimmed[i + 1..].to_lowercase(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_names() {
        assert!(is_hidden_name(".bashrc"));
        assert!(is_hidden_name("notes.txt~"));
        assert!(!is_hidden_name("notes.txt"));
    }

    #[test]
    fn extensions() {
        assert_eq!(extension_of("a.TXT"), "txt");
        assert_eq!(extension_of("archive.tar.gz"), "gz");
        assert_eq!(extension_of(".bashrc"), "");
        assert_eq!(extension_of(".config.json"), "json");
        assert_eq!(extension_of("noext"), "");
        assert_eq!(extension_of("trailing."), "");
    }
}
