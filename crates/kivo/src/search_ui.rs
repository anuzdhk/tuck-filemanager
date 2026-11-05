// SPDX-License-Identifier: GPL-3.0-or-later
//! Search for the UI: a background indexer that keeps the name and content
//! indexes fresh at idle priority, and query helpers that run off the UI
//! thread and hand back ready-to-show results.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::Duration;

use kivo_core::listing::RawEntry;
use kivo_core::ops::Cancel;
use kivo_core::paths;
use kivo_core::search::content::ContentIndex;
use kivo_core::search::index::{self, Index};
use kivo_core::search::live::{self, Mode};
use kivo_core::search::Query;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Starting = 0,
    Indexing = 1,
    Ready = 2,
}

struct Shared {
    names: Mutex<Option<Arc<Index>>>,
    content: OnceLock<ContentIndex>,
    state: AtomicU8,
    entries: AtomicU64,
    nudge: Mutex<Option<mpsc::Sender<()>>>,
}

fn shared() -> &'static Shared {
    static S: OnceLock<Shared> = OnceLock::new();
    S.get_or_init(|| Shared {
        names: Mutex::new(None),
        content: OnceLock::new(),
        state: AtomicU8::new(State::Starting as u8),
        entries: AtomicU64::new(0),
        nudge: Mutex::new(None),
    })
}

/// Indexed folders: home, or `KIVO_INDEX_ROOTS` (colon-separated) for tests.
pub fn roots() -> Vec<PathBuf> {
    match std::env::var("KIVO_INDEX_ROOTS") {
        Ok(v) if !v.is_empty() => v.split(':').map(PathBuf::from).collect(),
        _ => vec![paths::home()],
    }
}

fn cache_dir() -> PathBuf {
    paths::cache_home().join("kivo")
}

/// Start the indexer (once). Loads the saved index right away, then
/// refreshes it in the background and every few minutes after.
pub fn start() {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() || std::env::var_os("KIVO_NO_INDEX").is_some() {
        return;
    }
    let (tx, rx) = mpsc::channel::<()>();
    *shared().nudge.lock().expect("lock") = Some(tx);
    std::thread::Builder::new()
        .name("kivo-indexer".into())
        .spawn(move || {
            index::be_nice();
            let s = shared();
            let file = cache_dir().join("names.idx");
            if let Ok(idx) = Index::load(&file) {
                s.entries.store(idx.len() as u64, Ordering::Relaxed);
                *s.names.lock().expect("lock") = Some(Arc::new(idx));
                s.state.store(State::Ready as u8, Ordering::Relaxed);
            }
            if let Ok(ci) = ContentIndex::open(&cache_dir().join("content")) {
                let _ = s.content.set(ci);
            }
            loop {
                if s.state.load(Ordering::Relaxed) != State::Ready as u8 {
                    s.state.store(State::Indexing as u8, Ordering::Relaxed);
                }
                let old = s.names.lock().expect("lock").clone();
                let (fresh, _) = index::crawl(&roots(), old.as_deref(), &Cancel::new());
                let fresh = Arc::new(fresh);
                s.entries.store(fresh.len() as u64, Ordering::Relaxed);
                *s.names.lock().expect("lock") = Some(fresh.clone());
                s.state.store(State::Ready as u8, Ordering::Relaxed);
                let _ = fresh.save(&file);
                if let Some(ci) = s.content.get() {
                    let _ = ci.update(&fresh, &Cancel::new());
                }
                // Refresh every five minutes, or soon after a nudge.
                match rx.recv_timeout(Duration::from_secs(300)) {
                    Ok(()) => {
                        std::thread::sleep(Duration::from_secs(2));
                        while rx.try_recv().is_ok() {}
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        })
        .expect("spawn indexer");
}

/// Something changed on disk (copy, move, rename…): refresh soon.
pub fn nudge() {
    if let Some(tx) = shared().nudge.lock().expect("lock").as_ref() {
        let _ = tx.send(());
    }
}

pub fn state() -> (State, u64) {
    let s = shared();
    let st = match s.state.load(Ordering::Relaxed) {
        2 => State::Ready,
        1 => State::Indexing,
        _ => State::Starting,
    };
    (st, s.entries.load(Ordering::Relaxed))
}

/// One result, checked to still exist.
pub struct Found {
    pub entry: RawEntry,
    pub parent: PathBuf,
    pub score: i32,
    pub in_content: bool,
}

fn found(path: &Path, score: i32, in_content: bool) -> Option<Found> {
    let parent = path.parent()?.to_path_buf();
    let entry = RawEntry::from_path(path, path.file_name()?.to_owned()).ok()?;
    Some(Found { entry, parent, score, in_content })
}

/// Does the index cover `dir`? (Inside an indexed root, and indexed.)
pub fn covers(dir: &Path) -> bool {
    let idx = shared().names.lock().expect("lock").clone();
    idx.is_some_and(|i| roots().iter().any(|r| dir.starts_with(r)) && i.find(dir).is_some())
}

pub struct Outcome {
    pub results: Vec<Found>,
    pub total: usize,
    pub micros: u128,
}

/// Indexed search: names, plus file contents when asked. Blocking (worker).
pub fn indexed(q: &Query, contents: bool, limit: usize) -> Option<Outcome> {
    let idx = shared().names.lock().expect("lock").clone()?;
    let start = std::time::Instant::now();
    let now = chrono::Local::now().timestamp();
    let r = idx.search(q, limit, now);
    let mut results: Vec<Found> = r.hits.iter().filter_map(|h| found(&idx.path(h.id), h.score, false)).collect();
    let mut total = r.total;
    if contents && !q.terms.is_empty()
        && let Some(ci) = shared().content.get() {
            let have: std::collections::HashSet<PathBuf> = results.iter().map(|f| f.parent.join(&f.entry.name)).collect();
            let since = q.modified.map(|d| d.since(now));
            for (p, score) in ci.search(&q.terms, limit) {
                if have.contains(&p) {
                    continue;
                }
                if let Some(loc) = &q.location
                    && !p.starts_with(loc) {
                        continue;
                    }
                // Apply the same filters the name search uses.
                if let Some(id) = idx.find(&p)
                    && !q.accepts(idx.kind(id), idx.size(id), idx.mtime(id), false, since) {
                        continue;
                    }
                if let Some(f) = found(&p, 100 + score as i32, true) {
                    results.push(f);
                    total += 1;
                }
            }
        }
    Some(Outcome { results, total, micros: start.elapsed().as_micros() })
}

/// Live search under `dir`, streaming batches to `on_batch` (worker thread).
pub fn walk(dir: &Path, q: &Query, contents: bool, cancel: &Cancel, mut on_batch: impl FnMut(Vec<Found>) -> bool) {
    let mode = if contents { Mode::Contents } else { Mode::Names };
    live::search(dir, q, mode, cancel, |hits| {
        let batch: Vec<Found> = hits.into_iter().filter_map(|h| found(&h.path, h.score, h.in_content)).collect();
        on_batch(batch)
    });
}
