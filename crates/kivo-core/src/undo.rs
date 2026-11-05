// SPDX-License-Identifier: GPL-3.0-or-later
//! Undo history. Every undo is itself non-destructive: things this app
//! created are moved to the trash, never deleted outright.

use std::path::PathBuf;

use crate::ops::{self, Mode, OpError};
use crate::trash::{Trash, TrashedItem};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Copy or move: (source, destination) per top-level item.
    Transferred { mode: Mode, pairs: Vec<(PathBuf, PathBuf)> },
    /// (old path, new path).
    Renamed(Vec<(PathBuf, PathBuf)>),
    Trashed(Vec<TrashedItem>),
    /// New folders, files, links or duplicates.
    Created(Vec<PathBuf>),
}

impl Action {
    /// "Undo Move to Trash" style label.
    pub fn label(&self) -> String {
        let n = |k: usize| if k == 1 { String::new() } else { format!(" of {k} Items") };
        match self {
            Action::Transferred { mode: Mode::Copy, pairs } => format!("Copy{}", n(pairs.len())),
            Action::Transferred { mode: Mode::Move, pairs } => format!("Move{}", n(pairs.len())),
            Action::Renamed(v) => format!("Rename{}", n(v.len())),
            Action::Trashed(v) => format!("Move to Trash{}", n(v.len())),
            Action::Created(v) => format!("New Item{}", if v.len() == 1 { String::new() } else { "s".into() }),
        }
    }
}

/// Reverse `action`. Returns the paths that now hold the restored items.
pub fn undo(action: &Action, trash: &Trash) -> Result<Vec<PathBuf>, Vec<OpError>> {
    let mut errors = Vec::new();
    let mut restored = Vec::new();
    match action {
        Action::Transferred { mode: Mode::Copy, pairs } => {
            for (_, dst) in pairs {
                if let Err(e) = trash.trash(dst) {
                    errors.push(e);
                }
            }
        }
        Action::Transferred { mode: Mode::Move, pairs } => {
            for (src, dst) in pairs.iter().rev() {
                let parent = src.parent().unwrap_or(std::path::Path::new("/"));
                let _ = std::fs::create_dir_all(parent);
                match ops::rename_noreplace(dst, src) {
                    Ok(()) => restored.push(src.clone()),
                    Err(_) => {
                        // Different device, or the name is taken now: move with keep-both.
                        let r = ops::transfer(
                            Mode::Move,
                            std::slice::from_ref(dst),
                            parent,
                            &mut ops::Fixed(ops::Resolution::KeepBoth),
                            &ops::Cancel::new(),
                        );
                        restored.extend(r.done.into_iter().map(|(_, d)| d));
                        errors.extend(r.errors);
                    }
                }
            }
        }
        Action::Renamed(v) => {
            let back: Vec<(PathBuf, String)> = v
                .iter()
                .map(|(old, new)| {
                    (new.clone(), old.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
                })
                .collect();
            match ops::rename_many(&back) {
                Ok(done) => restored.extend(done.into_iter().map(|(_, n)| n)),
                Err(e) => errors.push(e),
            }
        }
        Action::Trashed(items) => {
            for item in items {
                match trash.restore(item) {
                    Ok(p) => restored.push(p),
                    Err(e) => errors.push(e),
                }
            }
        }
        Action::Created(paths) => {
            for p in paths {
                if let Err(e) = trash.trash(p) {
                    errors.push(e);
                }
            }
        }
    }
    if errors.is_empty() { Ok(restored) } else { Err(errors) }
}

/// Bounded undo stack, shared by all windows.
#[derive(Debug, Default)]
pub struct History {
    stack: Vec<Action>,
}

impl History {
    const LIMIT: usize = 100;

    pub fn push(&mut self, a: Action) {
        let empty = match &a {
            Action::Transferred { pairs, .. } => pairs.is_empty(),
            Action::Renamed(v) => v.is_empty(),
            Action::Trashed(v) => v.is_empty(),
            Action::Created(v) => v.is_empty(),
        };
        if empty {
            return;
        }
        self.stack.push(a);
        if self.stack.len() > Self::LIMIT {
            self.stack.remove(0);
        }
    }

    pub fn pop(&mut self) -> Option<Action> {
        self.stack.pop()
    }

    pub fn peek(&self) -> Option<&Action> {
        self.stack.last()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn undo_each_kind() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        let trash = Trash::with_home(p.join("Trash"));
        fs::create_dir(p.join("src")).unwrap();
        fs::create_dir(p.join("dst")).unwrap();
        fs::write(p.join("src/a.txt"), "a").unwrap();

        // Move, then undo: back where it was.
        let r = ops::transfer(Mode::Move, &[p.join("src/a.txt")], &p.join("dst"), &mut ops::Fixed(ops::Resolution::Skip), &ops::Cancel::new());
        let a = Action::Transferred { mode: Mode::Move, pairs: r.done };
        assert_eq!(undo(&a, &trash).unwrap(), vec![p.join("src/a.txt")]);
        assert!(p.join("src/a.txt").exists() && !p.join("dst/a.txt").exists());

        // Copy, then undo: the copy goes to the trash, not oblivion.
        let r = ops::transfer(Mode::Copy, &[p.join("src/a.txt")], &p.join("dst"), &mut ops::Fixed(ops::Resolution::Skip), &ops::Cancel::new());
        undo(&Action::Transferred { mode: Mode::Copy, pairs: r.done }, &trash).unwrap();
        assert!(!p.join("dst/a.txt").exists());
        assert_eq!(trash.list().len(), 1);

        // Rename, then undo.
        let new = ops::rename(&p.join("src/a.txt"), "b.txt").unwrap();
        undo(&Action::Renamed(vec![(p.join("src/a.txt"), new)]), &trash).unwrap();
        assert!(p.join("src/a.txt").exists());

        // Trash, then undo.
        let item = trash.trash(&p.join("src/a.txt")).unwrap();
        undo(&Action::Trashed(vec![item]), &trash).unwrap();
        assert_eq!(fs::read(p.join("src/a.txt")).unwrap(), b"a");

        // Create, then undo.
        let f = ops::create_folder(p, "New Folder").unwrap();
        undo(&Action::Created(vec![f.clone()]), &trash).unwrap();
        assert!(!f.exists());
    }

    #[test]
    fn move_undo_keeps_both_if_original_name_reused() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        let trash = Trash::with_home(p.join("Trash"));
        fs::create_dir(p.join("dst")).unwrap();
        fs::write(p.join("a.txt"), "moved").unwrap();
        let r = ops::transfer(Mode::Move, &[p.join("a.txt")], &p.join("dst"), &mut ops::Fixed(ops::Resolution::Skip), &ops::Cancel::new());
        fs::write(p.join("a.txt"), "newer").unwrap();
        let back = undo(&Action::Transferred { mode: Mode::Move, pairs: r.done }, &trash).unwrap();
        assert_eq!(back, vec![p.join("a (2).txt")]);
        assert_eq!(fs::read(p.join("a.txt")).unwrap(), b"newer");
    }

    #[test]
    fn history_labels_and_limit() {
        let mut h = History::default();
        h.push(Action::Created(vec![]));
        assert!(h.peek().is_none(), "empty actions are ignored");
        for i in 0..150 {
            h.push(Action::Created(vec![PathBuf::from(format!("/{i}"))]));
        }
        assert_eq!(h.pop(), Some(Action::Created(vec![PathBuf::from("/149")])));
        assert_eq!(Action::Trashed(vec![]).label(), "Move to Trash of 0 Items");
        assert_eq!(
            Action::Transferred { mode: Mode::Move, pairs: vec![(PathBuf::new(), PathBuf::new())] }.label(),
            "Move"
        );
    }
}
