// SPDX-License-Identifier: GPL-3.0-or-later
//! Cleaner: find caches that are safe to delete and free the space.
//!
//! Only a fixed list of well-known cache folders is ever touched, plus the
//! Trash when the user asks. Cleaning empties a folder's contents but keeps
//! the folder; symbolic links are removed without following them, so
//! nothing outside a cache folder can be deleted. Browser caches are skipped
//! while the browser is running.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::ops::Cancel;
use crate::trash::Trash;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Folders whose contents are deleted.
    Cache,
    /// Everything in the Trash, deleted for good.
    Trash,
}

#[derive(Debug, Clone)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub detail: String,
    pub paths: Vec<PathBuf>,
    /// Ticked when the Cleaner opens.
    pub default_on: bool,
    /// Process names that must not be running while this is cleaned.
    pub needs_closed: Vec<&'static str>,
    pub kind: Kind,
}

#[derive(Debug, Clone)]
pub struct Found {
    pub item: Item,
    pub bytes: u64,
    pub files: u64,
    /// "Firefox is running" when it can't be cleaned now.
    pub blocked_by: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub freed: u64,
    pub files: u64,
    pub errors: Vec<String>,
}

struct Known {
    id: &'static str,
    name: &'static str,
    detail: &'static str,
    /// Relative to the cache folder (`~/.cache`) or, with a leading `~/`,
    /// to the home folder.
    paths: &'static [&'static str],
    default_on: bool,
    needs_closed: &'static [&'static str],
}

const KNOWN: &[Known] = &[
    Known { id: "thumbnails", name: "File previews", detail: "Thumbnails of your pictures, videos and documents. They're made again when needed.", paths: &["thumbnails"], default_on: true, needs_closed: &[] },
    Known { id: "shaders", name: "Graphics shader caches", detail: "Made again by your graphics driver; games and apps may start a little slower once.", paths: &["mesa_shader_cache", "mesa_shader_cache_db", "nvidia", "radv_builtin_shaders64"], default_on: true, needs_closed: &[] },
    Known { id: "fontconfig", name: "Font cache", detail: "Rebuilt automatically.", paths: &["fontconfig"], default_on: true, needs_closed: &[] },
    Known { id: "firefox", name: "Firefox cache", detail: "Stored copies of web pages and images. Your history, passwords and logins are kept.", paths: &["mozilla/firefox", "~/snap/firefox/common/.cache"], default_on: true, needs_closed: &["firefox", "firefox-bin"] },
    Known { id: "chrome", name: "Chrome cache", detail: "Stored copies of web pages and images. Your history, passwords and logins are kept.", paths: &["google-chrome"], default_on: true, needs_closed: &["chrome"] },
    Known { id: "chromium", name: "Chromium cache", detail: "Stored copies of web pages and images. Your history, passwords and logins are kept.", paths: &["chromium", "~/snap/chromium/common/.cache"], default_on: true, needs_closed: &["chromium", "chromium-browse"] },
    Known { id: "brave", name: "Brave cache", detail: "Stored copies of web pages and images. Your history, passwords and logins are kept.", paths: &["BraveSoftware"], default_on: true, needs_closed: &["brave", "brave-browser"] },
    Known { id: "edge", name: "Microsoft Edge cache", detail: "Stored copies of web pages and images. Your history, passwords and logins are kept.", paths: &["microsoft-edge"], default_on: true, needs_closed: &["msedge"] },
    Known { id: "pip", name: "Python package downloads (pip)", detail: "Downloaded again the next time they're needed.", paths: &["pip"], default_on: true, needs_closed: &[] },
    Known { id: "npm", name: "Node package downloads (npm)", detail: "Downloaded again the next time they're needed.", paths: &["~/.npm/_cacache"], default_on: true, needs_closed: &[] },
    Known { id: "yarn", name: "Node package downloads (Yarn)", detail: "Downloaded again the next time they're needed.", paths: &["yarn"], default_on: true, needs_closed: &[] },
    Known { id: "go-build", name: "Go build cache", detail: "Rebuilt by the next go build.", paths: &["go-build"], default_on: true, needs_closed: &[] },
    Known { id: "cargo", name: "Rust crate downloads (Cargo)", detail: "Downloaded again by the next build; offline builds need them.", paths: &["~/.cargo/registry/cache"], default_on: false, needs_closed: &[] },
];

/// Cache folders handled above (or that must never be touched), by their
/// top-level name in `~/.cache`.
fn known_top(name: &str) -> bool {
    // Tuck's own caches stay: its icons are in use while it runs.
    const KEEP: &[&str] = &["tuck", "mozilla", "thumbnails", "fontconfig", "pip", "yarn", "go-build", "google-chrome", "chromium", "BraveSoftware", "microsoft-edge", "mesa_shader_cache", "mesa_shader_cache_db", "nvidia", "radv_builtin_shaders64"];
    KEEP.contains(&name)
}

fn resolve(rel: &str, home: &Path, cache: &Path) -> PathBuf {
    match rel.strip_prefix("~/") {
        Some(r) => home.join(r),
        None => cache.join(rel),
    }
}

/// Everything the Cleaner offers: the known caches, each other app's cache
/// folder (off by default), and the Trash (off by default).
pub fn catalogue(home: &Path, cache: &Path, trash: Option<&Trash>) -> Vec<Item> {
    let mut items: Vec<Item> = KNOWN
        .iter()
        .map(|k| Item {
            id: k.id.into(),
            name: k.name.into(),
            detail: k.detail.into(),
            paths: k.paths.iter().map(|p| resolve(p, home, cache)).collect(),
            default_on: k.default_on,
            needs_closed: k.needs_closed.to_vec(),
            kind: Kind::Cache,
        })
        .collect();
    if let Ok(rd) = fs::read_dir(cache) {
        let mut others: Vec<(String, PathBuf)> = rd
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .filter(|(n, _)| !known_top(n))
            .collect();
        others.sort();
        for (name, path) in others {
            items.push(Item {
                id: format!("app:{name}"),
                name: format!("{name} cache"),
                detail: "This app's own cache. Usually safe: the app makes it again, but it may be slower or sign you out once.".into(),
                paths: vec![path],
                default_on: false,
                needs_closed: Vec::new(),
                kind: Kind::Cache,
            });
        }
    }
    if let Some(t) = trash {
        items.push(Item {
            id: "trash".into(),
            name: "Trash".into(),
            detail: "Files you moved to the Trash, deleted for good. This can't be undone.".into(),
            paths: vec![t.home_dir().join("files")],
            default_on: false,
            needs_closed: Vec::new(),
            kind: Kind::Trash,
        });
    }
    items
}

/// Bytes and files under `path`, without following symlinks or leaving its
/// file system.
pub fn size_of(path: &Path) -> (u64, u64) {
    let Ok(meta) = fs::symlink_metadata(path) else { return (0, 0) };
    if !meta.is_dir() {
        return (meta.blocks() * 512, 1);
    }
    let dev = meta.dev();
    let (mut bytes, mut files) = (0u64, 0u64);
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                if m.dev() == dev {
                    stack.push(e.path());
                }
            } else {
                bytes += m.blocks() * 512;
                files += 1;
            }
        }
    }
    (bytes, files)
}

/// The first of `names` that's running, if any (from /proc/*/comm).
pub fn running(names: &[&str], proc_dir: &Path) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    let rd = fs::read_dir(proc_dir).ok()?;
    for e in rd.flatten() {
        if !e.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if let Ok(comm) = fs::read_to_string(e.path().join("comm")) {
            let comm = comm.trim();
            if let Some(n) = names.iter().find(|n| **n == comm) {
                return Some(n.to_string());
            }
        }
    }
    None
}

fn pretty(process: &str) -> String {
    let n = process.split(['-', '_']).next().unwrap_or(process);
    let mut c = n.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// Measure every item (skipping empty ones) and note which are blocked.
pub fn scan(items: Vec<Item>, proc_dir: &Path, cancel: &Cancel) -> Vec<Found> {
    let mut out = Vec::new();
    for item in items {
        if cancel.is_cancelled() {
            break;
        }
        let (mut bytes, mut files) = (0, 0);
        for p in &item.paths {
            let (b, f) = size_of(p);
            bytes += b;
            files += f;
        }
        if bytes == 0 && files == 0 {
            continue;
        }
        let blocked_by = running(&item.needs_closed, proc_dir).map(|p| format!("Close {} first", pretty(&p)));
        out.push(Found { item, bytes, files, blocked_by });
    }
    out.sort_by_key(|f| std::cmp::Reverse(f.bytes));
    out
}

/// Delete everything inside `dir`, keeping `dir` itself. Symlinks are
/// removed, never followed.
fn empty_dir(dir: &Path, cancel: &Cancel, report: &mut Report) {
    let Ok(meta) = fs::symlink_metadata(dir) else { return };
    if !meta.is_dir() {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if cancel.is_cancelled() {
            return;
        }
        let p = e.path();
        let Ok(m) = fs::symlink_metadata(&p) else { continue };
        let (bytes, files) = size_of(&p);
        // remove_dir_all doesn't follow symlinks inside the tree.
        let r = if m.is_dir() { fs::remove_dir_all(&p) } else { fs::remove_file(&p) };
        match r {
            Ok(()) => {
                report.freed += bytes;
                report.files += files;
            }
            Err(err) => {
                // Partly removed: count what went.
                let (left, left_files) = size_of(&p);
                report.freed += bytes.saturating_sub(left);
                report.files += files.saturating_sub(left_files);
                report.errors.push(format!("{}: {err}", p.display()));
            }
        }
    }
}

/// Clean the chosen items. Blocked items are skipped.
pub fn clean(found: &[Found], trash: Option<&Trash>, cancel: &Cancel) -> Report {
    let mut report = Report::default();
    for f in found.iter().filter(|f| f.blocked_by.is_none()) {
        match f.item.kind {
            Kind::Cache => {
                for p in &f.item.paths {
                    empty_dir(p, cancel, &mut report);
                }
            }
            Kind::Trash => {
                let Some(t) = trash else { continue };
                for item in t.list() {
                    if cancel.is_cancelled() {
                        break;
                    }
                    let (bytes, files) = size_of(&item.trashed);
                    match t.purge(&item) {
                        Ok(()) => {
                            report.freed += bytes;
                            report.files += files;
                        }
                        Err(e) => report.errors.push(e.to_string()),
                    }
                }
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, bytes: usize) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![b'x'; bytes]).unwrap();
    }

    #[test]
    fn finds_measures_and_cleans_only_caches() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        let cache = home.join(".cache");
        let proc_dir = d.path().join("proc");
        fs::create_dir_all(&proc_dir).unwrap();
        write(&cache.join("thumbnails/large/a.png"), 10_000);
        write(&cache.join("pip/http/b"), 20_000);
        write(&cache.join("someapp/data.bin"), 5_000);
        write(&cache.join("tuck/icons/x.svg"), 100);
        write(&home.join(".npm/_cacache/c"), 3_000);
        // Something precious outside, reachable through a symlink in a cache.
        write(&home.join("Documents/keep.txt"), 1_000);
        std::os::unix::fs::symlink(home.join("Documents"), cache.join("thumbnails/link")).unwrap();
        std::os::unix::fs::symlink(home.join("Documents/keep.txt"), cache.join("pip/keep-link")).unwrap();

        let items = catalogue(&home, &cache, None);
        assert!(items.iter().any(|i| i.id == "app:someapp" && !i.default_on), "other apps are listed, unticked");
        assert!(!items.iter().any(|i| i.id == "app:tuck"), "Tuck's own cache is never offered");
        let found = scan(items, &proc_dir, &Cancel::new());
        let ids: Vec<&str> = found.iter().map(|f| f.item.id.as_str()).collect();
        assert!(ids.contains(&"thumbnails") && ids.contains(&"pip") && ids.contains(&"npm") && ids.contains(&"app:someapp"));
        assert!(!ids.contains(&"firefox"), "empty caches aren't listed");
        assert!(found.windows(2).all(|w| w[0].bytes >= w[1].bytes), "largest first");

        let chosen: Vec<Found> = found.into_iter().filter(|f| f.item.default_on).collect();
        let r = clean(&chosen, None, &Cancel::new());
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(r.freed >= 33_000, "freed {}", r.freed);
        // Emptied, but the folders themselves remain.
        assert!(cache.join("thumbnails").is_dir() && fs::read_dir(cache.join("thumbnails")).unwrap().next().is_none());
        assert!(cache.join("pip").is_dir() && fs::read_dir(cache.join("pip")).unwrap().next().is_none());
        // Untouched: the unticked app cache, Tuck's cache, and the symlink targets.
        assert!(cache.join("someapp/data.bin").exists());
        assert!(cache.join("tuck/icons/x.svg").exists());
        assert!(home.join("Documents/keep.txt").exists(), "a symlink must never take its target with it");
    }

    #[test]
    fn running_browsers_block_their_cache() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        let cache = home.join(".cache");
        write(&cache.join("mozilla/firefox/abc.default/cache2/entries/1"), 4_000);
        let proc_dir = d.path().join("proc");
        write(&proc_dir.join("4242/comm"), 0);
        fs::write(proc_dir.join("4242/comm"), "firefox\n").unwrap();
        write(&proc_dir.join("self/comm"), 0);

        let found = scan(catalogue(&home, &cache, None), &proc_dir, &Cancel::new());
        let ff = found.iter().find(|f| f.item.id == "firefox").expect("firefox listed");
        assert_eq!(ff.blocked_by.as_deref(), Some("Close Firefox first"));
        let r = clean(&found, None, &Cancel::new());
        assert_eq!(r.freed, 0);
        assert!(cache.join("mozilla/firefox/abc.default/cache2/entries/1").exists(), "skipped while running");
    }

    #[test]
    fn empties_the_trash_when_asked() {
        let d = tempfile::tempdir().unwrap();
        let t = Trash::with_home(d.path().join("Trash"));
        write(&d.path().join("a.txt"), 2_000);
        t.trash(&d.path().join("a.txt")).unwrap();
        let home = d.path().join("home");
        let items: Vec<Item> = catalogue(&home, &home.join(".cache"), Some(&t)).into_iter().filter(|i| i.kind == Kind::Trash).collect();
        assert!(!items[0].default_on, "the Trash is never ticked by default");
        let found = scan(items, d.path(), &Cancel::new());
        let r = clean(&found, Some(&t), &Cancel::new());
        assert!(r.errors.is_empty() && r.files == 1);
        assert!(t.list().is_empty());
    }
}
