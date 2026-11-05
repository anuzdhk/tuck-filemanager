// SPDX-License-Identifier: GPL-3.0-or-later
//! The name index.
//!
//! Layout is struct-of-arrays: every lowercased name sits in one contiguous
//! byte buffer, so a query is a parallel `memmem` scan over ~20–40 MB for a
//! million files, with ranking only for the matches. The index is immutable;
//! a refresh builds a new one in the background and the UI swaps it in.
//!
//! Refreshing is incremental: a folder whose modification time hasn't
//! changed keeps its file list from the old index without being read again,
//! so a refresh costs one `stat` per folder rather than one per file.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, BufWriter, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Instant;

use memchr::memmem;
use rayon::prelude::*;

use super::{Kind, Query, name_score, rank};
use crate::ops::Cancel;

const NONE: u32 = u32::MAX;
const F_DIR: u8 = 1;
const F_HIDDEN: u8 = 2; // the item or any ancestor is hidden
const MAGIC: &[u8; 8] = b"KIVOIDX1";

#[derive(Default)]
pub struct Index {
    names: Vec<u8>,
    name_off: Vec<u32>,
    lower: Vec<u8>,
    lower_off: Vec<u32>,
    parent: Vec<u32>,
    size: Vec<u64>,
    mtime: Vec<i64>,
    kind: Vec<u8>,
    flags: Vec<u8>,
    depth: Vec<u16>,
    /// Sparse: (entry, comma-separated lowercase tags), sorted by entry.
    tags: Vec<(u32, String)>,
    pub built_at: i64,
    children: OnceLock<(Vec<u32>, Vec<u32>)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub id: u32,
    pub score: i32,
}

#[derive(Debug, Clone, Default)]
pub struct Results {
    pub hits: Vec<Hit>,
    /// Matches before the limit was applied.
    pub total: usize,
    pub micros: u128,
}

impl Index {
    pub fn len(&self) -> usize {
        self.parent.len()
    }

    pub fn is_empty(&self) -> bool {
        self.parent.is_empty()
    }

    fn name_bytes(&self, i: u32) -> &[u8] {
        &self.names[self.name_off[i as usize] as usize..self.name_off[i as usize + 1] as usize]
    }

    fn lower_bytes(&self, i: u32) -> &[u8] {
        &self.lower[self.lower_off[i as usize] as usize..self.lower_off[i as usize + 1] as usize]
    }

    pub fn name(&self, i: u32) -> String {
        String::from_utf8_lossy(self.name_bytes(i)).into_owned()
    }

    pub fn is_dir(&self, i: u32) -> bool {
        self.flags[i as usize] & F_DIR != 0
    }

    /// Hidden, or inside a hidden folder (relative to the indexed root).
    pub fn is_hidden(&self, i: u32) -> bool {
        self.flags[i as usize] & F_HIDDEN != 0
    }

    pub fn size(&self, i: u32) -> u64 {
        self.size[i as usize]
    }

    pub fn mtime(&self, i: u32) -> i64 {
        self.mtime[i as usize]
    }

    pub fn kind(&self, i: u32) -> Kind {
        Kind::from_u8(self.kind[i as usize])
    }

    /// Full path. Root entries store their absolute path as their name.
    pub fn path(&self, i: u32) -> PathBuf {
        let mut parts: Vec<&[u8]> = Vec::with_capacity(self.depth[i as usize] as usize + 1);
        let mut cur = i;
        while cur != NONE {
            parts.push(self.name_bytes(cur));
            cur = self.parent[cur as usize];
        }
        let mut p = PathBuf::new();
        for part in parts.iter().rev() {
            p.push(std::ffi::OsStr::from_bytes(part));
        }
        p
    }

    /// Children in compressed-sparse-row form, built on first use.
    fn children(&self) -> &(Vec<u32>, Vec<u32>) {
        self.children.get_or_init(|| {
            let n = self.len();
            let mut count = vec![0u32; n + 1];
            for &p in &self.parent {
                if p != NONE {
                    count[p as usize + 1] += 1;
                }
            }
            for i in 1..=n {
                count[i] += count[i - 1];
            }
            let mut start = count.clone();
            let mut list = vec![0u32; count[n] as usize];
            for (i, &p) in self.parent.iter().enumerate() {
                if p != NONE {
                    list[start[p as usize] as usize] = i as u32;
                    start[p as usize] += 1;
                }
            }
            (count, list)
        })
    }

    fn children_of(&self, i: u32) -> &[u32] {
        let (start, list) = self.children();
        &list[start[i as usize] as usize..start[i as usize + 1] as usize]
    }

    /// Entry for an absolute path, if indexed.
    /// Every tag in use, sorted, each once.
    pub fn all_tags(&self) -> Vec<String> {
        let mut all: Vec<String> = self.tags.iter().flat_map(|(_, t)| t.split(',')).filter(|t| !t.is_empty()).map(String::from).collect();
        all.sort();
        all.dedup();
        all
    }

    pub fn find(&self, path: &Path) -> Option<u32> {
        let roots: Vec<u32> = (0..self.len() as u32).filter(|&i| self.parent[i as usize] == NONE).collect();
        for r in roots {
            let root_path = PathBuf::from(std::ffi::OsStr::from_bytes(self.name_bytes(r)));
            let Ok(rest) = path.strip_prefix(&root_path) else { continue };
            let mut cur = r;
            let mut ok = true;
            for comp in rest.components() {
                let want = comp.as_os_str().as_bytes();
                match self.children_of(cur).iter().find(|&&c| self.name_bytes(c) == want) {
                    Some(&c) => cur = c,
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                return Some(cur);
            }
        }
        None
    }

    /// Run a query. Returns the best `limit` hits, best first.
    pub fn search(&self, q: &Query, limit: usize, now: i64) -> Results {
        let started = Instant::now();
        let name_terms: Vec<&String> = q.terms.iter().filter(|t| !t.contains('/')).collect();
        let path_terms: Vec<&String> = q.terms.iter().filter(|t| t.contains('/')).collect();
        let finders: Vec<memmem::Finder> = name_terms.iter().map(|t| memmem::Finder::new(t.as_bytes())).collect();
        let since = q.modified.map(|d| d.since(now));
        let want_hidden = q.hidden || q.terms.iter().any(|t| t.starts_with('.'));
        let location = match &q.location {
            Some(loc) => match self.find(loc) {
                Some(l) => Some(l),
                None => return Results { micros: started.elapsed().as_micros(), ..Default::default() },
            },
            None => None,
        };
        // Per-query memo for "is inside the location?": 0 unknown, 1 yes, 2 no.
        let memo: Option<Vec<AtomicU8>> = location.map(|_| (0..self.len()).map(|_| AtomicU8::new(0)).collect());
        let inside = |i: u32| -> bool {
            let (Some(loc), Some(memo)) = (location, memo.as_ref()) else { return true };
            let mut chain = Vec::new();
            let mut cur = self.parent[i as usize];
            let ans = loop {
                if cur == loc {
                    break true;
                }
                if cur == NONE {
                    break false;
                }
                match memo[cur as usize].load(Ordering::Relaxed) {
                    1 => break true,
                    2 => break false,
                    _ => {}
                }
                chain.push(cur);
                cur = self.parent[cur as usize];
            };
            for c in chain {
                memo[c as usize].store(if ans { 1 } else { 2 }, Ordering::Relaxed);
            }
            ans
        };
        let tagged = |i: u32| -> bool {
            if q.tags.is_empty() {
                return true;
            }
            match self.tags.binary_search_by_key(&i, |(id, _)| *id) {
                Ok(pos) => {
                    let have: Vec<&str> = self.tags[pos].1.split(',').collect();
                    q.tags.iter().all(|t| have.contains(&t.as_str()))
                }
                Err(_) => false,
            }
        };

        let mut matches: Vec<Hit> = (0..self.len() as u32)
            .into_par_iter()
            .with_min_len(8192)
            .filter_map(|i| {
                let lower = self.lower_bytes(i);
                if !finders.iter().all(|f| f.find(lower).is_some()) {
                    return None;
                }
                let flags = self.flags[i as usize];
                if flags & F_HIDDEN != 0 && !want_hidden {
                    return None;
                }
                let is_dir = flags & F_DIR != 0;
                let (size, mtime) = (self.size[i as usize], self.mtime[i as usize]);
                if !q.accepts(Kind::from_u8(self.kind[i as usize]), size, mtime, is_dir, since) {
                    return None;
                }
                if !inside(i) || !tagged(i) {
                    return None;
                }
                if !path_terms.is_empty() {
                    let p = self.path(i).to_string_lossy().to_lowercase();
                    if !path_terms.iter().all(|t| p.contains(t.as_str())) {
                        return None;
                    }
                }
                let name = std::str::from_utf8(lower).unwrap_or("");
                let base = name_terms.first().map(|t| name_score(name, t)).unwrap_or(300);
                let score = rank(base, name.len(), self.depth[i as usize] as usize, flags & F_HIDDEN != 0, is_dir, mtime, now);
                Some(Hit { id: i, score })
            })
            .collect();
        let total = matches.len();
        if matches.len() > limit && limit > 0 {
            matches.select_nth_unstable_by(limit - 1, |a, b| b.score.cmp(&a.score).then(a.id.cmp(&b.id)));
            matches.truncate(limit);
        }
        matches.sort_unstable_by(|a, b| b.score.cmp(&a.score).then(a.id.cmp(&b.id)));
        Results { hits: matches, total, micros: started.elapsed().as_micros() }
    }

    // ------------------------------------------------------------ storage

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(d) = path.parent() {
            fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("tmp");
        {
            let mut w = BufWriter::with_capacity(1 << 20, fs::File::create(&tmp)?);
            w.write_all(MAGIC)?;
            w.write_all(&self.built_at.to_le_bytes())?;
            write_bytes(&mut w, &self.names)?;
            write_u32s(&mut w, &self.name_off)?;
            write_bytes(&mut w, &self.lower)?;
            write_u32s(&mut w, &self.lower_off)?;
            write_u32s(&mut w, &self.parent)?;
            write_u64s(&mut w, &self.size)?;
            write_u64s(&mut w, &self.mtime.iter().map(|&m| m as u64).collect::<Vec<_>>())?;
            write_bytes(&mut w, &self.kind)?;
            write_bytes(&mut w, &self.flags)?;
            write_u32s(&mut w, &self.depth.iter().map(|&d| d as u32).collect::<Vec<_>>())?;
            let tag_ids: Vec<u32> = self.tags.iter().map(|(i, _)| *i).collect();
            write_u32s(&mut w, &tag_ids)?;
            let tag_text: String = self.tags.iter().map(|(_, t)| format!("{t}\n")).collect();
            write_bytes(&mut w, tag_text.as_bytes())?;
            w.flush()?;
        }
        fs::rename(tmp, path)
    }

    pub fn load(path: &Path) -> io::Result<Index> {
        let mut data = Vec::new();
        fs::File::open(path)?.read_to_end(&mut data)?;
        let mut r = Reader { data: &data, pos: 0 };
        if r.take(8)? != MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "not a Kivo index"));
        }
        let built_at = i64::from_le_bytes(r.take(8)?.try_into().expect("8 bytes"));
        let names = r.bytes()?;
        let name_off = r.u32s()?;
        let lower = r.bytes()?;
        let lower_off = r.u32s()?;
        let parent = r.u32s()?;
        let size = r.u64s()?;
        let mtime = r.u64s()?.into_iter().map(|m| m as i64).collect();
        let kind = r.bytes()?;
        let flags = r.bytes()?;
        let depth = r.u32s()?.into_iter().map(|d| d as u16).collect();
        let tag_ids = r.u32s()?;
        let tag_text = String::from_utf8(r.bytes()?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let tags = tag_ids.into_iter().zip(tag_text.lines().map(String::from)).collect();
        let idx = Index { names, name_off, lower, lower_off, parent, size, mtime, kind, flags, depth, tags, built_at, children: OnceLock::new() };
        let n = idx.parent.len();
        let consistent = idx.name_off.len() == n + 1
            && idx.lower_off.len() == n + 1
            && [idx.size.len(), idx.mtime.len(), idx.kind.len(), idx.flags.len(), idx.depth.len()].iter().all(|&l| l == n)
            && idx.name_off.last().is_none_or(|&o| o as usize == idx.names.len())
            && idx.lower_off.last().is_none_or(|&o| o as usize == idx.lower.len())
            && idx.parent.iter().all(|&p| p == NONE || (p as usize) < n);
        if !consistent {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "index file is inconsistent"));
        }
        Ok(idx)
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> io::Result<&[u8]> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.data.len()).ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "truncated index"))?;
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn len(&mut self) -> io::Result<usize> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().expect("8 bytes")) as usize)
    }
    fn bytes(&mut self) -> io::Result<Vec<u8>> {
        let n = self.len()?;
        Ok(self.take(n)?.to_vec())
    }
    fn u32s(&mut self) -> io::Result<Vec<u32>> {
        let n = self.len()?;
        let b = self.take(n.checked_mul(4).ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?)?;
        Ok(b.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).collect())
    }
    fn u64s(&mut self) -> io::Result<Vec<u64>> {
        let n = self.len()?;
        let b = self.take(n.checked_mul(8).ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?)?;
        Ok(b.as_chunks::<8>().0.iter().map(|c| u64::from_le_bytes(*c)).collect())
    }
}

fn write_bytes(w: &mut impl Write, b: &[u8]) -> io::Result<()> {
    w.write_all(&(b.len() as u64).to_le_bytes())?;
    w.write_all(b)
}

fn write_u32s(w: &mut impl Write, v: &[u32]) -> io::Result<()> {
    w.write_all(&(v.len() as u64).to_le_bytes())?;
    let mut buf = Vec::with_capacity(v.len() * 4);
    for x in v {
        buf.extend_from_slice(&x.to_le_bytes());
    }
    w.write_all(&buf)
}

fn write_u64s(w: &mut impl Write, v: &[u64]) -> io::Result<()> {
    w.write_all(&(v.len() as u64).to_le_bytes())?;
    let mut buf = Vec::with_capacity(v.len() * 8);
    for x in v {
        buf.extend_from_slice(&x.to_le_bytes());
    }
    w.write_all(&buf)
}

// ------------------------------------------------------------------ building

/// Folders whose contents aren't indexed (the folder itself still is).
pub const SKIP_CONTENTS: &[&str] = &[
    ".git", ".hg", ".svn", "node_modules", "__pycache__", ".cache", ".ccache", ".npm", ".yarn", ".gradle",
    ".m2", ".rustup", ".cargo", ".venv", "venv", ".tox", ".mypy_cache", ".pytest_cache", ".Trash", "Trash",
    ".thumbnails", ".local/share/Trash", ".steam", ".wine",
];

pub struct Builder {
    idx: Index,
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

impl Builder {
    pub fn new() -> Self {
        let mut idx = Index::default();
        idx.name_off.push(0);
        idx.lower_off.push(0);
        Builder { idx }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn push(&mut self, parent: u32, name: &[u8], is_dir: bool, size: u64, mtime: i64, hidden: bool, depth: u16, tags: Option<String>) -> u32 {
        let i = &mut self.idx;
        let id = i.parent.len() as u32;
        i.names.extend_from_slice(name);
        i.name_off.push(i.names.len() as u32);
        let s = String::from_utf8_lossy(name);
        let lower = if s.is_ascii() { s.to_ascii_lowercase() } else { s.to_lowercase() };
        i.lower.extend_from_slice(lower.as_bytes());
        i.lower_off.push(i.lower.len() as u32);
        i.parent.push(parent);
        i.size.push(size);
        i.mtime.push(mtime);
        i.kind.push(Kind::of(&s, is_dir) as u8);
        i.flags.push((if is_dir { F_DIR } else { 0 }) | (if hidden { F_HIDDEN } else { 0 }));
        i.depth.push(depth);
        if let Some(t) = tags.filter(|t| !t.is_empty()) {
            i.tags.push((id, t));
        }
        id
    }

    pub fn finish(mut self) -> Index {
        self.idx.tags.sort_by_key(|(i, _)| *i);
        self.idx.built_at = chrono::Local::now().timestamp();
        self.idx
    }
}

/// Dolphin/Baloo-compatible tags (`user.xdg.tags`), lowercased.
pub fn read_tags(path: &Path) -> Option<String> {
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let name = c"user.xdg.tags";
    let mut buf = [0u8; 1024];
    // SAFETY: valid C strings and a buffer of the stated size.
    let n = unsafe { libc::lgetxattr(c.as_ptr(), name.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if n <= 0 {
        return None;
    }
    let s = String::from_utf8_lossy(&buf[..n as usize]).to_lowercase();
    Some(s.split(',').map(str::trim).filter(|t| !t.is_empty()).collect::<Vec<_>>().join(","))
}

pub struct CrawlStats {
    pub dirs_read: u64,
    pub dirs_reused: u64,
    pub entries: u64,
}

/// Build an index of `roots`, staying on each root's file system. With `old`,
/// folders whose mtime hasn't changed reuse their old file list.
pub fn crawl(roots: &[PathBuf], old: Option<&Index>, cancel: &Cancel) -> (Index, CrawlStats) {
    crawl_with(roots, old, &HashSet::new(), cancel)
}

/// Like [`crawl`], but folders in `force` are always read again — for
/// changes that don't touch a folder's mtime, such as new tags on a file.
pub fn crawl_with(roots: &[PathBuf], old: Option<&Index>, force: &HashSet<PathBuf>, cancel: &Cancel) -> (Index, CrawlStats) {
    let mut b = Builder::new();
    let mut stats = CrawlStats { dirs_read: 0, dirs_reused: 0, entries: 0 };
    for root in roots {
        let Ok(m) = fs::metadata(root) else { continue };
        if !m.is_dir() {
            continue;
        }
        let id = b.push(NONE, root.as_os_str().as_bytes(), true, 0, m.mtime(), false, 0, None);
        let old_id = old.and_then(|o| o.find(root));
        walk(&mut b, old, force, root, id, old_id, m.mtime(), m.dev(), 1, false, &mut stats, cancel);
    }
    stats.entries = b.idx.len() as u64;
    (b.finish(), stats)
}

#[allow(clippy::too_many_arguments)]
fn walk(b: &mut Builder, old: Option<&Index>, force: &HashSet<PathBuf>, dir: &Path, id: u32, old_id: Option<u32>, dir_mtime: i64, dev: u64, depth: u16, hidden: bool, stats: &mut CrawlStats, cancel: &Cancel) {
    if cancel.is_cancelled() {
        return;
    }
    // Unchanged folder: reuse its listing; only subfolders are stat-ed.
    if let (Some(o), Some(oid)) = (old, old_id)
        && o.mtime[oid as usize] == dir_mtime
        && o.is_dir(oid)
        && !force.contains(dir)
    {
        {
            stats.dirs_reused += 1;
            for &c in o.children_of(oid) {
                let name = o.name_bytes(c).to_vec();
                let child_hidden = hidden || name.first() == Some(&b'.');
                if o.is_dir(c) {
                    let p = dir.join(std::ffi::OsStr::from_bytes(&name));
                    let Ok(m) = fs::symlink_metadata(&p) else { continue };
                    if !m.is_dir() {
                        continue;
                    }
                    let tags = o.tags.binary_search_by_key(&c, |(i, _)| *i).ok().map(|pos| o.tags[pos].1.clone());
                    let cid = b.push(id, &name, true, 0, m.mtime(), child_hidden, depth, tags);
                    if m.dev() == dev && !skip_contents(&p, &name) {
                        walk(b, old, force, &p, cid, Some(c), m.mtime(), dev, depth + 1, child_hidden, stats, cancel);
                    }
                } else {
                    let tags = o.tags.binary_search_by_key(&c, |(i, _)| *i).ok().map(|pos| o.tags[pos].1.clone());
                    b.push(id, &name, false, o.size[c as usize], o.mtime[c as usize], child_hidden, depth, tags);
                }
            }
            return;
        }
    }
    stats.dirs_read += 1;
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut subdirs = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name();
        let nb = name.as_bytes();
        let p = e.path();
        let Ok(m) = fs::symlink_metadata(&p) else { continue };
        let child_hidden = hidden || nb.first() == Some(&b'.');
        let tags = read_tags(&p);
        if m.is_dir() {
            let cid = b.push(id, nb, true, 0, m.mtime(), child_hidden, depth, tags);
            if m.dev() == dev && !skip_contents(&p, nb) {
                subdirs.push((p, cid, m.mtime()));
            }
        } else {
            b.push(id, nb, false, m.len(), m.mtime(), child_hidden, depth, tags);
        }
    }
    // Old children by name, built once per folder (not one scan per subfolder).
    let old_kids: HashMap<&[u8], u32> = match (old, old_id) {
        (Some(o), Some(oid)) => o.children_of(oid).iter().map(|&c| (o.name_bytes(c), c)).collect(),
        _ => HashMap::new(),
    };
    for (p, cid, mt) in subdirs {
        let old_child = p.file_name().and_then(|n| old_kids.get(n.as_bytes()).copied());
        walk(b, old, force, &p, cid, old_child, mt, dev, depth + 1, hidden || p.file_name().is_some_and(|n| n.as_bytes().first() == Some(&b'.')), stats, cancel);
    }
}

fn skip_contents(p: &Path, name: &[u8]) -> bool {
    let n = std::str::from_utf8(name).unwrap_or("");
    if SKIP_CONTENTS.contains(&n) {
        return true;
    }
    let s = p.to_string_lossy();
    if SKIP_CONTENTS.iter().any(|x| x.contains('/') && s.ends_with(x)) {
        return true;
    }
    // Build caches mark themselves (cargo's target/, many tools).
    p.join("CACHEDIR.TAG").exists()
}

/// Lower this thread's CPU and I/O priority (indexing must not slow the system).
pub fn be_nice() {
    // SAFETY: plain syscalls on the calling thread.
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS, tid, 19);
        // IOPRIO_CLASS_IDLE (3) << IOPRIO_CLASS_SHIFT (13); IOPRIO_WHO_PROCESS = 1.
        libc::syscall(libc::SYS_ioprio_set, 1, tid as libc::c_long, 3 << 13);
    }
}

/// Index of names for an old snapshot, keyed by path (for diagnostics/tests).
pub fn paths_of(idx: &Index) -> HashMap<PathBuf, u32> {
    (0..idx.len() as u32).map(|i| (idx.path(i), i)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        for dir in ["Documents/Reports", "Pictures/Holiday", "code/proj/src", "code/proj/node_modules/lib", ".config/app", "code/proj/target"] {
            fs::create_dir_all(p.join(dir)).unwrap();
        }
        fs::write(p.join("code/proj/target/CACHEDIR.TAG"), "Signature: 8a477f597d28d172789f06886806bc55").unwrap();
        fs::write(p.join("code/proj/target/build.o"), "x").unwrap();
        for f in [
            "Documents/Reports/report.pdf",
            "Documents/Reports/annual-report-2024.pdf",
            "Documents/myreport.txt",
            "Pictures/Holiday/beach.jpg",
            "Pictures/Holiday/Report Photo.png",
            "code/proj/src/main.rs",
            "code/proj/node_modules/lib/report.js",
            ".config/app/report.conf",
        ] {
            fs::write(p.join(f), vec![b'x'; 10]).unwrap();
        }
        fs::write(p.join("Documents/big.iso"), vec![0u8; 5000]).unwrap();
        d
    }

    fn names(idx: &Index, r: &Results) -> Vec<String> {
        r.hits.iter().map(|h| idx.name(h.id)).collect()
    }

    #[test]
    fn builds_and_ranks() {
        let d = fixture();
        let (idx, stats) = crawl(&[d.path().to_path_buf()], None, &Cancel::new());
        assert!(stats.entries > 10);
        let now = chrono::Local::now().timestamp();
        let r = idx.search(&Query::parse("report", d.path()), 50, now);
        let n = names(&idx, &r);
        assert_eq!(n[0], "report.pdf", "{n:?}");
        assert!(n.contains(&"annual-report-2024.pdf".to_string()));
        assert!(n.contains(&"myreport.txt".to_string()));
        assert!(n.contains(&"Report Photo.png".to_string()), "case-insensitive");
        assert!(!n.contains(&"report.js".to_string()), "node_modules contents skipped");
        assert!(!n.contains(&"report.conf".to_string()), "hidden excluded by default");
        let pos = |s: &str| n.iter().position(|x| x == s).unwrap();
        assert!(pos("annual-report-2024.pdf") < pos("myreport.txt"), "word start beats mid-word");
        let r = idx.search(&Query::parse("report hidden:yes", d.path()), 50, now);
        assert!(names(&idx, &r).contains(&"report.conf".to_string()));
        let r = idx.search(&Query::parse("build", d.path()), 50, now);
        assert!(r.hits.is_empty(), "CACHEDIR.TAG folders skipped");
    }

    #[test]
    fn filters_location_kind_size() {
        let d = fixture();
        let (idx, _) = crawl(&[d.path().to_path_buf()], None, &Cancel::new());
        let now = chrono::Local::now().timestamp();
        let q = Query::parse(&format!("report in:{}", d.path().join("Pictures").display()), d.path());
        assert_eq!(names(&idx, &idx.search(&q, 50, now)), ["Report Photo.png"]);
        let q = Query::parse("kind:pdf", d.path());
        assert_eq!(idx.search(&q, 50, now).hits.len(), 2);
        let q = Query::parse("size:>1k", d.path());
        assert_eq!(names(&idx, &idx.search(&q, 50, now)), ["big.iso"]);
        let q = Query::parse("holiday/", d.path());
        let r = names(&idx, &idx.search(&q, 50, now));
        assert!(r.contains(&"beach.jpg".to_string()), "path terms: {r:?}");
        let q = Query::parse(&format!("x in:{}", d.path().join("nope").display()), d.path());
        assert!(idx.search(&q, 50, now).hits.is_empty());
        let only = idx.search(&Query::parse("report", d.path()), 2, now);
        assert_eq!((only.hits.len(), only.total > 2), (2, true));
    }

    #[test]
    fn paths_roundtrip_and_find() {
        let d = fixture();
        let (idx, _) = crawl(&[d.path().to_path_buf()], None, &Cancel::new());
        let target = d.path().join("code/proj/src/main.rs");
        let id = idx.find(&target).unwrap();
        assert_eq!(idx.path(id), target);
        assert_eq!(idx.name(id), "main.rs");
        assert_eq!(idx.kind(id), Kind::Code);
    }

    #[test]
    fn save_load_roundtrip_and_corruption() {
        let d = fixture();
        let (idx, _) = crawl(&[d.path().to_path_buf()], None, &Cancel::new());
        let f = d.path().join("idx.bin");
        idx.save(&f).unwrap();
        let back = Index::load(&f).unwrap();
        assert_eq!(back.len(), idx.len());
        let now = chrono::Local::now().timestamp();
        let q = Query::parse("report", d.path());
        assert_eq!(names(&back, &back.search(&q, 50, now)), names(&idx, &idx.search(&q, 50, now)));
        let bytes = fs::read(&f).unwrap();
        fs::write(&f, &bytes[..bytes.len() / 2]).unwrap();
        assert!(Index::load(&f).is_err(), "truncated file rejected, not trusted");
        fs::write(&f, b"garbage").unwrap();
        assert!(Index::load(&f).is_err());
    }

    #[test]
    fn incremental_refresh_reuses_unchanged_folders() {
        let d = fixture();
        let (idx, first) = crawl(&[d.path().to_path_buf()], None, &Cancel::new());
        let (same, second) = crawl(&[d.path().to_path_buf()], Some(&idx), &Cancel::new());
        assert_eq!(same.len(), idx.len());
        assert_eq!(second.dirs_read, 0, "nothing changed, nothing re-read");
        assert!(second.dirs_reused >= first.dirs_read);
        // Change one folder.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fs::write(d.path().join("Documents/new-report.md"), "x").unwrap();
        fs::remove_file(d.path().join("Documents/myreport.txt")).unwrap();
        let (fresh, third) = crawl(&[d.path().to_path_buf()], Some(&same), &Cancel::new());
        // A forced folder is read again even though its mtime is unchanged.
        let force: HashSet<PathBuf> = [d.path().to_path_buf()].into();
        let (_, forced) = crawl_with(&[d.path().to_path_buf()], Some(&fresh), &force, &Cancel::new());
        assert!(forced.dirs_read >= 1);
        assert_eq!(third.dirs_read, 1, "only Documents re-read");
        let now = chrono::Local::now().timestamp();
        let n = names(&fresh, &fresh.search(&Query::parse("report", d.path()), 50, now));
        assert!(n.contains(&"new-report.md".to_string()));
        assert!(!n.contains(&"myreport.txt".to_string()));
    }

    #[test]
    fn tags_are_searchable() {
        let d = fixture();
        let f = d.path().join("Documents/myreport.txt");
        let c = std::ffi::CString::new(f.as_os_str().as_bytes()).unwrap();
        let v = b"Work,Urgent";
        // SAFETY: valid strings and buffer.
        let r = unsafe { libc::setxattr(c.as_ptr(), c"user.xdg.tags".as_ptr(), v.as_ptr().cast(), v.len(), 0) };
        if r != 0 {
            eprintln!("xattrs unsupported here; skipping");
            return;
        }
        let (idx, _) = crawl(&[d.path().to_path_buf()], None, &Cancel::new());
        let now = chrono::Local::now().timestamp();
        assert_eq!(names(&idx, &idx.search(&Query::parse("tag:work", d.path()), 50, now)), ["myreport.txt"]);
        assert!(idx.search(&Query::parse("tag:work tag:missing", d.path()), 50, now).hits.is_empty());
    }
}
