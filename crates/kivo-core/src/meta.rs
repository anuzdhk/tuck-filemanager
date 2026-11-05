// SPDX-License-Identifier: GPL-3.0-or-later
//! Tags, comments and ratings, stored the way Dolphin/Baloo store them so
//! both file managers see the same data:
//!
//! * `user.xdg.tags`     – comma-separated tags (freedesktop convention)
//! * `user.xdg.comment`  – free text
//! * `user.baloo.rating` – 0..=10 (half stars; the UI uses whole stars = 2, 4…)
//!
//! Extended attributes live in the file system, so they follow the file when
//! it is moved or copied with attributes, and are absent on file systems that
//! don't support them (FAT, some network shares): [`supported`] tells.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

pub const TAGS: &str = "user.xdg.tags";
pub const COMMENT: &str = "user.xdg.comment";
pub const RATING: &str = "user.baloo.rating";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Meta {
    pub tags: Vec<String>,
    pub comment: String,
    /// 0..=10.
    pub rating: u8,
}

impl Meta {
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty() && self.comment.is_empty() && self.rating == 0
    }
}

fn cpath(p: &Path) -> io::Result<CString> {
    CString::new(p.as_os_str().as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

/// Read one attribute; `Ok(None)` when it isn't set.
pub fn get(path: &Path, name: &str) -> io::Result<Option<Vec<u8>>> {
    let p = cpath(path)?;
    let n = CString::new(name).expect("attribute name");
    let mut buf = vec![0u8; 256];
    loop {
        // SAFETY: valid C strings, buffer of the stated length.
        let r = unsafe { libc::getxattr(p.as_ptr(), n.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
        if r >= 0 {
            buf.truncate(r as usize);
            return Ok(Some(buf));
        }
        let e = io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::ENODATA) => return Ok(None),
            Some(libc::ERANGE) if buf.len() < 1 << 20 => buf.resize(buf.len() * 4, 0),
            _ => return Err(e),
        }
    }
}

/// Set an attribute, or remove it when `value` is empty.
pub fn set(path: &Path, name: &str, value: &[u8]) -> io::Result<()> {
    let p = cpath(path)?;
    let n = CString::new(name).expect("attribute name");
    // SAFETY: valid C strings and buffer.
    let r = unsafe {
        if value.is_empty() {
            libc::removexattr(p.as_ptr(), n.as_ptr())
        } else {
            libc::setxattr(p.as_ptr(), n.as_ptr(), value.as_ptr().cast(), value.len(), 0)
        }
    };
    if r == 0 {
        return Ok(());
    }
    let e = io::Error::last_os_error();
    if value.is_empty() && e.raw_os_error() == Some(libc::ENODATA) {
        return Ok(());
    }
    Err(e)
}

/// Can this file carry tags (does its file system support user xattrs)?
pub fn supported(path: &Path) -> bool {
    match get(path, TAGS) {
        Ok(_) => true,
        Err(e) => e.raw_os_error() != Some(libc::ENOTSUP),
    }
}

/// Split Baloo's comma-separated tag list. Tags can't contain commas; blank
/// entries and duplicates are dropped, order kept.
pub fn parse_tags(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in raw.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        if !out.iter().any(|x| x == t) {
            out.push(t.to_string());
        }
    }
    out
}

pub fn join_tags(tags: &[String]) -> String {
    parse_tags(&tags.iter().map(|t| t.replace(',', " ")).collect::<Vec<_>>().join(","))
        .join(",")
}

fn text(path: &Path, name: &str) -> io::Result<String> {
    Ok(get(path, name)?.map(|v| String::from_utf8_lossy(&v).into_owned()).unwrap_or_default())
}

pub fn read(path: &Path) -> io::Result<Meta> {
    let tags = parse_tags(&text(path, TAGS)?);
    let comment = text(path, COMMENT)?;
    let rating = text(path, RATING)?.trim().parse::<u8>().unwrap_or(0).min(10);
    Ok(Meta { tags, comment, rating })
}

pub fn write_tags(path: &Path, tags: &[String]) -> io::Result<()> {
    set(path, TAGS, join_tags(tags).as_bytes())
}

pub fn write_comment(path: &Path, comment: &str) -> io::Result<()> {
    set(path, COMMENT, comment.trim().as_bytes())
}

pub fn write_rating(path: &Path, rating: u8) -> io::Result<()> {
    let r = rating.min(10);
    let v = if r == 0 { String::new() } else { r.to_string() };
    set(path, RATING, v.as_bytes())
}

/// Every tag used by the given files, sorted, without duplicates.
pub fn collect_tags<'a>(paths: impl IntoIterator<Item = &'a Path>) -> Vec<String> {
    let mut all: Vec<String> = Vec::new();
    for p in paths {
        if let Ok(Some(v)) = get(p, TAGS) {
            for t in parse_tags(&String::from_utf8_lossy(&v)) {
                if !all.contains(&t) {
                    all.push(t);
                }
            }
        }
    }
    all.sort_by_key(|t| t.to_lowercase());
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_lists() {
        assert_eq!(parse_tags("work, urgent,,work , home"), vec!["work", "urgent", "home"]);
        assert_eq!(join_tags(&["a".into(), "b,c".into(), "a".into()]), "a,b c");
        assert!(parse_tags("").is_empty());
    }

    #[test]
    fn round_trip_on_disk() {
        // Use the build dir: /tmp may be a tmpfs without user xattrs.
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("temp dir");
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "x").expect("write");
        if !supported(&f) {
            eprintln!("skipping: no user xattrs here");
            return;
        }
        assert!(read(&f).expect("read").is_empty());
        write_tags(&f, &["Work".into(), "Q4".into()]).expect("tags");
        write_comment(&f, "  draft  ").expect("comment");
        write_rating(&f, 8).expect("rating");
        let m = read(&f).expect("read");
        assert_eq!(m, Meta { tags: vec!["Work".into(), "Q4".into()], comment: "draft".into(), rating: 8 });
        // Baloo's exact encoding.
        assert_eq!(get(&f, TAGS).expect("get"), Some(b"Work,Q4".to_vec()));
        assert_eq!(get(&f, RATING).expect("get"), Some(b"8".to_vec()));
        // Clearing removes the attributes.
        write_tags(&f, &[]).expect("clear");
        write_rating(&f, 0).expect("clear");
        write_comment(&f, "").expect("clear");
        assert_eq!(get(&f, TAGS).expect("get"), None);
        assert!(read(&f).expect("read").is_empty());
        assert_eq!(collect_tags([f.as_path()]), Vec::<String>::new());
    }
}
