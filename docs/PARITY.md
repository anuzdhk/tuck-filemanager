# Dolphin parity checklist

Status: ✅ done (implemented **and** tested), 🟡 partial (what's missing or untested is noted), ❌ missing.
Reference: Dolphin (KF6) handbook. This file is updated at the end of every milestone.

"Tested" here means exercised in the real app through its actions (scripted with `KIVO_ACTIONS`, checked in screenshots) or by unit tests. GTK4 has no public API for synthesizing key presses, so behaviour that only runs on physical key or mouse input is marked 🟡 until it's checked by hand.

_Last updated: Milestone 7 (themes, accents, icons, accessibility)._

The test hooks can now also select items, type into dialogs, answer dialogs, open context menus and simulate drops (`select:`, `type:`, `respond:`, `menu`, `drop-copy:` …; see `crates/kivo/src/debug.rs`).

## Views
| Feature | Status | Notes |
|---|---|---|
| Icons view | ✅ | Virtualized `GtkGridView` |
| Compact view | ✅ | Small icons beside names, multi-column grid |
| Details view | ✅ | Name, Size, Date Modified, Kind; resizable, reorderable columns; click a header to sort |
| Expandable folders in Details | 🟡 | Expanders render and child folders load. Expanded children don't live-update. Turned off while "Show in Groups" is on (GTK's tree model has no sections). Expanding hasn't been clicked through by hand yet. |
| Zoom in/out/reset (Ctrl +/−/0, Ctrl+wheel) | 🟡 | Zoom actions tested (7 steps, per view mode, saved per folder). Ctrl+wheel is implemented but untested. |
| Previews toggle | ✅ | "Show Previews" in the sort/view menu, saved per folder. Thumbnails follow the freedesktop spec and share the cache with Nautilus/Dolphin (MD5 of URI, `Thumb::URI`/`Thumb::MTime` validation, `fail/` markers, 0600 files written atomically). Images are scaled in-process; everything else uses the system `.thumbnailer` entries (tested: video via the GStreamer thumbnailer). PDFs fall back to Kivo's own Poppler renderer when the system PDF thumbnailer is missing or broken (it is broken on the test machine). Work runs on 4 worker threads, newest request first, with a 600-texture memory cache. |
| Hidden files toggle (Ctrl+H, Alt+.) | ✅ | Hidden items dimmed; heading shows the "N hidden" count |
| Sort by name/size/date/kind/extension, ascending/descending, folders first | ✅ | Natural, case-insensitive order (`file2` < `file10`), matching Dolphin |
| Group by sort role | 🟡 | Group headers in Details only. GTK's grid view has no section headers, so Icons and Compact sort in group order but show no headers. |
| Per-folder view settings (and "apply to all") | 🟡 | Per-folder settings tested (saved to `~/.config/kivo/views.json`). "Use this view for all folders" is implemented but untested. |
| Additional information columns | 🟡 | Fixed set of 4 columns; no column chooser yet |

## Navigation
| Feature | Status | Notes |
|---|---|---|
| Breadcrumb location bar | ✅ | Home-relative crumbs, scrolls to the current folder, works for remote URIs |
| Editable location bar (Ctrl+L), path completion | 🟡 | Entry, `~` expansion, URIs, clear error toasts and Tab completion are implemented. Completion logic is unit-tested; the typing path is untested. |
| Back / Forward / Up / Home | ✅ | Up selects the folder you came from. Mouse back/forward buttons are implemented but untested. |
| Recently closed tabs | ✅ | Ctrl+Shift+T |
| Tabs (new, close, reorder, middle-click open) | 🟡 | New, close and reopen tested; reordering is built into `AdwTabBar`. Middle-click and Ctrl+Enter "open folder in new tab" are untested. |
| Split view (F3), focus switch | ✅ | Active pane gets an accent rule; Ctrl+F3 or Tab switches panes |
| Drag and drop between tabs and panes | 🟡 | Drag from any view; drop onto a folder, empty space, another pane or a tab. Move within a drive, copy across drives; Ctrl copies, Shift moves, Ctrl+Shift links. The drop pipeline is tested through a hook; real pointer drags and tab drops are untested. Dropping onto sidebar places isn't supported yet. |
| Session restore | ✅ | Tabs, split state, active pane, window size and panel visibility |
| Type-ahead selection | 🟡 | Matching logic unit-tested; key handling untested |
| Filter bar (Ctrl+I) | 🟡 | Implemented, with an empty-result message; typing path untested |
| Selection mode | ❌ | |
| Select all / invert selection | ✅ | Both tested. Selection shortcuts only act while a file view has focus, so Ctrl+A still works in text fields. |

## Panels
| Feature | Status | Notes |
|---|---|---|
| Places panel (edit, add, hide, reorder) | 🟡 | XDG user folders, Recent, Trash and Computer, with current-place highlight. No editing, adding, hiding or reordering yet. |
| Devices section (mount/unmount/eject) | 🟡 | Devices listed and update on change. Mount, unmount and eject (with plain-language errors) are implemented but untested. |
| Folders panel (tree) | ❌ | Milestone 8 |
| Information panel (preview + metadata) | ✅ | Thumbnail preview (falls back to a large icon), kind, size, dates, link target, location, item count for folders, multi-selection totals |
| Terminal panel (F4), follows current folder | ❌ | Milestone 8 |
| Storage indicator | ✅ | Home file system used/free; turns red above 90% |

## File operations
| Feature | Status | Notes |
|---|---|---|
| Copy / Cut / Paste | 🟡 | Clipboard uses GNOME (`x-special/gnome-copied-files`), KDE (`x-kde-cutselection`), `text/uri-list` and plain-text formats, so it should interoperate with Nautilus and Dolphin. The transfer behind paste is tested. Wayland only lets a focused window own the clipboard and test windows are unfocused, so Ctrl+C/Ctrl+V needs a hand test. |
| Move / Copy to (dialog) | ❌ | Use drag and drop or cut/paste for now |
| Rename (F2), inline | ✅ | Rename dialog, not inline: the name is selected without its extension, with live validation, a warning when the extension or visibility changes, never overwrites. Case-only renames work. |
| Batch rename | ✅ | `###` counters or find/replace, live preview, collision check. Two-phase rename, so swaps work and a failure rolls everything back. |
| Duplicate (Ctrl+D) | ✅ | "Report copy.pdf", "Report copy 2.pdf", for files and folders |
| Move to Trash / Restore | ✅ | Own freedesktop Trash implementation (home trash and per-drive `.Trash-$uid`), checked against GNOME: `gio list trash:///` shows Kivo-trashed items with their original path, and Restore from Kivo's Trash view works. Restore never overwrites and recreates missing folders. Undo toast. Drives without a trash offer permanent deletion instead. |
| Permanent delete (Shift+Del) with confirmation | ✅ | Confirmation names the items and says it can't be undone; Cancel is the default. Empty Trash is confirmed too. |
| Create symlink | ✅ | "Create Link" in the menu and Ctrl+Shift drop |
| Undo / Redo (Ctrl+Z) | 🟡 | Undo tested for move, copy, rename, trash and new items. Undo never deletes: undoing a copy moves the copy to the trash. History lasts for the session. No Redo yet. |
| Background jobs with progress and cancel | ✅ | Jobs run on worker threads. The header shows a spinner and a popover with progress bar, bytes and items, and Cancel. Each top-level item is all-or-nothing: cancel or failure removes what it created, and a move deletes the source only after a complete copy. Files are written under a hidden temporary name and renamed into place. A free-space check runs before starting. Closing the window asks, cancels and cleans up. After a crash or `kill -9`, leftovers are swept on the next start (tested). |
| Conflict handling: replace / skip / keep both / apply to all | ✅ | Dialog shows size and date of both versions. Folder onto folder offers Merge and asks per clashing file inside. "Apply to all" is remembered separately for files and folders. Replace swaps atomically. |
| Create New: folder, file, from templates | 🟡 | New Folder tested. New Document (empty file, or any file in `~/Templates`) is implemented but untested. |

## Integration
| Feature | Status | Notes |
|---|---|---|
| Open with default app | 🟡 | Double-click / Enter → `GtkFileLauncher` (portal-aware); untested by click |
| Open With (and set default) | 🟡 | Uses the system app chooser (portal), where the default can be set. Untested, because it opens an external dialog. |
| Open in Terminal | ✅ | Uses `$TERMINAL`, then the desktop's own terminal (Konsole on KDE, Ptyxis/Console/GNOME Terminal on GNOME, …), then 15 known terminals, each with the right working-folder flag. Tested with a stand-in terminal (it started in the right folder). |
| Copy Path / Copy Location | 🟡 | Implemented (Ctrl+Alt+C); the clipboard can't be checked from unfocused test windows |
| Context menus | ✅ | Item menu (common actions at the top level), background menu (New Folder, New Document, Paste, Open in Terminal, Show Hidden, Properties) and a Trash menu (Restore, Delete Permanently, Empty Trash) |
| Custom context-menu actions (service menus) | ❌ | |
| Properties: permissions, ownership, size, folder size, free space | 🟡 | Kind, background folder size and item count, location (with "open containing folder"), dates, link target, original location for trashed items, drive free space and type. Permissions: owner, group and others access plus "allow running as a program", applied immediately; you can only change them if you own the file. Ownership is shown but can't be changed (needs administrator rights). Permission editing is untested. |
| Network locations: SFTP, SMB, FTP, WebDAV | 🟡 | URIs typed into the location bar are listed through GVfs. Untested against real servers. No "Connect to Server" UI. |
| Tags (xattr `user.xdg.tags`, Baloo-compatible) | ❌ | |
| Comments (`user.xdg.comment`) and rating | ❌ | |
| Git status indicators | ❌ | |
| Search (names + contents, filters) | ✅ | Ctrl+F or the search button. Results show in the current pane as you type, best match first: exact name, then prefix, then word start, then anywhere, adjusted for depth, recency and hidden items. Details shows a "Where" column. Filters: kind, date, size (drop-downs or typed `kind:pdf`, `modified:week`, `size:>10mb`), `tag:` (Dolphin-compatible `user.xdg.tags`), `in:` path, `hidden:yes`. Scope: This Folder or Everywhere. "Contents" also searches inside text and source files. All tested in the app. |
| Search index | ✅ | Persistent name index of the home folder, loaded at startup in ~75 ms, then refreshed on an idle-priority thread every 5 minutes and soon after any change made in Kivo. Refreshes re-read only changed folders (0.08 s for 416k entries). Skips the contents of `.git`, `node_modules`, caches and anything with `CACHEDIR.TAG`; doesn't cross into other drives. Under 50 ms on 1M entries even on 2 threads (see `docs/BENCHMARKS.md`). |
| Live search fallback | ✅ | Folders outside the index (other drives, system folders), or before the first index is ready, are searched by a parallel walk that streams results in batches (tested on `/usr/share/doc`: 2,810 results in 0.1 s) |
| Content index | 🟡 | tantivy full-text index of text and source files ≤ 1 MB, incremental by modification time, deletions dropped (unit-tested). Not benchmarked on a large real home folder. PDFs and office documents aren't content-indexed yet. |
| Live folder updates | ✅ | Create, delete and rename from outside the app appear immediately |
| Keyboard-only operation | 🟡 | Every Milestone 1–2 action has a shortcut; the Menu key and Shift+F10 open the context menu. Not yet verified with keyboard only. |
| Customizable shortcuts | ❌ | Shortcuts come from one table (`SHORTCUTS` in `main.rs`) but can't be edited yet |

## Beyond Dolphin (this project)
| Feature | Status | Notes |
|---|---|---|
| Double-click archive extraction | ✅ | Detected by content, so renamed archives work and ZIP-based documents (.docx, .odt, .epub, .jar …) still open normally. Extracts next to the archive: one top-level item is kept as is, several go into a folder named after the archive, never overwriting ("Project (2)"). Runs in the background with progress and cancel, then selects the result, with an Undo toast. Tested in the app: multi-item zip, single-folder tar.gz, disguised zip, password zip (ZipCrypto, from the system `zip` tool), malicious zip, and cancelling a 1.5 GB extraction (nothing left behind). |
| Formats | 🟡 | zip (incl. ZipCrypto and AES passwords), tar, tar.gz, tar.bz2, tar.xz, tar.zst, gz, bz2, xz, zst, 7z (incl. AES passwords) and rar (via libarchive) are all unit-tested. Password-protected RAR uses the system `unrar` if installed (its licence doesn't allow bundling); that path is untested. RAR symlinks aren't extracted (libarchive doesn't expose their targets). |
| Password only when needed | ✅ | Asked only when the archive turns out to be encrypted; a wrong password says so and asks again |
| Extraction safety | ✅ | Unit-tested: ".." traversal aborts with nothing extracted; absolute paths become relative; symlinks are created last and dropped if they resolve outside what's placed next to the archive (including link-chain tricks); nothing is written through a symlink; size, entry-count and compression-ratio limits stop bombs; free space is watched; setuid/setgid bits are dropped. A crash mid-extraction is cleaned up on the next start. |
| Compress from context menu | ✅ | ZIP, tar.gz, tar.xz, tar.zst or 7z, with an optional password for ZIP (AES-256) and 7z (AES-256, encrypted headers). Never overwrites; temporary file renamed into place; progress and cancel. Round trips for every format are unit-tested; the dialog and ZIP output are checked in the app (and read back with Python's `zipfile`). |
| Quick Look (Space) | ✅ | Space opens it, Space or Esc closes it, arrow keys move through the folder while it's open (the selection follows), Return opens. Space still types a space while you're in the middle of type-ahead. The window is reused, so it shows the item's icon at once; decoding, rendering and reading happen on worker threads, and stale results are dropped. Tested in the app with every type: PNG, a 48-megapixel JPEG, animated GIF (first frame only when animations are turned off), MP4 video and Ogg audio (via GStreamer), a multi-page PDF (Poppler, first 12 pages), Rust code (GtkSourceView syntax highlighting, light/dark scheme), Markdown (rendered natively, raw HTML shown as text), ZIP contents, folders, text with an unknown extension (detected by content), and an unknown binary (large icon and details). Header: "Open with <default app>", Open With…, and Extract for archives. |
| Quick Look: app packages | 🟡 | Installers (.deb, .rpm, .AppImage, .flatpakref, .exe …) show a large icon and details; the Install button arrives with the Teku integration (Milestone 6) |
| Quick Look: playback errors | 🟡 | Missing GStreamer plugins show a plain message with what to install. That path is untested, because all plugins are installed here. |
| Indexed search < 50 ms / 1M files | ✅ | 2–11 ms median on 20 threads; ≤ 45 ms median (49.8 ms max) on 2 threads for the worst case |
| Teku app installation | ✅ | Ported natively from Teku, no external dependency. The file type comes from the contents (deb ar header, RPM lead, AppImage `AI\x01/\x02` marker, `[Flatpak Ref]`, PE/MZ, OLE2 .msi, xar, `koly` trailer), so renamed files can't fool it. The distro comes from `ID` then `ID_LIKE`. Routes and commands match Teku exactly: `pkexec env DEBIAN_FRONTEND=noninteractive apt-get install -y`, `pkexec dnf install -y`, `pkexec zypper --non-interactive install --allow-unsigned-rpm`, `flatpak install --noninteractive --from`. Teku's messages are kept for wrong-distro, fake/corrupt, Windows/macOS, missing Flatpak, and cancelled authentication (pkexec 126/127). Unit-tested: detection for every type, os-release for 9 distros, the routing matrix, commands, history. |
| Install flow (double-click, right-click › Install…, Quick Look) | 🟡 | One flow: confirm (type, system, method, destination) → live progress with the installer's output (can't be closed mid-install) → done or error with Show Log. Tested in the app: the AppImage install end to end; confirm dialogs for a real .deb and a .flatpakref; refusals for a fake .deb, an .rpm on Ubuntu, and an .exe. A real apt/dnf/zypper/flatpak install wasn't run, because that changes the system and asks for your password. |
| AppImages | ✅ | Moved to ~/Applications, made executable, name and icon read from the image, menu entry `teku-<id>.desktop` with `X-Teku-AppImage`, the same as Teku, so each can manage the other's installs. Differences: an older AppImage with the same name goes to the Trash instead of being overwritten, Exec paths are escaped, and double-clicking an already-installed AppImage runs it. |
| Install logs, history, removing AppImages | ✅ | Per-install logs plus `last.log`, and a Teku-format tab-separated history. Main menu › Installed Apps… lists AppImages with Remove (moves the AppImage to the Trash, removes the menu entry and icon) and recent activity. Removal is unit-tested; the Remove button wasn't clicked in the app. |
| 10 themes + accent colours + user themes | ✅ | Light, Dark, Follow System (also follows the desktop's high-contrast switch live), Dracula, Material, Translucent, Nord, Catppuccin, Tokyo Night, High Contrast. 9 accent presets, "Theme default", and a custom colour picker, applied to every theme. Changes apply live, with no restart. Every theme was screenshotted in the app (`docs/screenshots/milestone-7/`). |
| Readability guaranteed (WCAG AA) | ✅ | Every theme × every accent is resolved through a contrast engine that nudges colours in OKLCH, keeping their hue, until all text passes 4.5:1 (7:1 in High Contrast). That covers primary, secondary/dim and sidebar text, accent text, text on accent and destructive buttons, selected rows and the header bar. Unit-tested for 9 themes × 16 accents (including pure yellow, white and black) × compositor on/off. File-type badge text also passes AA. |
| Translucent theme | ✅ | Semi-transparent window, sidebar and content (around 78–84% opaque). Opacity is raised automatically until text passes AA over both a white and a black wallpaper, and stacked surfaces are accounted for. Solid colours when no compositor is running (X11 without compositing). Screenshotted over light and dark backdrops. No blur: GTK has no portable blur API. |
| User themes | ✅ | `~/.config/kivo/themes/*.toml` (name, dark, colours; missing colours are derived). Bad files are reported in plain language in Preferences. Colours are auto-fixed for readability (unit-tested with a Solarized theme whose secondary text was too faint). |
| Icon Theme: Normal / Material | ✅ | Normal: Tabler outline icons (MIT, bundled; strokes converted to fills so GTK can recolour them) plus Kivo's original folder icons (tinted with the accent, emblems for Home, Desktop, Documents, Downloads, Pictures, Music, Videos, Templates, Public) and document icons (12 types with type badges). Material: Material Symbols Rounded (Apache-2.0, bundled), all in the accent colour. Both screenshotted. |
| Text size, spacing, reduced motion | 🟡 | Preferences: text size 80–160% (screenshotted at 140%), Compact / Comfortable / Spacious density (screenshotted), Reduce Motion (turns off GTK animations and GIF animation in Quick Look). Reduce Motion was set but not checked visually. |
| 100,000-item folders | 🟡 | See `docs/BENCHMARKS.md`. Loads in 0.6–2.3 s; the UI blocks during the final insert. Scrolling is virtualized, but frame times haven't been measured. |
