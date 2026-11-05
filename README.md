# Kivo

A fast, friendly file manager for Linux, written in Rust with GTK4 and libadwaita. It aims for Dolphin's feature set with a macOS Finder-like feel: double-click archive extraction, Quick Look, instant search, and app installation built in (from [Teku](https://github.com/anuzdhk/teku)).

> **Status: early development (Milestones 1–5 of 9: browsing, file operations, archives, Quick Look, search).** See [docs/PARITY.md](docs/PARITY.md) for exactly what works and what is partial or untested, and [docs/PLAN.md](docs/PLAN.md) for the architecture and roadmap.

## Build

Requirements: Rust (stable), GTK ≥ 4.16 and libadwaita ≥ 1.6 development files.

Debian / Ubuntu:

```bash
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev libgtksourceview-5-dev libpoppler-glib-dev libarchive-dev gstreamer1.0-plugins-good
```

Fedora:

```bash
sudo dnf install gcc pkgconf-pkg-config gtk4-devel libadwaita-devel gtksourceview5-devel poppler-glib-devel libarchive-devel
```

Arch:

```bash
sudo pacman -S base-devel gtk4 libadwaita gtksourceview5 poppler-glib libarchive
```

Then:

```bash
cargo build --release
./target/release/kivo            # restores your last session, or opens Home
./target/release/kivo ~/Downloads
cargo test --workspace             # core unit tests
```

## Layout

- `crates/kivo-core`: UI-free logic (sorting, grouping, listing, file operations, freedesktop trash, undo, names, permissions, terminal detection) with its own tests.
- `crates/kivo`: the GTK application.
- `docs/`: plan, Dolphin parity checklist, benchmarks.

## Testing hooks

The app can be driven from scripts for smoke tests and screenshots:

```bash
KIVO_ACTIONS="win.view-mode::details;win.split" KIVO_SCREENSHOT=/tmp/shot.png ./target/release/kivo ~/
```

`KIVO_ACTIONS` runs window/app actions in order. `KIVO_SCREENSHOT` renders the window to PNG and then closes it (this works on both Wayland and X11). `KIVO_DEBUG_TIMING=1` prints listing timings.

## Third-party notices

- `crates/kivo-core/tests/fixtures/tree.rar` comes from the compress-tools crate (MIT OR Apache-2.0). See `crates/kivo-core/tests/fixtures/README.md`.
- RAR extraction uses the system libarchive (BSD-2-Clause). Password-protected RAR uses the system `unrar` if present; it is not bundled.

## License

GPL-3.0-or-later (see [LICENSE](LICENSE)). The project includes logic ported from Teku, which is GPL-3.0-or-later.
