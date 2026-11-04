// SPDX-License-Identifier: GPL-3.0-or-later
//! File-name rules: validation, unique names ("Report (2).pdf"), duplicate
//! names ("Report copy.pdf") and batch-rename patterns.

use std::path::{Path, PathBuf};

/// Compound extensions kept together when numbering or renaming.
const COMPOUND: &[&str] = &[".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tar.lz", ".tar.lzma", ".tar.z"];

/// Split "photo.final.jpg" into ("photo.final", ".jpg"), keeping ".tar.gz"
/// together. Leading-dot names (".bashrc") have no extension; folders never do.
pub fn split_ext(name: &str) -> (&str, &str) {
    let lower = name.to_ascii_lowercase();
    for c in COMPOUND {
        if lower.len() > c.len() && lower.ends_with(c) {
            let i = name.len() - c.len();
            return (&name[..i], &name[i..]);
        }
    }
    match name.rfind('.') {
        Some(0) | None => (name, ""),
        Some(i) if i + 1 == name.len() => (name, ""),
        Some(i) => (&name[..i], &name[i..]),
    }
}

/// Why a name can't be used, in words a user understands.
pub fn validate(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("The name can't be empty.".into());
    }
    if name == "." || name == ".." {
        return Err("“.” and “..” are reserved names.".into());
    }
    if name.contains('/') {
        return Err("Names can't contain “/”.".into());
    }
    if name.contains('\0') {
        return Err("Names can't contain a null character.".into());
    }
    if name.len() > 255 {
        return Err("The name is too long (255 bytes at most).".into());
    }
    Ok(())
}

/// Warnings that don't block a rename but deserve a heads-up.
pub fn warnings(old: &str, new: &str, is_dir: bool) -> Vec<String> {
    let mut w = Vec::new();
    if new.starts_with('.') && !old.starts_with('.') {
        w.push("Names starting with “.” are hidden.".into());
    }
    if !is_dir {
        let (_, a) = split_ext(old);
        let (_, b) = split_ext(new);
        if !a.eq_ignore_ascii_case(b) {
            w.push("Changing the extension may change which app opens the file.".into());
        }
    }
    if new != new.trim() {
        w.push("The name starts or ends with spaces.".into());
    }
    w
}

/// A free name in `dir` based on `name`: "name", then "name (2)", "name (3)"…
/// `taken` lets callers reserve names not yet on disk.
pub fn unique_name(dir: &Path, name: &str, taken: &dyn Fn(&str) -> bool) -> String {
    let exists = |n: &str| taken(n) || dir.join(n).symlink_metadata().is_ok();
    if !exists(name) {
        return name.to_string();
    }
    let (stem, ext) = split_ext(name);
    let stem = strip_counter(stem);
    for i in 2.. {
        let candidate = format!("{stem} ({i}){ext}");
        if !exists(&candidate) {
            return candidate;
        }
    }
    unreachable!()
}

pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(unique_name(dir, name, &|_| false))
}

/// "Report.pdf" → "Report copy.pdf", then "Report copy 2.pdf"…
pub fn duplicate_name(dir: &Path, name: &str, is_dir: bool) -> String {
    let (stem, ext) = if is_dir { (name, "") } else { split_ext(name) };
    let base = stem.trim_end_matches(char::is_numeric).trim_end();
    let base = base.strip_suffix(" copy").unwrap_or(stem);
    let base = if base.is_empty() { stem } else { base };
    let first = format!("{base} copy{ext}");
    if dir.join(&first).symlink_metadata().is_err() {
        return first;
    }
    for i in 2.. {
        let c = format!("{base} copy {i}{ext}");
        if dir.join(&c).symlink_metadata().is_err() {
            return c;
        }
    }
    unreachable!()
}

/// Remove a trailing " (N)" so "a (2)" numbers as "a (3)", not "a (2) (2)".
fn strip_counter(stem: &str) -> &str {
    if let Some(open) = stem.rfind(" (") {
        let inner = &stem[open + 2..];
        if let Some(num) = inner.strip_suffix(')')
            && !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
                return &stem[..open];
            }
    }
    stem
}

/// Batch rename, like Dolphin's: either a pattern where a run of `#` becomes
/// a zero-padded counter ("Holiday ###" → "Holiday 001"), or find/replace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchRule {
    Pattern { pattern: String, start: u64, keep_ext: bool },
    Replace { find: String, replace: String, case_sensitive: bool },
}

/// New names for `names`, in order. Errors if a result is invalid or two
/// results collide.
pub fn batch_rename(names: &[String], rule: &BatchRule) -> Result<Vec<String>, String> {
    let mut out = Vec::with_capacity(names.len());
    for (i, name) in names.iter().enumerate() {
        let new = match rule {
            BatchRule::Pattern { pattern, start, keep_ext } => {
                let n = start + i as u64;
                let mut s = fill_hashes(pattern, n);
                if !s.contains(&n.to_string()) && names.len() > 1 {
                    // No # in the pattern: append a counter so names stay unique.
                    s = format!("{s} {n}");
                }
                if *keep_ext {
                    s.push_str(split_ext(name).1);
                }
                s
            }
            BatchRule::Replace { find, replace, case_sensitive } => {
                if find.is_empty() {
                    name.clone()
                } else if *case_sensitive {
                    name.replace(find.as_str(), replace)
                } else {
                    replace_ci(name, find, replace)
                }
            }
        };
        validate(&new).map_err(|e| format!("“{new}”: {e}"))?;
        out.push(new);
    }
    let mut seen = std::collections::HashSet::new();
    for n in &out {
        if !seen.insert(n) {
            return Err(format!("Two items would both be named “{n}”."));
        }
    }
    Ok(out)
}

fn fill_hashes(pattern: &str, n: u64) -> String {
    let mut out = String::new();
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '#' {
            let mut width = 1;
            while chars.peek() == Some(&'#') {
                chars.next();
                width += 1;
            }
            out.push_str(&format!("{n:0width$}"));
        } else {
            out.push(c);
        }
    }
    out
}

fn replace_ci(s: &str, find: &str, rep: &str) -> String {
    let lower = s.to_lowercase();
    let f = find.to_lowercase();
    if lower.len() != s.len() {
        // Lowercasing changed byte lengths (rare scripts): fall back to exact.
        return s.replace(find, rep);
    }
    let mut out = String::new();
    let mut i = 0;
    while let Some(pos) = lower[i..].find(&f) {
        out.push_str(&s[i..i + pos]);
        out.push_str(rep);
        i += pos + f.len();
    }
    out.push_str(&s[i..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn splits_extensions() {
        assert_eq!(split_ext("a.txt"), ("a", ".txt"));
        assert_eq!(split_ext("a.b.jpg"), ("a.b", ".jpg"));
        assert_eq!(split_ext("backup.tar.gz"), ("backup", ".tar.gz"));
        assert_eq!(split_ext("BACKUP.TAR.XZ"), ("BACKUP", ".TAR.XZ"));
        assert_eq!(split_ext(".bashrc"), (".bashrc", ""));
        assert_eq!(split_ext("Makefile"), ("Makefile", ""));
        assert_eq!(split_ext("trailing."), ("trailing.", ""));
        assert_eq!(split_ext(".tar.gz"), (".tar", ".gz"));
    }

    #[test]
    fn validates() {
        assert!(validate("ok.txt").is_ok());
        assert!(validate("").is_err());
        assert!(validate("  ").is_err());
        assert!(validate("..").is_err());
        assert!(validate("a/b").is_err());
        assert!(validate(&"x".repeat(256)).is_err());
        assert!(validate(&"x".repeat(255)).is_ok());
    }

    #[test]
    fn warns() {
        assert_eq!(warnings("a.txt", "a.md", false).len(), 1);
        assert!(warnings("a.txt", "b.txt", false).is_empty());
        assert!(warnings("dir", "dir.old", true).is_empty());
        assert_eq!(warnings("a", ".a", true).len(), 1);
    }

    #[test]
    fn unique_names() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        assert_eq!(unique_name(p, "a.txt", &|_| false), "a.txt");
        fs::write(p.join("a.txt"), "").unwrap();
        assert_eq!(unique_name(p, "a.txt", &|_| false), "a (2).txt");
        fs::write(p.join("a (2).txt"), "").unwrap();
        assert_eq!(unique_name(p, "a.txt", &|_| false), "a (3).txt");
        assert_eq!(unique_name(p, "a (2).txt", &|_| false), "a (3).txt");
        fs::write(p.join("x.tar.gz"), "").unwrap();
        assert_eq!(unique_name(p, "x.tar.gz", &|_| false), "x (2).tar.gz");
        assert_eq!(unique_name(p, "new", &|n| n == "new"), "new (2)");
        // Broken symlinks count as taken.
        std::os::unix::fs::symlink("/nope", p.join("link")).unwrap();
        assert_eq!(unique_name(p, "link", &|_| false), "link (2)");
    }

    #[test]
    fn duplicate_names() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        assert_eq!(duplicate_name(p, "Report.pdf", false), "Report copy.pdf");
        fs::write(p.join("Report copy.pdf"), "").unwrap();
        assert_eq!(duplicate_name(p, "Report.pdf", false), "Report copy 2.pdf");
        assert_eq!(duplicate_name(p, "Report copy.pdf", false), "Report copy 2.pdf");
        assert_eq!(duplicate_name(p, "Photos 2024", true), "Photos 2024 copy");
    }

    #[test]
    fn batch_patterns() {
        let names = vec!["a.jpg".to_string(), "b.png".into(), "c".into()];
        let r = BatchRule::Pattern { pattern: "Holiday ###".into(), start: 1, keep_ext: true };
        assert_eq!(batch_rename(&names, &r).unwrap(), ["Holiday 001.jpg", "Holiday 002.png", "Holiday 003"]);
        let r = BatchRule::Pattern { pattern: "Trip".into(), start: 1, keep_ext: false };
        assert_eq!(batch_rename(&names, &r).unwrap(), ["Trip 1", "Trip 2", "Trip 3"]);
        let r = BatchRule::Pattern { pattern: "x/#".into(), start: 1, keep_ext: false };
        assert!(batch_rename(&names, &r).is_err());
    }

    #[test]
    fn batch_replace() {
        let names = vec!["IMG_001.JPG".to_string(), "img_002.jpg".into()];
        let r = BatchRule::Replace { find: "img_".into(), replace: "Photo ".into(), case_sensitive: false };
        assert_eq!(batch_rename(&names, &r).unwrap(), ["Photo 001.JPG", "Photo 002.jpg"]);
        let r = BatchRule::Replace { find: "img_".into(), replace: "".into(), case_sensitive: true };
        assert_eq!(batch_rename(&names, &r).unwrap(), ["IMG_001.JPG", "002.jpg"]);
        let r = BatchRule::Replace { find: "0".into(), replace: "".into(), case_sensitive: true };
        let dup = vec!["a0".to_string(), "a".into()];
        assert!(batch_rename(&dup, &r).unwrap_err().contains("both"));
    }
}
