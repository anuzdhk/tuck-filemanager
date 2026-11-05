// SPDX-License-Identifier: GPL-3.0-or-later
//! Git status for the folder being shown, like Dolphin's git plugin: which
//! items are modified, added, untracked or in conflict, and the branch.
//!
//! Runs `git status --porcelain=v1 -z` once per folder (off the main
//! thread). A folder counts as changed when anything inside it is.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum State {
    /// Ordered by importance: a folder shows its most important child state.
    Clean,
    Untracked,
    Renamed,
    Added,
    Deleted,
    Modified,
    Conflicted,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Clean => "Unchanged",
            State::Untracked => "Not tracked",
            State::Renamed => "Renamed",
            State::Added => "Added",
            State::Deleted => "Deleted",
            State::Modified => "Modified",
            State::Conflicted => "Conflict",
        }
    }

    /// Short id for CSS classes and icon names.
    pub fn id(self) -> &'static str {
        match self {
            State::Clean => "clean",
            State::Untracked => "untracked",
            State::Renamed => "renamed",
            State::Added => "added",
            State::Deleted => "deleted",
            State::Modified => "modified",
            State::Conflicted => "conflict",
        }
    }

    fn from_xy(x: u8, y: u8) -> State {
        match (x, y) {
            (b'?', b'?') => State::Untracked,
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => State::Conflicted,
            (_, b'M') | (b'M', _) | (_, b'T') | (b'T', _) => State::Modified,
            (_, b'D') | (b'D', _) => State::Deleted,
            (b'A', _) => State::Added,
            (b'R', _) | (b'C', _) => State::Renamed,
            _ => State::Modified,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Status {
    pub root: PathBuf,
    pub branch: String,
    /// Changed paths relative to `root` (folders end without a slash).
    pub changes: HashMap<PathBuf, State>,
}

impl Status {
    /// State of `path` (absolute). Folders take the strongest state found
    /// inside them; tracked, unchanged files are Clean.
    pub fn state_of(&self, path: &Path) -> State {
        let canon;
        let rel = match path.strip_prefix(&self.root) {
            Ok(r) => r,
            // A symlinked route to the repository (git reports real paths).
            Err(_) => {
                canon = match path.canonicalize() {
                    Ok(c) => c,
                    Err(_) => return State::Clean,
                };
                match canon.strip_prefix(&self.root) {
                    Ok(r) => r,
                    Err(_) => return State::Clean,
                }
            }
        };
        if let Some(s) = self.changes.get(rel) {
            return *s;
        }
        // Inside an untracked folder?
        for anc in rel.ancestors().skip(1) {
            if anc.as_os_str().is_empty() {
                break;
            }
            if self.changes.get(anc) == Some(&State::Untracked) {
                return State::Untracked;
            }
        }
        self.changes
            .iter()
            .filter(|(p, _)| p.starts_with(rel) && p.as_path() != rel)
            .map(|(_, s)| *s)
            .max()
            .unwrap_or(State::Clean)
    }
}

/// Parse `git status --porcelain=v1 -z` output.
pub fn parse_porcelain(out: &[u8]) -> HashMap<PathBuf, State> {
    let mut map = HashMap::new();
    let mut parts = out.split(|b| *b == 0).filter(|p| !p.is_empty());
    while let Some(rec) = parts.next() {
        if rec.len() < 4 {
            continue;
        }
        let (x, y) = (rec[0], rec[1]);
        let path = String::from_utf8_lossy(&rec[3..]);
        let path = PathBuf::from(path.trim_end_matches('/'));
        let state = State::from_xy(x, y);
        if x == b'R' || x == b'C' {
            // The source path follows as its own record.
            parts.next();
        }
        if x == b'!' {
            continue;
        }
        map.insert(path, state);
    }
    map
}

fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

/// Status of the repository containing `dir`, or `None` outside a work tree
/// (or when git isn't installed). Only `dir`'s part of the tree is scanned.
pub fn status(dir: &Path) -> Option<Status> {
    let top = git(dir, &["rev-parse", "--show-toplevel"])?;
    let root = PathBuf::from(String::from_utf8_lossy(&top).trim());
    let branch = git(dir, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|b| String::from_utf8_lossy(&b).trim().to_string())
        .unwrap_or_default();
    let out = git(dir, &["status", "--porcelain=v1", "-z", "--untracked-files=normal", "--", "."])?;
    Some(Status { root, branch, changes: parse_porcelain(&out) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain() {
        let out = b" M src/main.rs\0?? new/\0A  added.txt\0R  b.txt\0a.txt\0UU both.rs\0 D gone.md\0";
        let m = parse_porcelain(out);
        assert_eq!(m[Path::new("src/main.rs")], State::Modified);
        assert_eq!(m[Path::new("new")], State::Untracked);
        assert_eq!(m[Path::new("added.txt")], State::Added);
        assert_eq!(m[Path::new("b.txt")], State::Renamed);
        assert!(!m.contains_key(Path::new("a.txt")));
        assert_eq!(m[Path::new("both.rs")], State::Conflicted);
        assert_eq!(m[Path::new("gone.md")], State::Deleted);
    }

    #[test]
    fn folder_states() {
        let s = Status {
            root: "/r".into(),
            branch: "main".into(),
            changes: parse_porcelain(b" M src/a/x.rs\0?? src/b.rs\0?? new/\0"),
        };
        assert_eq!(s.state_of(Path::new("/r/src")), State::Modified);
        assert_eq!(s.state_of(Path::new("/r/src/a")), State::Modified);
        assert_eq!(s.state_of(Path::new("/r/src/b.rs")), State::Untracked);
        assert_eq!(s.state_of(Path::new("/r/new/deep/f.txt")), State::Untracked);
        assert_eq!(s.state_of(Path::new("/r/README")), State::Clean);
        assert_eq!(s.state_of(Path::new("/elsewhere")), State::Clean);
    }

    #[test]
    fn real_repository() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().expect("temp dir");
        let d = dir.path();
        let run = |args: &[&str]| {
            assert!(Command::new("git").arg("-C").arg(d).args(args).stdout(Stdio::null()).stderr(Stdio::null()).status().expect("git").success());
        };
        run(&["init", "-q", "-b", "main"]);
        std::fs::write(d.join("a.txt"), "1").expect("write");
        run(&["add", "a.txt"]);
        run(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);
        std::fs::write(d.join("a.txt"), "2").expect("write");
        std::fs::write(d.join("b.txt"), "new").expect("write");
        std::fs::create_dir(d.join("sub")).expect("mkdir");
        std::fs::write(d.join("sub/c.txt"), "c").expect("write");
        let s = status(d).expect("status");
        assert_eq!(s.branch, "main");
        // From a subfolder, paths are still relative to the repository root.
        let sub = status(&d.join("sub")).expect("status");
        assert_eq!(sub.state_of(&d.join("sub/c.txt")), State::Untracked);
        let root = s.root.clone();
        assert_eq!(s.state_of(&root.join("a.txt")), State::Modified);
        assert_eq!(s.state_of(&root.join("b.txt")), State::Untracked);
        assert!(status(Path::new("/")).is_none() || Path::new("/.git").exists());
    }
}
