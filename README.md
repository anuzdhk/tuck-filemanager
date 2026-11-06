# Tuck

A fast, friendly file manager for Linux, written in Rust with GTK4 and libadwaita. It aims for Dolphin's feature set with a macOS Finder-like feel: double-click archive extraction, Quick Look, instant search, and app installation built in (from [Teku](https://github.com/anuzdhk/teku)).

> **Status: early development (Milestones 1–8 of 9: browsing, file operations, archives, Quick Look, search, app installation, themes and icons, panels and Dolphin integration).** See [docs/PARITY.md](docs/PARITY.md) for exactly what works and what is partial or untested, and [docs/PLAN.md](docs/PLAN.md) for the architecture and roadmap.

## Build

Requirements: Rust (stable), GTK ≥ 4.16 and libadwaita ≥ 1.6 development files.

Debian / Ubuntu:

```bash
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev libgtksourceview-5-dev libpoppler-glib-dev libarchive-dev libvte-2.91-gtk4-dev gstreamer1.0-plugins-good
```

Fedora:

```bash
sudo dnf install gcc pkgconf-pkg-config gtk4-devel libadwaita-devel gtksourceview5-devel poppler-glib-devel libarchive-devel vte291-gtk4-devel
```

Arch:

```bash
sudo pacman -S base-devel gtk4 libadwaita gtksourceview5 poppler-glib libarchive vte4
```

Then:

```bash
cargo build --release
./target/release/tuck            # restores your last session, or opens Home
./target/release/tuck ~/Downloads
cargo test --workspace             # core unit tests
```

## Layout

- `crates/tuck-core`: UI-free logic (sorting, grouping, listing, file operations, freedesktop trash, undo, names, permissions, terminal detection) with its own tests.
- `crates/tuck`: the GTK application.
- `docs/`: plan, Dolphin parity checklist, benchmarks.

## Panels, tags and Dolphin integration

- **Terminal** (F4): a terminal under the files that follows the folder you're in, and moves the files along when you `cd`.
- **Folders** (F7): a folder tree in the sidebar.
- **Tags, rating, comment**: in the Information panel (F11). Stored the same way as Dolphin (`user.xdg.tags`, `user.baloo.rating`, `user.xdg.comment`), so both apps see them. Tags also appear in the sidebar; click one to find everything with that tag.
- **Git**: coloured dots on changed files when a folder is in a git repository (needs `git` installed).
- **Service menus**: actions installed for Dolphin (`~/.local/share/kio/servicemenus`, `/usr/share/kio/servicemenus`) appear in Tuck's context menu.
- **Places**: Ctrl+B adds a folder (shared with GTK's bookmarks); right-click to rename or remove. Connect to Server… opens SFTP, SMB, FTP and WebDAV shares.
- **Shortcuts**: change any of them in Preferences › Keyboard.

## Updates

Preferences › Updates › **Check Now** asks GitHub for the latest release of this project. If there's a newer one, Tuck shows its notes and offers the package that fits your system (`.deb` on Debian/Ubuntu, `.rpm` on Fedora/openSUSE, an Arch package, or the AppImage when you're running one or nothing else fits). **Download** saves it to your Downloads folder with progress; when the release publishes a `SHA256SUMS` file (or `<file>.sha256`), the download is verified and discarded if it doesn't match. **Install…** then hands it to Tuck's normal install flow, which asks before changing anything. Turn on **Check Automatically** for a once-a-day check at startup; it only fetches the version information.

Releases are expected at `github.com/anuzdhk/kivo-filemanager` (`REPO` in `crates/tuck-core/src/update.rs`). For release builds, attach the packages and a `SHA256SUMS` file to each GitHub release.

Tuck was called Kivo during development. On first start it moves Kivo's settings, session and caches (`~/.config/kivo`, `~/.local/state/kivo`, `~/.cache/kivo`) to the new `tuck` folders.

## Themes

Pick a theme from the gallery, an accent colour, an icon style (Tuck, Material, or **System** to use your desktop's own icon theme), text size and spacing in **Preferences** (Ctrl+,). Tuck adjusts colours automatically so all text stays readable (WCAG AA) whatever you pick.

To add your own theme, put a `.toml` file in `~/.config/tuck/themes/` and press **Reload Themes**; it appears in the gallery:

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
TUCK_ACTIONS="win.view-mode::details;win.split" TUCK_SCREENSHOT=/tmp/shot.png ./target/release/tuck ~/
```

`TUCK_ACTIONS` runs window/app actions in order. `TUCK_SCREENSHOT` renders the window to PNG and then closes it (this works on both Wayland and X11). `TUCK_DEBUG_TIMING=1` prints listing timings. `TUCK_DEBUG_KEYS=1` logs which widget has focus for each key press, and whether the file view handled it.

Adding a built-in theme: add an entry with its TOML to `BUILTIN` and its name to `CHOICES` in `crates/tuck-core/src/theme.rs`. The gallery, readability checks and tests pick it up from there.

## Third-party notices

- UI icons: [Tabler Icons](https://github.com/tabler/tabler-icons), MIT (`LICENSES/tabler-icons-MIT.txt`), and [Material Symbols](https://github.com/google/material-design-icons), Apache-2.0 (`LICENSES/material-symbols-Apache-2.0.txt`). See `data/icons/README.md`. Tuck's folder and document icons are original artwork.

- App installation is ported from [Teku](https://github.com/anuzdhk/teku) by anuzdhk (GPL-3.0-or-later): file-type detection, distro detection, install routes and commands, AppImage menu entries (`teku-*.desktop` with `X-Teku-AppImage`, kept compatible), messages and history format. See `crates/tuck-core/src/install/`.

- `crates/tuck-core/tests/fixtures/tree.rar` comes from the compress-tools crate (MIT OR Apache-2.0). See `crates/tuck-core/tests/fixtures/README.md`.
- RAR extraction uses the system libarchive (BSD-2-Clause). Password-protected RAR uses the system `unrar` if present; it is not bundled.

## License

GPL-3.0-or-later (see [LICENSE](LICENSE)). Tuck includes code ported from Teku, which is also GPL-3.0-or-later, so the combined work is distributed under GPL-3.0-or-later with full source available.
