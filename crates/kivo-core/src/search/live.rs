// SPDX-License-Identifier: GPL-3.0-or-later
//! Live search: walk a folder in parallel and stream matches as they're
//! found. Used where the index doesn't reach (other drives, system folders,
//! before the first index is built) and for searching file contents within
//! a folder.

use std::fs;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use memchr::memmem;

use super::{Kind, Query, name_score, rank};
use crate::ops::Cancel;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveHit {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: i64,
    pub score: i32,
    /// Matched inside the file rather than in its name.
    pub in_content: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Names,
    /// Names, plus contents of text files.
    Contents,
}

/// Largest file whose contents are searched.
pub const MAX_CONTENT_BYTES: u64 = 8 * 1024 * 1024;

/// Search under `root`, calling `on_batch` with matches roughly every 100 ms.
/// `on_batch` returns false to stop. Stays on `root`'s file system.
pub fn search(root: &Path, q: &Query, mode: Mode, cancel: &Cancel, mut on_batch: impl FnMut(Vec<LiveHit>) -> bool) {
    let Ok(rm) = fs::metadata(root) else { return };
    let dev = rm.dev();
    let now = chrono::Local::now().timestamp();
    let since = q.modified.map(|d| d.since(now));
    let want_hidden = q.hidden || q.terms.iter().any(|t| t.starts_with('.'));
    let (tx, rx) = mpsc::channel::<LiveHit>();
    let terms: Vec<String> = q.terms.clone();
    let q2 = q.clone();
    let cancel2 = cancel.clone();
    let root2 = root.to_path_buf();
    let walker = std::thread::spawn(move || {
        let ctx = Ctx { q: &q2, terms: &terms, mode, since, want_hidden, dev, now, cancel: &cancel2 };
        rayon::scope(|s| visit(s, &ctx, root2, 0, false, tx));
    });
    let mut batch = Vec::new();
    let mut last = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(hit) => batch.push(hit),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if (!batch.is_empty() && last.elapsed() >= Duration::from_millis(100)) || batch.len() >= 2000 {
            if !on_batch(std::mem::take(&mut batch)) {
                cancel.cancel();
                break;
            }
            last = Instant::now();
        }
    }
    let _ = walker.join();
    // Drain anything sent after a stop request.
    for hit in rx.try_iter() {
        batch.push(hit);
    }
    if !batch.is_empty() && !cancel.is_cancelled() {
        on_batch(batch);
    }
}

struct Ctx<'a> {
    q: &'a Query,
    terms: &'a [String],
    mode: Mode,
    since: Option<i64>,
    want_hidden: bool,
    dev: u64,
    now: i64,
    cancel: &'a Cancel,
}

fn visit<'s>(s: &rayon::Scope<'s>, ctx: &'s Ctx<'s>, dir: PathBuf, depth: usize, hidden: bool, tx: mpsc::Sender<LiveHit>) {
    if ctx.cancel.is_cancelled() {
        return;
    }
    let Ok(rd) = fs::read_dir(&dir) else { return };
    for e in rd.flatten() {
        if ctx.cancel.is_cancelled() {
            return;
        }
        let name = e.file_name();
        let nb = name.as_bytes();
        let child_hidden = hidden || nb.first() == Some(&b'.');
        if child_hidden && !ctx.want_hidden {
            continue;
        }
        let p = e.path();
        let Ok(m) = fs::symlink_metadata(&p) else { continue };
        let is_dir = m.is_dir();
        let name_str = String::from_utf8_lossy(nb);
        let lower = name_str.to_lowercase();
        let kind = Kind::of(&name_str, is_dir);
        let filters_ok = ctx.q.accepts(kind, m.len(), m.mtime(), is_dir, ctx.since) && ctx.q.tags.is_empty();
        if filters_ok {
            let name_hit = ctx.terms.iter().filter(|t| !t.contains('/')).all(|t| lower.contains(t.as_str()));
            let content_hit = !name_hit
                && ctx.mode == Mode::Contents
                && m.is_file()
                && !ctx.terms.is_empty()
                && m.len() <= MAX_CONTENT_BYTES
                && contains_all(&p, ctx.terms);
            if name_hit || content_hit {
                let base = if name_hit { ctx.terms.first().map(|t| name_score(&lower, t)).unwrap_or(300) } else { 150 };
                let score = rank(base, lower.len(), depth, child_hidden, is_dir, m.mtime(), ctx.now);
                let _ = tx.send(LiveHit { path: p.clone(), is_dir, size: m.len(), mtime: m.mtime(), score, in_content: content_hit });
            }
        }
        if is_dir && m.dev() == ctx.dev && !super::index::SKIP_CONTENTS.contains(&name_str.as_ref()) {
            let tx = tx.clone();
            s.spawn(move |s| visit(s, ctx, p, depth + 1, child_hidden, tx));
        }
    }
}

/// Does the file's text contain every term (ASCII case-insensitive)?
/// Binary files (NUL in the first block) are skipped.
pub fn contains_all(path: &Path, terms: &[String]) -> bool {
    let Ok(mut f) = fs::File::open(path) else { return false };
    let mut data = Vec::new();
    if f.by_ref().take(MAX_CONTENT_BYTES).read_to_end(&mut data).is_err() {
        return false;
    }
    if data[..data.len().min(4096)].contains(&0) {
        return false;
    }
    data.make_ascii_lowercase();
    terms.iter().all(|t| memmem::find(&data, t.as_bytes()).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_name_and_content_matches() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        fs::create_dir_all(p.join("a/b/c")).unwrap();
        fs::create_dir_all(p.join(".hidden")).unwrap();
        fs::write(p.join("a/Invoice-March.pdf"), "x").unwrap();
        fs::write(p.join("a/b/c/notes.txt"), "Remember the INVOICE for March").unwrap();
        fs::write(p.join("a/b/binary.bin"), b"invoice\0\0\0").unwrap();
        fs::write(p.join(".hidden/invoice.txt"), "x").unwrap();
        let q = Query::parse("invoice", p);
        let mut names = Vec::new();
        search(p, &q, Mode::Names, &Cancel::new(), |b| {
            names.extend(b.into_iter().map(|h| h.path.file_name().unwrap().to_string_lossy().into_owned()));
            true
        });
        assert_eq!(names, ["Invoice-March.pdf"]);

        let mut hits = Vec::new();
        search(p, &q, Mode::Contents, &Cancel::new(), |b| {
            hits.extend(b);
            true
        });
        hits.sort_by(|a, b| a.path.cmp(&b.path));
        let found: Vec<(String, bool)> = hits.iter().map(|h| (h.path.file_name().unwrap().to_string_lossy().into_owned(), h.in_content)).collect();
        assert_eq!(found, [("Invoice-March.pdf".to_string(), false), ("notes.txt".to_string(), true)]);
    }

    #[test]
    fn stops_when_asked() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..300 {
            fs::write(d.path().join(format!("match-{i}")), "").unwrap();
        }
        let mut calls = 0;
        search(d.path(), &Query::parse("match", d.path()), Mode::Names, &Cancel::new(), |_| {
            calls += 1;
            false
        });
        assert_eq!(calls, 1);
    }
}
