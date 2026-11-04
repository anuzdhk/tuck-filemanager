// SPDX-License-Identifier: GPL-3.0-or-later
//! Fast local directory listing, meant to run on a worker thread.
//!
//! GIO's enumerator computes content types and icons per file and costs
//! ~1.3 s for 100,000 files. Reading the directory directly and stat-ing
//! each entry is several times faster; content types are then resolved once
//! per extension by the caller.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::entry::{EntryKind, EntryMeta};

#[derive(Debug, Clone)]
pub struct RawEntry {
    /// On-disk name, exactly as the OS gave it.
    pub name: OsString,
    pub meta: EntryMeta,
    pub is_symlink: bool,
    /// Symlink pointing nowhere.
    pub broken_link: bool,
    pub link_target: Option<PathBuf>,
    pub created: Option<i64>,
    pub mode: u32,
}

impl RawEntry {
    /// Build an entry for one path. Symlinks report their target's type and
    /// size, like every file manager; broken links are plain files.
    pub fn from_path(path: &Path, name: OsString) -> io::Result<Self> {
        let lmeta = fs::symlink_metadata(path)?;
        let is_symlink = lmeta.file_type().is_symlink();
        let (meta, broken) = if is_symlink {
            match fs::metadata(path) {
                Ok(m) => (m, false),
                Err(_) => (lmeta.clone(), true),
            }
        } else {
            (lmeta.clone(), false)
        };
        let ft = meta.file_type();
        let kind = if ft.is_dir() && !broken {
            EntryKind::Directory
        } else if ft.is_file() || broken || ft.is_symlink() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        let created = meta.created().ok().and_then(|t| {
            t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs() as i64)
        });
        let display = name.to_string_lossy().into_owned();
        Ok(RawEntry {
            meta: EntryMeta::new(display, kind, meta.size(), meta.mtime(), String::new()),
            link_target: if is_symlink { fs::read_link(path).ok() } else { None },
            is_symlink,
            broken_link: broken,
            created,
            mode: meta.mode(),
            name,
        })
    }
}

/// List `dir`, calling `send` with batches. The first batch is small so the
/// UI can paint immediately. `send` returns false to cancel. Entries that
/// vanish between readdir and stat are skipped silently.
pub fn list_dir(
    dir: &Path,
    first_batch: usize,
    batch: usize,
    mut send: impl FnMut(Vec<RawEntry>) -> bool,
) -> io::Result<()> {
    let rd = fs::read_dir(dir)?;
    let mut out = Vec::with_capacity(first_batch);
    let mut limit = first_batch.max(1);
    for de in rd {
        let Ok(de) = de else { continue };
        let name = de.file_name();
        match RawEntry::from_path(&de.path(), name) {
            Ok(e) => out.push(e),
            Err(_) => continue,
        }
        if out.len() >= limit {
            if !send(std::mem::take(&mut out)) {
                return Ok(());
            }
            limit = batch.max(1);
            out.reserve(limit);
        }
    }
    if !out.is_empty() {
        send(out);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn lists_files_dirs_and_links() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        fs::write(p.join("a.txt"), b"hello").unwrap();
        fs::create_dir(p.join("sub")).unwrap();
        symlink(p.join("sub"), p.join("link-to-dir")).unwrap();
        symlink(p.join("missing"), p.join("broken")).unwrap();

        let mut all = Vec::new();
        list_dir(p, 1, 2, |b| {
            all.extend(b);
            true
        })
        .unwrap();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<_> = all.iter().map(|e| e.name.to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["a.txt", "broken", "link-to-dir", "sub"]);

        let a = &all[0];
        assert_eq!(a.meta.size, 5);
        assert_eq!(a.meta.kind, EntryKind::File);
        let broken = &all[1];
        assert!(broken.is_symlink && broken.broken_link);
        assert_eq!(broken.meta.kind, EntryKind::File);
        let link = &all[2];
        assert!(link.is_symlink && !link.broken_link);
        assert_eq!(link.meta.kind, EntryKind::Directory);
        assert_eq!(link.link_target.as_deref(), Some(p.join("sub").as_path()));
    }

    #[test]
    fn batches_and_cancel() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..10 {
            fs::write(d.path().join(format!("f{i}")), b"").unwrap();
        }
        let mut sizes = Vec::new();
        list_dir(d.path(), 3, 4, |b| {
            sizes.push(b.len());
            true
        })
        .unwrap();
        assert_eq!(sizes, [3, 4, 3]);

        let mut calls = 0;
        list_dir(d.path(), 3, 4, |_| {
            calls += 1;
            false
        })
        .unwrap();
        assert_eq!(calls, 1, "returning false stops the listing");
    }

    #[test]
    fn missing_dir_is_an_error() {
        let err = list_dir(Path::new("/definitely/not/here"), 1, 1, |_| true).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
