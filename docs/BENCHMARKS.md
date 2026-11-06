# Benchmarks

All numbers are from the development machine: Ubuntu 26.04, 20 threads, NVMe, GNOME on Wayland, release build. The machine was in normal desktop use during the runs (load average ~3), so expect noise. Each figure is a range over repeated runs, not a best case.

## Opening a folder with 100,000 files (Milestone 1)

Set-up: `seq 1 100000 | sed 's/^/file-/;s/$/.txt/' | xargs touch` in an empty folder. Timings are printed with `TUCK_DEBUG_TIMING=1`.

| Stage | Time |
|---|---|
| First 300 items on screen | ~50 ms |
| Read directory + stat 100k entries (worker thread, `tuck-core::listing`) | ~0.25 s |
| Build 100k `FileItem`s (main thread) | 30–55 ms |
| Insert into GTK models (filter → sort → selection → view), window already open | 0.33–2.3 s |
| Same insert with `GTK_A11Y=none` | 0.27–0.5 s |

How it got here, from the first working version, which took **8.7 s**:

1. **Stopped GIO from sniffing content types and stat-ing thumbnails** for every file (`standard::content-type`, `thumbnail::path`). 8.7 s → 4.8 s.
2. **Native lister on a worker thread.** GIO's enumerator alone needs 1.3–2.2 s for 100k files. Reading the directory directly takes ~0.25 s, and content types are guessed once per extension (cached). 4.8 s → 1.8 s.
3. **Precomputed natural-sort keys.** Each comparison is a memcmp instead of a Unicode-aware walk. A randomized test checks that the key order equals the natural comparison.
4. **Batches pre-sorted on the worker thread, and the store swapped in one change** instead of 50 splices. Hidden views are detached from the model.

**What's left.** The final insert is dominated by GTK's accessibility bookkeeping. The run-to-run variance (0.3 s vs 2.3 s) tracks AT-SPI activity on the desktop: with `GTK_A11Y=none` it stays under 0.5 s. Turning accessibility off isn't an option, because screen readers have to work. During that insert the UI blocks once; afterwards scrolling is virtualized (only visible rows exist as widgets). Frame times while scrolling haven't been measured yet.

Reproduce:

```bash
mkdir -p /tmp/big && (cd /tmp/big && seq 1 100000 | sed 's/^/file-/;s/$/.txt/' | xargs touch)
cargo build --release -p tuck
TUCK_DEBUG_TIMING=1 ./target/release/tuck /tmp/big
# Simulate opening the folder after the window is up:
TUCK_DEBUG_TIMING=1 TUCK_LIST_DELAY_MS=2000 ./target/release/tuck /tmp/big
```

## Search (Milestone 5)

Target: results under 50 ms on 1,000,000 indexed files.

Reproduce (adding `--home` also crawls your home folder; it reads only names, sizes and dates):

```bash
cargo run --release -p tuck-core --example search_bench -- --home
RAYON_NUM_THREADS=2 cargo run --release -p tuck-core --example search_bench
```

The synthetic index has 1,000,000 entries with realistic names, folder depth, extensions, hidden items, sizes and dates. Each query below ran 40 times after a warm-up and returns the best 500 results. Times are per query.

### 20 threads (this machine)

| Query | Matches | Median | p95 | Max |
|---|---:|---:|---:|---:|
| `report` | 33,232 | 2.3 ms | 2.9 ms | 3.3 ms |
| `re` | 220,457 | 4.6 ms | 5.5 ms | 5.9 ms |
| `e` (worst case) | 713,437 | 10.6 ms | 13.0 ms | 14.1 ms |
| `invoice 2024` | 3 | 2.6 ms | 3.1 ms | 3.3 ms |
| `holiday beach kind:image` | 90 | 2.9 ms | 3.4 ms | 3.9 ms |
| `size:>40mb` | 186,193 | 2.4 ms | 3.0 ms | 3.1 ms |
| `zzzzqq` (no match) | 0 | 2.2 ms | 2.4 ms | 2.8 ms |
| `photo in:/bench/home` | 33,197 | 3.4 ms | 3.8 ms | 4.0 ms |

### 4 and 2 threads (a typical laptop, and a slow one)

| Query | 4 threads, median | 2 threads, median | 2 threads, max |
|---|---:|---:|---:|
| `report` | 6.5 ms | 11.0 ms | 13.1 ms |
| `re` | 11.4 ms | 18.5 ms | 20.1 ms |
| `e` (worst case) | 23.9 ms | 44.6 ms | 49.8 ms |
| `holiday beach kind:image` | 7.2 ms | 14.3 ms | 15.3 ms |
| `budget modified:month` | 4.9 ms | 9.6 ms | 10.7 ms |
| `zzzzqq` | 5.4 ms | 10.2 ms | 12.8 ms |

The target is met everywhere. On two threads the single-letter worst case comes closest, at 44.6 ms median and 49.8 ms max.

### Index storage and upkeep

| | |
|---|---|
| Build 1M entries in memory (synthetic) | 0.32 s |
| Save to disk | 40 ms |
| Load at startup | 71–75 ms |
| File size for 1M entries | 74.7 MB |
| Real home folder: 416,013 entries, first crawl | 5.0 s (17,340 folders read) |
| Real home folder: incremental refresh | 0.08 s (17,339 folders reused, 1 re-read) |
| Real home folder queries (`report`, `e`, `kind:pdf`) | 0.3–2.2 ms median |

How it stays fast:
- All names are lowercased into one contiguous buffer, so a query is a parallel `memmem` scan with ranking only for matches.
- The index is immutable. A refresh builds a new one on an idle-priority thread (`nice 19`, `ioprio` idle class) and swaps it in.
- A refresh `stat`s each folder and re-reads only folders whose modification time changed.

Content search (tantivy) covers text and source files up to 1 MB and is updated the same way. Its query time is included in the in-app timings ("0.7 ms" for a content search in the test home). It hasn't been benchmarked at scale yet.
