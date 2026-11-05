# Kivo

A fast, friendly file manager for Linux, written in Rust with GTK4 and libadwaita. It aims for Dolphin's feature set with a macOS Finder-like feel: double-click archive extraction, Quick Look, instant search, and app installation built in (from [Teku](https://github.com/anuzdhk/teku)).

> **Status: early development (Milestones 1–7 of 9: browsing, file operations, archives, Quick Look, search, app installation, themes and icons).** See [docs/PARITY.md](docs/PARITY.md) for exactly what works and what is partial or untested, and [docs/PLAN.md](docs/PLAN.md) for the architecture and roadmap.

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

## Themes

Pick a theme, accent colour, icon style, text size and spacing in **Preferences** (Ctrl+,). Kivo adjusts colours automatically so all text stays readable (WCAG AA) whatever you pick.

To add your own theme, put a `.toml` file in `~/.config/kivo/themes/`:

```toml
name = "Solarized Dark"
dark = true
[colors]
window_bg = "#002b36"
window_fg = "#93a1a1"
sidebar_bg = "#073642"   # optional, like every key below
dim_fg = "#839496"
accent = "#268bd2"
```

## Testing hooks

The app can be driven from scripts for smoke tests and screenshots:

```bash
KIVO_ACTIONS="win.view-mode::details;win.split" KIVO_SCREENSHOT=/tmp/shot.png ./target/release/kivo ~/
```

`KIVO_ACTIONS` runs window/app actions in order. `KIVO_SCREENSHOT` renders the window to PNG and then closes it (this works on both Wayland and X11). `KIVO_DEBUG_TIMING=1` prints listing timings.

## Third-party notices

- UI icons: [Tabler Icons](https://github.com/tabler/tabler-icons), MIT (`LICENSES/tabler-icons-MIT.txt`), and [Material Symbols](https://github.com/google/material-design-icons), Apache-2.0 (`LICENSES/material-symbols-Apache-2.0.txt`). See `data/icons/README.md`. Kivo's folder and document icons are original artwork.

- App installation is ported from [Teku](https://github.com/anuzdhk/teku) by anuzdhk (GPL-3.0-or-later): file-type detection, distro detection, install routes and commands, AppImage menu entries (`teku-*.desktop` with `X-Teku-AppImage`, kept compatible), messages and history format. See `crates/kivo-core/src/install/`.

- `crates/kivo-core/tests/fixtures/tree.rar` comes from the compress-tools crate (MIT OR Apache-2.0). See `crates/kivo-core/tests/fixtures/README.md`.
- RAR extraction uses the system libarchive (BSD-2-Clause). Password-protected RAR uses the system `unrar` if present; it is not bundled.

## License

GPL-3.0-or-later (see [LICENSE](LICENSE)). Kivo includes code ported from Teku, which is also GPL-3.0-or-later, so the combined work is distributed under GPL-3.0-or-later with full source available.
