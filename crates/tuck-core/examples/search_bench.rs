// SPDX-License-Identifier: GPL-3.0-or-later
//! Search benchmark. Run with:
//!   cargo run --release -p tuck-core --example search_bench [-- --home]
//!
//! 1. Builds a synthetic index of 1,000,000 entries (realistic names, folder
//!    depth and extensions) and times queries over many runs.
//! 2. Times saving and loading that index.
//! 3. With --home, crawls your real home folder (names, sizes and dates
//!    only) and then refreshes it incrementally.

use std::path::{Path, PathBuf};
use std::time::Instant;

use tuck_core::ops::Cancel;
use tuck_core::search::Query;
use tuck_core::search::index::{Builder, Index, crawl};

const WORDS: &[&str] = &[
    "report", "invoice", "photo", "holiday", "beach", "project", "notes", "meeting", "budget", "draft", "final",
    "summary", "design", "logo", "banner", "scan", "receipt", "contract", "letter", "resume", "music", "track",
    "album", "video", "clip", "backup", "config", "main", "test", "lib", "utils", "index", "readme", "data",
    "export", "import", "chart", "plan", "family", "wedding", "trip", "paris", "tokyo", "berlin", "kids", "school",
    "homework", "lecture", "slides", "thesis", "paper", "review", "client", "server", "build", "release", "patch",
];
const EXTS: &[&str] = &["jpg", "png", "pdf", "docx", "txt", "md", "rs", "py", "js", "mp4", "mp3", "zip", "xlsx", "html", "json", "svg"];

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn pick<'a>(&mut self, v: &[&'a str]) -> &'a str {
        v[(self.next() % v.len() as u64) as usize]
    }
}

fn synthetic(n: usize) -> Index {
    let mut r = Rng(0x2545F4914F6CDD1D);
    let mut b = Builder::new();
    let now = chrono::Local::now().timestamp();
    let root = b.push(u32::MAX, b"/bench/home", true, 0, now, false, 0, None);
    let mut dirs: Vec<(u32, u16)> = vec![(root, 0)];
    let mut count = 1;
    while count < n {
        // ~6% folders, the rest files, spread over existing folders.
        let (parent, depth) = dirs[(r.next() % dirs.len() as u64) as usize];
        let hidden = r.next().is_multiple_of(50);
        let name = if r.next().is_multiple_of(16) && depth < 12 {
            let n = format!("{}{} {}", if hidden { "." } else { "" }, r.pick(WORDS), r.next() % 100);
            let id = b.push(parent, n.as_bytes(), true, 0, now - (r.next() % 100_000_000) as i64, hidden, depth + 1, None);
            dirs.push((id, depth + 1));
            count += 1;
            continue;
        } else {
            format!("{}{}-{}_{}.{}", if hidden { "." } else { "" }, r.pick(WORDS), r.pick(WORDS), r.next() % 10_000, r.pick(EXTS))
        };
        b.push(parent, name.as_bytes(), false, r.next() % 50_000_000, now - (r.next() % 300_000_000) as i64, hidden, depth + 1, None);
        count += 1;
    }
    b.finish()
}

fn stats(mut v: Vec<f64>) -> (f64, f64, f64) {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p = |q: f64| v[((v.len() as f64 - 1.0) * q).round() as usize];
    (p(0.5), p(0.95), *v.last().unwrap())
}

fn bench_queries(idx: &Index, home: &Path, queries: &[&str], runs: usize) {
    println!("| Query | Matches | Median | p95 | Max |");
    println!("|---|---:|---:|---:|---:|");
    let now = chrono::Local::now().timestamp();
    for q in queries {
        let parsed = Query::parse(q, home);
        let _ = idx.search(&parsed, 500, now); // warm-up
        let mut times = Vec::new();
        let mut total = 0;
        for _ in 0..runs {
            let t = Instant::now();
            let r = idx.search(&parsed, 500, now);
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            total = r.total;
        }
        let (med, p95, max) = stats(times);
        println!("| `{q}` | {total} | {med:.1} ms | {p95:.1} ms | {max:.1} ms |");
    }
}

fn main() {
    let threads = rayon::current_num_threads();
    println!("Threads: {threads}\n");

    let t = Instant::now();
    let idx = synthetic(1_000_000);
    println!("Synthetic index: {} entries, built in {:.2} s\n", idx.len(), t.elapsed().as_secs_f64());
    bench_queries(
        &idx,
        Path::new("/bench/home"),
        &[
            "report",
            "re",
            "e",
            "invoice 2024",
            "holiday beach kind:image",
            "size:>40mb",
            "budget modified:month",
            "zzzzqq",
            "photo in:/bench/home",
        ],
        40,
    );

    let tmp = std::env::temp_dir().join(format!("tuck-bench-{}.idx", std::process::id()));
    let t = Instant::now();
    idx.save(&tmp).unwrap();
    let save = t.elapsed();
    let size = std::fs::metadata(&tmp).unwrap().len();
    let t = Instant::now();
    let back = Index::load(&tmp).unwrap();
    let load = t.elapsed();
    let _ = std::fs::remove_file(&tmp);
    println!(
        "\nSave: {:.0} ms · Load: {:.0} ms · File: {:.1} MB · Entries after load: {}",
        save.as_secs_f64() * 1000.0,
        load.as_secs_f64() * 1000.0,
        size as f64 / 1e6,
        back.len()
    );

    if std::env::args().any(|a| a == "--home") {
        let home: PathBuf = tuck_core::paths::home();
        let t = Instant::now();
        let (real, s1) = crawl(std::slice::from_ref(&home), None, &Cancel::new());
        let full = t.elapsed();
        let t = Instant::now();
        let (_, s2) = crawl(std::slice::from_ref(&home), Some(&real), &Cancel::new());
        let refresh = t.elapsed();
        println!(
            "\nHome folder: {} entries · full crawl {:.2} s ({} folders read) · incremental refresh {:.2} s ({} folders re-read, {} reused)",
            real.len(),
            full.as_secs_f64(),
            s1.dirs_read,
            refresh.as_secs_f64(),
            s2.dirs_read,
            s2.dirs_reused
        );
        bench_queries(&real, &home, &["report", "e", "kind:pdf", "zzzzqq"], 20);
    }
}
