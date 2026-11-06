// SPDX-License-Identifier: GPL-3.0-or-later
//! Full-text content index (tantivy), for "search file contents" everywhere.
//!
//! Text and source files up to 1 MB are indexed, using the name index as the
//! list of candidates. Updates are incremental: a small state file remembers
//! each indexed file's modification time, so unchanged files are skipped and
//! deleted ones are dropped.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Field, STORED, STRING, Schema, TEXT, Value};
use tantivy::{Index as TIndex, IndexReader, ReloadPolicy, TantivyDocument, Term, doc};

use super::Kind;
use super::index::Index;
use crate::ops::Cancel;

pub const MAX_BYTES: u64 = 1024 * 1024;

pub struct ContentIndex {
    dir: PathBuf,
    index: TIndex,
    reader: IndexReader,
    f_path: Field,
    f_body: Field,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UpdateStats {
    pub indexed: usize,
    pub unchanged: usize,
    pub removed: usize,
}

fn err(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

impl ContentIndex {
    pub fn open(dir: &Path) -> std::io::Result<ContentIndex> {
        fs::create_dir_all(dir)?;
        let mut sb = Schema::builder();
        let f_path = sb.add_text_field("path", STRING | STORED);
        let f_body = sb.add_text_field("body", TEXT);
        let schema = sb.build();
        let mmap = tantivy::directory::MmapDirectory::open(dir).map_err(err)?;
        let index = match TIndex::open_or_create(mmap, schema.clone()) {
            Ok(i) => i,
            Err(_) => {
                // Incompatible or damaged: start over rather than fail.
                fs::remove_dir_all(dir)?;
                fs::create_dir_all(dir)?;
                TIndex::open_or_create(tantivy::directory::MmapDirectory::open(dir).map_err(err)?, schema).map_err(err)?
            }
        };
        let reader = index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into().map_err(err)?;
        Ok(ContentIndex { dir: dir.to_path_buf(), index, reader, f_path, f_body })
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join("tuck-state.json")
    }

    fn load_state(&self) -> HashMap<String, i64> {
        crate::settings::load_json(&self.state_path())
    }

    /// Bring the content index in line with the name index.
    pub fn update(&self, names: &Index, cancel: &Cancel) -> std::io::Result<UpdateStats> {
        let mut state = self.load_state();
        let mut stats = UpdateStats::default();
        let mut writer = self.index.writer::<TantivyDocument>(50_000_000).map_err(err)?;
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for i in 0..names.len() as u32 {
            if cancel.is_cancelled() {
                break;
            }
            if !matches!(names.kind(i), Kind::Text | Kind::Code) || names.size(i) > MAX_BYTES || names.is_dir(i) || names.is_hidden(i) {
                continue;
            }
            let path = names.path(i);
            let key = path.to_string_lossy().into_owned();
            seen.insert(key.clone());
            let mtime = names.mtime(i);
            if state.get(&key) == Some(&mtime) {
                stats.unchanged += 1;
                continue;
            }
            let Some(text) = read_text(&path) else { continue };
            writer.delete_term(Term::from_field_text(self.f_path, &key));
            writer.add_document(doc!(self.f_path => key.clone(), self.f_body => text)).map_err(err)?;
            state.insert(key, mtime);
            stats.indexed += 1;
        }
        if !cancel.is_cancelled() {
            let gone: Vec<String> = state.keys().filter(|k| !seen.contains(*k)).cloned().collect();
            for k in gone {
                writer.delete_term(Term::from_field_text(self.f_path, &k));
                state.remove(&k);
                stats.removed += 1;
            }
        }
        writer.commit().map_err(err)?;
        crate::settings::save_json(&self.state_path(), &state)?;
        self.reader.reload().map_err(err)?;
        Ok(stats)
    }

    /// Files containing all `terms`, best first.
    pub fn search(&self, terms: &[String], limit: usize) -> Vec<(PathBuf, f32)> {
        if terms.is_empty() {
            return Vec::new();
        }
        let mut qp = QueryParser::for_index(&self.index, vec![self.f_body]);
        qp.set_conjunction_by_default();
        // Quote each word so punctuation in it isn't read as query syntax.
        let text: String = terms.iter().map(|t| format!("\"{}\" ", t.replace('"', " "))).collect();
        let (query, _) = qp.parse_query_lenient(&text);
        let searcher = self.reader.searcher();
        let Ok(top) = searcher.search(&query, &TopDocs::with_limit(limit).order_by_score()) else { return Vec::new() };
        top.into_iter()
            .filter_map(|(score, addr)| {
                let d: TantivyDocument = searcher.doc(addr).ok()?;
                let p = d.get_first(self.f_path)?.as_str()?.to_string();
                Some((PathBuf::from(p), score))
            })
            .collect()
    }

    pub fn len(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn read_text(path: &Path) -> Option<String> {
    let mut f = fs::File::open(path).ok()?;
    let mut data = Vec::new();
    f.by_ref().take(MAX_BYTES).read_to_end(&mut data).ok()?;
    if data[..data.len().min(4096)].contains(&0) {
        return None;
    }
    Some(String::from_utf8_lossy(&data).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::index::crawl;

    #[test]
    fn indexes_updates_and_searches_contents() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        fs::create_dir_all(home.join("notes")).unwrap();
        fs::write(home.join("notes/meeting.txt"), "Budget review with Priya on Thursday").unwrap();
        fs::write(home.join("notes/todo.md"), "buy milk; review budget").unwrap();
        fs::write(home.join("notes/main.rs"), "fn budget() {}").unwrap();
        fs::write(home.join("photo.jpg"), "budget").unwrap(); // not a text kind
        let (names, _) = crawl(std::slice::from_ref(&home), None, &Cancel::new());
        let ci = ContentIndex::open(&d.path().join("content")).unwrap();
        let s = ci.update(&names, &Cancel::new()).unwrap();
        assert_eq!(s.indexed, 3);
        let hits: Vec<String> = ci.search(&["budget".into(), "review".into()], 10).into_iter().map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert!(hits.contains(&"meeting.txt".into()) && hits.contains(&"todo.md".into()));
        assert!(ci.search(&["priya".into()], 10).len() == 1, "case-insensitive");

        // Nothing changed: nothing re-indexed.
        let s = ci.update(&names, &Cancel::new()).unwrap();
        assert_eq!((s.indexed, s.unchanged), (0, 3));

        // Delete one, edit one.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fs::remove_file(home.join("notes/todo.md")).unwrap();
        fs::write(home.join("notes/meeting.txt"), "Rescheduled to Friday").unwrap();
        let (names, _) = crawl(std::slice::from_ref(&home), Some(&names), &Cancel::new());
        let s = ci.update(&names, &Cancel::new()).unwrap();
        assert_eq!((s.indexed, s.removed), (1, 1));
        assert!(ci.search(&["review".into()], 10).is_empty());
        assert_eq!(ci.search(&["friday".into()], 10).len(), 1);
        // Reopening keeps the data.
        drop(ci);
        let ci = ContentIndex::open(&d.path().join("content")).unwrap();
        assert_eq!(ci.len(), 2);
    }
}
