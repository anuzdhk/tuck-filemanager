# Strata

A fast, friendly file manager for Linux, written in Rust with GTK4 and libadwaita. It aims for Dolphin's feature set with a macOS Finder-like feel: double-click archive extraction, Quick Look, instant search, and app installation built in (from [Teku](https://github.com/anuzdhk/teku)).

> **Status: early development (Milestone 1 of 9, browsing).** See [docs/PARITY.md](docs/PARITY.md) for exactly what works and what is partial or untested, and [docs/PLAN.md](docs/PLAN.md) for the architecture and roadmap.

## Build

Requirements: Rust (stable), GTK ≥ 4.16 and libadwaita ≥ 1.6 development files.

Debian / Ubuntu:

```bash
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev
```

Fedora:

```bash
sudo dnf install gcc pkgconf-pkg-config gtk4-devel libadwaita-devel
```

Arch:

```bash
sudo pacman -S base-devel gtk4 libadwaita
```

Then:

```bash
cargo build --release
./target/release/strata            # restores your last session, or opens Home
./target/release/strata ~/Downloads
cargo test --workspace             # core unit tests
```

## Layout

- `crates/strata-core`: UI-free logic (sorting, grouping, listing, paths, settings) with its own tests.
- `crates/strata`: the GTK application.
- `docs/`: plan, Dolphin parity checklist, benchmarks.

## Testing hooks

The app can be driven from scripts for smoke tests and screenshots:

```bash
STRATA_ACTIONS="win.view-mode::details;win.split" STRATA_SCREENSHOT=/tmp/shot.png ./target/release/strata ~/
```

`STRATA_ACTIONS` runs window/app actions in order. `STRATA_SCREENSHOT` renders the window to PNG and then closes it (this works on both Wayland and X11). `STRATA_DEBUG_TIMING=1` prints listing timings.

## License

GPL-3.0-or-later (see [LICENSE](LICENSE)). The project includes logic ported from Teku, which is GPL-3.0-or-later.
