// SPDX-License-Identifier: GPL-3.0-or-later
//! The freedesktop.org Trash specification (v1.0), so items trashed here show
//! up in Dolphin, Nautilus and `gio trash`, and vice versa.
//!
//! - Home trash: `$XDG_DATA_HOME/Trash/{files,info}`; info `Path` is absolute.
//! - Other drives: `$topdir/.Trash/$uid` if `.Trash` exists, is a real folder
//!   and has the sticky bit; otherwise `$topdir/.Trash-$uid`. Info `Path` is
//!   relative to `$topdir` there.
//! - A name is reserved by creating `info/NAME.trashinfo` with O_EXCL before
//!   the item is moved into `files/NAME`, so two trashers can't collide.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::names;
use crate::ops::{self, OpError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashedItem {
    /// Where the item was before it was trashed.
    pub original: PathBuf,
    /// Where it lives now (`…/files/NAME`).
    pub trashed: PathBuf,
    /// Its `.trashinfo` file.
    pub info: PathBuf,
    /// `DeletionDate` as written (`YYYY-MM-DDThh:mm:ss`, local time).
    pub deleted: String,
}

#[derive(Debug, Clone)]
pub struct Trash {
    home: PathBuf,
    uid: u32,
}

impl Trash {
    pub fn for_user() -> Self {
        // KIVO_TRASH_DIR redirects the home trash for tests.
        let home = std::env::var_os("KIVO_TRASH_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::paths::data_home().join("Trash"));
        Self { home, uid: unsafe { libc::getuid() } }
    }

    /// A trash rooted elsewhere (tests).
    pub fn with_home(home: PathBuf) -> Self {
        Self { home, uid: unsafe { libc::getuid() } }
    }

    pub fn home_dir(&self) -> &Path {
        &self.home
    }

    /// Move `path` to the trash.
    pub fn trash(&self, path: &Path) -> Result<TrashedItem, OpError> {
        let meta = fs::symlink_metadata(path).map_err(|e| OpError::io(&e, path, "move to the trash"))?;
        let path = absolute(path);
        let (dir, topdir) = self.trash_dir_for(&path, meta.dev())?;
        let files = dir.join("files");
        let info_dir = dir.join("info");
        for d in [&dir, &files, &info_dir] {
            if !d.exists() {
                fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(d)
                    .map_err(|e| no_trash_here(&path, &e))?;
            }
        }

        let recorded = match &topdir {
            Some(top) => path.strip_prefix(top).unwrap_or(&path).to_path_buf(),
            None => path.clone(),
        };
        let deleted = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
        let body = format!("[Trash Info]\nPath={}\nDeletionDate={}\n", encode_path(&recorded), deleted);

        let base = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "item".into());
        let mut candidate = base.clone();
        for attempt in 0..10_000u32 {
            if attempt > 0 {
                candidate = names::unique_name(&files, &base, &|n| info_dir.join(format!("{n}.trashinfo")).exists());
            }
            let info = info_dir.join(format!("{candidate}.trashinfo"));
            let f = OpenOptions::new().write(true).create_new(true).mode_compat(0o600).open(&info);
            let mut f = match f {
                Ok(f) => f,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(no_trash_here(&path, &e)),
            };
            if let Err(e) = f.write_all(body.as_bytes()).and_then(|_| f.sync_all()) {
                let _ = fs::remove_file(&info);
                return Err(OpError::io(&e, &path, "move to the trash"));
            }
            let target = files.join(&candidate);
            if target.symlink_metadata().is_ok() {
                // Orphan in files/ without info: pick another name.
                let _ = fs::remove_file(&info);
                continue;
            }
            return match ops::rename_noreplace(&path, &target) {
                Ok(()) => Ok(TrashedItem { original: path, trashed: target, info, deleted }),
                Err(e) => {
                    let _ = fs::remove_file(&info);
                    Err(OpError::io(&e, &path, "move to the trash"))
                }
            };
        }
        Err(OpError::new(&path, "Couldn't find a free name in the trash.", "Empty the trash and try again."))
    }

    /// Put a trashed item back. Never overwrites: if something now exists at
    /// the original location, the restored item gets a numbered name. Missing
    /// parent folders are recreated. Returns where it went.
    pub fn restore(&self, item: &TrashedItem) -> Result<PathBuf, OpError> {
        let parent = item.original.parent().unwrap_or(Path::new("/"));
        fs::create_dir_all(parent).map_err(|e| OpError::io(&e, parent, "restore into"))?;
        let name = item.original.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let target = names::unique_path(parent, &name);
        match ops::rename_noreplace(&item.trashed, &target) {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
                let r = ops::transfer(
                    ops::Mode::Move,
                    std::slice::from_ref(&item.trashed),
                    parent,
                    &mut ops::Fixed(ops::Resolution::KeepBoth),
                    &ops::Cancel::new(),
                );
                if let Some(e) = r.errors.into_iter().next() {
                    return Err(e);
                }
            }
            Err(e) => return Err(OpError::io(&e, &item.trashed, "restore")),
        }
        let _ = fs::remove_file(&item.info);
        Ok(target)
    }

    /// Everything in the home trash and in the trash folders of mounted drives.
    pub fn list(&self) -> Vec<TrashedItem> {
        let mut out = read_trash_dir(&self.home, None);
        for top in mount_points() {
            for dir in [top.join(".Trash").join(self.uid.to_string()), top.join(format!(".Trash-{}", self.uid))] {
                if dir.join("info").is_dir() && dir != self.home {
                    out.extend(read_trash_dir(&dir, Some(&top)));
                }
            }
        }
        out
    }

    /// Find the trashed item matching what GIO reports for a `trash:///` entry.
    pub fn find(&self, original: &Path, deleted: Option<&str>) -> Option<TrashedItem> {
        let mut hits: Vec<_> = self.list().into_iter().filter(|i| i.original == original).collect();
        if let Some(d) = deleted
            && let Some(i) = hits.iter().position(|i| i.deleted == d) {
                return Some(hits.swap_remove(i));
            }
        // Newest first.
        hits.sort_by(|a, b| b.deleted.cmp(&a.deleted));
        hits.into_iter().next()
    }

    /// Permanently delete one trashed item.
    pub fn purge(&self, item: &TrashedItem) -> Result<(), OpError> {
        let r = ops::delete(std::slice::from_ref(&item.trashed), &mut ops::Fixed(ops::Resolution::Skip), &ops::Cancel::new());
        if let Some(e) = r.errors.into_iter().next() {
            return Err(e);
        }
        let _ = fs::remove_file(&item.info);
        Ok(())
    }

    /// Which trash folder serves `path`; also its top dir for non-home trash.
    fn trash_dir_for(&self, path: &Path, dev: u64) -> Result<(PathBuf, Option<PathBuf>), OpError> {
        let home_dev = nearest_existing(&self.home).and_then(|p| fs::metadata(p).ok()).map(|m| m.dev());
        if home_dev == Some(dev) {
            return Ok((self.home.clone(), None));
        }
        let top = mount_root(path, dev);
        let shared = top.join(".Trash");
        if let Ok(m) = fs::symlink_metadata(&shared) {
            let sticky = m.permissions().mode() & 0o1000 != 0;
            if m.is_dir() && !m.file_type().is_symlink() && sticky {
                let dir = shared.join(self.uid.to_string());
                if dir.is_dir() || fs::DirBuilder::new().mode(0o700).create(&dir).is_ok() {
                    return Ok((dir, Some(top)));
                }
            }
        }
        Ok((top.join(format!(".Trash-{}", self.uid)), Some(top)))
    }
}

fn no_trash_here(path: &Path, e: &io::Error) -> OpError {
    if e.kind() == io::ErrorKind::ReadOnlyFilesystem {
        return OpError::io(e, path, "move to the trash");
    }
    OpError::new(
        path,
        format!(
            "“{}” can't be moved to the trash on this drive.",
            path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        ),
        "You can delete it permanently instead.",
    )
}

trait ModeCompat {
    fn mode_compat(&mut self, m: u32) -> &mut Self;
}
impl ModeCompat for OpenOptions {
    fn mode_compat(&mut self, m: u32) -> &mut Self {
        std::os::unix::fs::OpenOptionsExt::mode(self, m)
    }
}

fn absolute(p: &Path) -> PathBuf {
    if p.is_absolute() {
        crate::paths::normalize(p)
    } else {
        crate::paths::normalize(&std::env::current_dir().unwrap_or_default().join(p))
    }
}

fn nearest_existing(p: &Path) -> Option<&Path> {
    let mut cur = Some(p);
    while let Some(c) = cur {
        if c.exists() {
            return Some(c);
        }
        cur = c.parent();
    }
    None
}

/// Highest ancestor of `path` still on device `dev`.
fn mount_root(path: &Path, dev: u64) -> PathBuf {
    let mut top = path.to_path_buf();
    let mut cur = path.parent();
    while let Some(p) = cur {
        match fs::metadata(p) {
            Ok(m) if m.dev() == dev => {
                top = p.to_path_buf();
                cur = p.parent();
            }
            _ => break,
        }
    }
    top
}

fn mount_points() -> Vec<PathBuf> {
    let text = fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    text.lines()
        .filter_map(|l| l.split_whitespace().nth(1))
        .map(|p| PathBuf::from(p.replace("\\040", " ")))
        .collect()
}

fn read_trash_dir(dir: &Path, topdir: Option<&Path>) -> Vec<TrashedItem> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(dir.join("info")) else { return out };
    for e in rd.flatten() {
        let info = e.path();
        let Some(name) = info.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".trashinfo")) else {
            continue;
        };
        let Ok(text) = fs::read_to_string(&info) else { continue };
        let Some((path, deleted)) = parse_info(&text) else { continue };
        let original = match topdir {
            Some(t) if path.is_relative() => t.join(path),
            _ => path,
        };
        out.push(TrashedItem { original, trashed: dir.join("files").join(name), info, deleted });
    }
    out
}

/// Parse a `.trashinfo` file into (Path, DeletionDate).
pub fn parse_info(text: &str) -> Option<(PathBuf, String)> {
    let mut in_section = false;
    let (mut path, mut date) = (None, String::new());
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line == "[Trash Info]";
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some(v) = line.strip_prefix("Path=") {
            path = decode_path(v);
        } else if let Some(v) = line.strip_prefix("DeletionDate=") {
            date = v.to_string();
        }
    }
    path.map(|p| (p, date))
}

/// Percent-encode a path for `.trashinfo` (RFC 2396; '/' kept).
pub fn encode_path(p: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::new();
    for &b in p.as_os_str().as_bytes() {
        let keep = b.is_ascii_alphanumeric() || b"-_.!~*'()/".contains(&b);
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn decode_path(s: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Trash) {
        let d = tempfile::tempdir().unwrap();
        let t = Trash::with_home(d.path().join("share/Trash"));
        (d, t)
    }

    #[test]
    fn encodes_like_the_spec() {
        let p = Path::new("/home/u/My Files/naïve 100%.txt");
        let e = encode_path(p);
        assert_eq!(e, "/home/u/My%20Files/na%C3%AFve%20100%25.txt");
        assert_eq!(decode_path(&e).unwrap(), p);
    }

    #[test]
    fn trash_writes_spec_files_and_restores() {
        let (d, t) = setup();
        let f = d.path().join("doc one.txt");
        fs::write(&f, "hello").unwrap();
        let item = t.trash(&f).unwrap();
        assert!(!f.exists());
        assert_eq!(item.trashed, d.path().join("share/Trash/files/doc one.txt"));
        let info = fs::read_to_string(&item.info).unwrap();
        assert!(info.starts_with("[Trash Info]\nPath="));
        assert!(info.contains("doc%20one.txt"));
        assert!(info.contains("DeletionDate=20"));
        let mode = fs::metadata(d.path().join("share/Trash")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);

        let back = t.restore(&item).unwrap();
        assert_eq!(back, f);
        assert_eq!(fs::read(&f).unwrap(), b"hello");
        assert!(!item.info.exists());
    }

    #[test]
    fn same_name_twice_gets_unique_trash_names() {
        let (d, t) = setup();
        let f = d.path().join("a.txt");
        fs::write(&f, "1").unwrap();
        let one = t.trash(&f).unwrap();
        fs::write(&f, "2").unwrap();
        let two = t.trash(&f).unwrap();
        assert_ne!(one.trashed, two.trashed);
        assert_eq!(two.trashed.file_name().unwrap(), "a (2).txt");
        assert_eq!(t.list().len(), 2);
    }

    #[test]
    fn restore_never_overwrites_and_recreates_parents() {
        let (d, t) = setup();
        let dir = d.path().join("gone/deeper");
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("x.txt");
        fs::write(&f, "trashed").unwrap();
        let item = t.trash(&f).unwrap();
        fs::remove_dir_all(d.path().join("gone")).unwrap();
        assert_eq!(t.restore(&item).unwrap(), f);

        let item = t.trash(&f).unwrap();
        fs::write(&f, "new").unwrap();
        let back = t.restore(&item).unwrap();
        assert_eq!(back, dir.join("x (2).txt"));
        assert_eq!(fs::read(&f).unwrap(), b"new");
        assert_eq!(fs::read(&back).unwrap(), b"trashed");
    }

    #[test]
    fn folders_and_lookup_and_purge() {
        let (d, t) = setup();
        let dir = d.path().join("Photos");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("p.jpg"), "x").unwrap();
        let item = t.trash(&dir).unwrap();
        assert!(item.trashed.join("p.jpg").exists());
        let found = t.find(&dir, Some(&item.deleted)).unwrap();
        assert_eq!(found, item);
        t.purge(&found).unwrap();
        assert!(!item.trashed.exists() && !item.info.exists());
        assert!(t.list().is_empty());
    }

    #[test]
    fn parses_info() {
        let (p, d) = parse_info("[Trash Info]\nPath=foo/bar%20baz\nDeletionDate=2004-08-31T22:32:08\n").unwrap();
        assert_eq!(p, Path::new("foo/bar baz"));
        assert_eq!(d, "2004-08-31T22:32:08");
        assert!(parse_info("[Other]\nPath=/x\n").is_none());
    }

    #[test]
    fn missing_file_is_explained() {
        let (d, t) = setup();
        let e = t.trash(&d.path().join("nope")).unwrap_err();
        assert!(e.message.contains("no longer exists"));
    }
}
