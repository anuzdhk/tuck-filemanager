# Benchmarks

All numbers are from the development machine: Ubuntu 26.04, 20 threads, NVMe, GNOME on Wayland, release build. The machine was in normal desktop use during the runs (load average ~3), so expect noise. Each figure is a range over repeated runs, not a best case.

## Opening a folder with 100,000 files (Milestone 1)

Set-up: `seq 1 100000 | sed 's/^/file-/;s/$/.txt/' | xargs touch` in an empty folder. Timings are printed with `KIVO_DEBUG_TIMING=1`.

| Stage | Time |
|---|---|
| First 300 items on screen | ~50 ms |
| Read directory + stat 100k entries (worker thread, `kivo-core::listing`) | ~0.25 s |
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
cargo build --release -p kivo
KIVO_DEBUG_TIMING=1 ./target/release/kivo /tmp/big
# Simulate opening the folder after the window is up:
KIVO_DEBUG_TIMING=1 KIVO_LIST_DELAY_MS=2000 ./target/release/kivo /tmp/big
```

## Search (Milestone 5)

Not built yet.
