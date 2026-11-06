# Tuck — plan and architecture

> The app is called **Tuck** (app ID `io.github.anuzdhk.Tuck`).

## Toolkit decision

**GTK4 (gtk4-rs) + libadwaita**, as requested. I considered Qt (closer to Dolphin) and iced/Slint (pure Rust), and GTK4 still wins:

- Native Wayland and X11 from one backend, packaged on every target distro.
- `GtkGridView` / `GtkColumnView` / `GtkTreeListModel` are virtualized, so 100,000-item folders are realistic.
- GIO gives us volumes, mounting and GVfs network backends (SFTP, SMB, FTP, WebDAV) without writing protocol code.
- VTE (embedded terminal), GtkSourceView (syntax highlighting), Poppler (PDF) and GStreamer (video/audio) all have GTK4 builds.
- libadwaita 1.6+ exposes its palette as CSS variables, so our ten themes and custom accents can override one set of tokens instead of fighting the stylesheet.

**Minimum runtime:** GTK 4.16 (CSS variables and `color-mix()`, which the theme engine relies on) and libadwaita 1.6. That covers Ubuntu 24.10+, Debian 13, Fedora 41+, openSUSE Tumbleweed and Leap 16, and Arch. Older LTS releases (Ubuntu 24.04, Debian 12, RHEL 9) get the AppImage, which bundles newer libraries.

## Workspace layout

```
crates/
  tuck-core/     UI-free library, no GTK dependency. Unit + integration tests.
    fs/            listing, sort/group, copy/move/link/rename/batch-rename,
                   conflict policy, undo journal, job engine (cancel, progress)
    trash/         freedesktop Trash spec (home + per-mount .Trash-$uid), restore
    archive/       detect-by-magic, safe extraction, compression
    search/        index (names + content), query parser, ranking, live fallback
    install/       Teku port: content detection, os-release, routing, installers, history
    meta/          tags/comments/rating as xattrs (Dolphin/Baloo compatible), git status
    thumbs/        freedesktop thumbnail cache paths, validation, writing
    settings/      config, per-folder view state, session, themes (TOML)
  tuck-indexd/   optional background indexer binary (low priority I/O + CPU)
  tuck/          GTK4 application: windows, views, panels, Quick Look, dialogs
data/              .desktop, metainfo, MIME, icons, themes, templates
packaging/         deb, rpm spec, PKGBUILD, AppImage, install.sh
docs/              this plan, PARITY.md, BENCHMARKS.md, screenshots
```

The UI talks to the core only through plain Rust types and channels. Every disk, network, thumbnail and search call runs on a worker pool, and results are posted back to the GTK main loop with `glib::MainContext::spawn_local`.

## Key designs

**Job engine.** Each operation (copy, move, extract, compress, install, trash) is a `Job` that streams `Progress` events and checks a `CancelToken` between chunks. Conflicts pause the job and ask the UI (Replace / Skip / Keep both, with "apply to all"). Finished jobs write an inverse operation into the undo journal.

**Extraction.** One `Extractor` trait, implemented with pure-Rust crates for zip (with AES and ZipCrypto passwords), tar, gz, bz2, xz, zst and 7z (`sevenz-rust2`, which supports AES). RAR goes through libarchive by FFI. Encrypted RAR is the one gap: libarchive cannot decrypt it, so we use the system `unrar` if it's installed and otherwise say so plainly. Extraction writes into a hidden staging directory next to the archive, then renames it into place atomically. Cancel or failure deletes the staging directory, so no partial output is left behind. Every entry is checked before it is written:
- path normalized; reject `..`, absolute paths and drive prefixes
- symlinks may only point inside the destination
- limits on total size, entry count and compression ratio (archive-bomb guard), with a clear message

**Search.**
- Name index: all paths go into one contiguous lower-cased byte arena plus an offset table, saved as a memory-mapped snapshot so startup does no parsing. A query is a parallel `memchr::memmem` scan split across cores with rayon, followed by ranking (exact > prefix > word-boundary > substring, then recency and path depth). Scanning ~40 MB for 1M paths takes a few ms on one core, so we have headroom under the 50 ms target.
- Updates: inotify on indexed roots (with a fallback to periodic directory-mtime rescans when the watch limit is reached), batched and written at idle priority.
- Content index: tantivy, for text-like files under a size cap, built at low priority.
- Unindexed locations: a streaming parallel walk (`ignore` crate) that sends batches of results to the UI.
- `cargo bench` with criterion against a synthetic 1M-path set and against a real home directory. The numbers will go in `docs/BENCHMARKS.md`.

**Teku port** (`tuck-core::install`), matching `teku` v0.1.2 behaviour:
- Detection reads the file contents, not the name. ar archive with `debian-binary` → deb. RPM lead magic `ED AB EE DB` → rpm. ELF/ISO with `AI\x01`/`AI\x02` at offset 8 → AppImage. `[Flatpak Ref]` header → flatpakref. `MZ`/PE → Windows. OLE2 + `.msi` → Windows. A `koly` trailer → dmg. `xar!` → pkg. Foreign-OS extension fallback works the same as in Teku. A known package extension with non-matching contents is reported as a fake or corrupted file.
- OS family comes from `ID` then `ID_LIKE` (debian/ubuntu, fedora/rhel/centos, suse/opensuse*/sles, arch), with `/usr/lib/os-release` as fallback.
- Routes: `pkexec env DEBIAN_FRONTEND=noninteractive apt-get install -y`, `pkexec dnf install -y`, `pkexec zypper --non-interactive install --allow-unsigned-rpm`, `flatpak install --noninteractive --from`. AppImages go to `~/Applications`, are made executable, and get `.desktop` metadata and `.DirIcon` extracted for a menu entry marked `X-Teku-AppImage`. That keeps Teku-installed and Tuck-installed AppImages interoperable, so either tool can remove them.
- pkexec exit 126/127 → "Authentication cancelled". History goes in Teku's tab-separated format under `$XDG_STATE_HOME/tuck/`, plus a per-install log.

**Metadata.** Tags go in `user.xdg.tags`, comments in `user.xdg.comment` and ratings in `user.baloo.rating`. These are the xattrs Dolphin/Baloo read and write, so tags show up in both apps.

**Thumbnails.** Uses the freedesktop spec: `$XDG_CACHE_HOME/thumbnails/{normal,large,x-large,xx-large}/<md5(uri)>.png`, with `Thumb::URI`/`Thumb::MTime` validation and the `fail/` directory. Images are thumbnailed in-process. Everything else goes through the system's registered thumbnailers (`/usr/share/thumbnailers/*.thumbnailer`), which is the same cache Nautilus and Dolphin use.

**Theming.**
- Each theme is a TOML file of colour tokens, mapped onto libadwaita CSS variables plus our own tokens. User themes live in `~/.config/tuck/themes/*.toml`.
- The accent is a single colour. Foreground and text-on-accent variants are derived per theme in OKLCH and nudged in lightness until they reach WCAG AA (4.5:1 for text, 3:1 for UI). This check also runs as a unit test across all themes × presets.
- Live switching swaps one `CssProvider`.
- Translucent checks whether the display is composited (always true on Wayland). On X11 without a compositor it falls back to a solid background. The content surfaces stay ≥ 85% opaque, so text contrast doesn't depend on the wallpaper.

**Icons.**
- Tabler (MIT) is bundled as symbolic SVGs in a GResource, so they recolour with the theme.
- Material Symbols Rounded (Apache-2.0) are bundled as symbolic and tinted with the accent.
- Folder and file-type icons are an original SVG set I'll draw: a rounded folder body with a soft tab and an emblem for Home, Documents, and the other standard folders, plus document sheets with a coloured type band.

## Milestones (each one compiles, runs, is tested and screenshotted before the next)

1. **Browsing.** Window shell, sidebar (Places/devices), header, breadcrumb, Icons/Compact/Details views, tabs, split view, sort/group/hidden, keyboard nav, live updates.
2. **File operations.** Job engine, copy/move/rename/batch rename/duplicate/links, trash/restore/delete, undo, conflicts, DnD, clipboard, Properties.
3. **Archives.** Double-click extraction, Compress, safety tests (traversal, symlink, bomb, cancel cleanup).
4. **Quick Look.** All preview types, thumbnails.
5. **Search.** Index, live fallback, filters, benchmark.
6. **Teku.** Detection, routing, install flow, history, AppImage removal.
7. **Themes and icons.** 10 themes, accents, custom themes, icon theme switch, accessibility pass.
8. **Remaining Dolphin parity.** Terminal, Information, Folders panels, network, tags, git, shortcuts editor, service menus, templates, session restore.
9. **Packaging.** deb, rpm, PKGBUILD, AppImage, install script, default-file-manager registration, README.

## License

The Teku port is a derivative of GPL-3.0-or-later code, so the whole project is **GPL-3.0-or-later**, with SPDX headers on every source file. Bundled assets keep their own licences (Tabler: MIT; Material Symbols: Apache-2.0; both GPL-3 compatible) and are listed in `LICENSES/` and `docs/THIRD_PARTY.md`.

## Risks / things I'll flag honestly

- **Encrypted RAR** depends on system `unrar` (non-free licence, so it can't be bundled).
- **Network locations** depend on GVfs being installed. That's standard on GNOME and Xfce but often missing on pure-KDE installs; the app detects this and explains it.
- **inotify** can't watch a full 1M-file home within default limits. The index uses a hybrid of watches plus a periodic rescan, so updates are near-real-time rather than instant everywhere.
- **Translucent blur:** GTK has no portable blur API. We get translucency, but blur happens only if the compositor applies it (e.g. KWin rules).
- **Scope:** this is a very large project. I'll report each milestone's real state, keep `PARITY.md` accurate, and won't mark untested work as done.
