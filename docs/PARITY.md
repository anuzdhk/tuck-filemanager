# Dolphin parity checklist

Status: ✅ done (implemented **and** tested), 🟡 partial (what's missing or untested is noted), ❌ missing.
Reference: Dolphin (KF6) handbook. This file is updated at the end of every milestone.

"Tested" here means exercised in the real app through its actions (scripted with `STRATA_ACTIONS`, checked in screenshots) or by unit tests. GTK4 has no public API for synthesizing key presses, so behaviour that only runs on physical key or mouse input is marked 🟡 until it's checked by hand.

_Last updated: Milestone 1 (browsing)._

## Views
| Feature | Status | Notes |
|---|---|---|
| Icons view | ✅ | Virtualized `GtkGridView` |
| Compact view | ✅ | Small icons beside names, multi-column grid |
| Details view | ✅ | Name, Size, Date Modified, Kind; resizable, reorderable columns; click a header to sort |
| Expandable folders in Details | 🟡 | Expanders render and child folders load. Expanded children don't live-update. Turned off while "Show in Groups" is on (GTK's tree model has no sections). Expanding hasn't been clicked through by hand yet. |
| Zoom in/out/reset (Ctrl +/−/0, Ctrl+wheel) | 🟡 | Zoom actions tested (7 steps, per view mode, saved per folder). Ctrl+wheel is implemented but untested. |
| Previews toggle | ❌ | Milestone 4 (thumbnails) |
| Hidden files toggle (Ctrl+H, Alt+.) | ✅ | Hidden items dimmed; heading shows the "N hidden" count |
| Sort by name/size/date/kind/extension, ascending/descending, folders first | ✅ | Natural, case-insensitive order (`file2` < `file10`), matching Dolphin |
| Group by sort role | 🟡 | Group headers in Details only. GTK's grid view has no section headers, so Icons and Compact sort in group order but show no headers. |
| Per-folder view settings (and "apply to all") | 🟡 | Per-folder settings tested (saved to `~/.config/strata/views.json`). "Use this view for all folders" is implemented but untested. |
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
| Drag and drop between tabs and panes | ❌ | Milestone 2 (needs file operations) |
| Session restore | ✅ | Tabs, split state, active pane, window size and panel visibility |
| Type-ahead selection | 🟡 | Matching logic unit-tested; key handling untested |
| Filter bar (Ctrl+I) | 🟡 | Implemented, with an empty-result message; typing path untested |
| Selection mode | ❌ | |
| Select all / invert selection | 🟡 | Select all tested; invert implemented but untested |

## Panels
| Feature | Status | Notes |
|---|---|---|
| Places panel (edit, add, hide, reorder) | 🟡 | XDG user folders, Recent, Trash and Computer, with current-place highlight. No editing, adding, hiding or reordering yet. |
| Devices section (mount/unmount/eject) | 🟡 | Devices listed and update on change. Mount, unmount and eject (with plain-language errors) are implemented but untested. |
| Folders panel (tree) | ❌ | Milestone 8 |
| Information panel (preview + metadata) | 🟡 | Large icon, kind, size, dates, link target, location, item count for folders, multi-selection totals. No preview yet (Milestone 4). |
| Terminal panel (F4), follows current folder | ❌ | Milestone 8 |
| Storage indicator | ✅ | Home file system used/free; turns red above 90% |

## File operations
| Feature | Status | Notes |
|---|---|---|
| Copy / Cut / Paste | ❌ | Milestone 2 |
| Move / Copy to (dialog) | ❌ | |
| Rename (F2), inline | ❌ | |
| Batch rename | ❌ | |
| Duplicate (Ctrl+D) | ❌ | |
| Move to Trash / Restore | ❌ | Trash location can already be browsed |
| Permanent delete (Shift+Del) with confirmation | ❌ | |
| Create symlink | ❌ | |
| Undo / Redo (Ctrl+Z) | ❌ | |
| Background jobs with progress and cancel | ❌ | |
| Conflict handling: replace / skip / keep both / apply to all | ❌ | |
| Create New: folder, file, from templates | ❌ | |

## Integration
| Feature | Status | Notes |
|---|---|---|
| Open with default app | 🟡 | Double-click / Enter → `GtkFileLauncher` (portal-aware), with a toast on failure; untested by click |
| Open With (and set default) | ❌ | |
| Open in Terminal | ❌ | |
| Copy Path / Copy Location | ❌ | |
| Custom context-menu actions (service menus) | ❌ | |
| Properties: permissions, ownership, size, folder size, free space | ❌ | |
| Network locations: SFTP, SMB, FTP, WebDAV | 🟡 | URIs typed into the location bar are listed through GVfs. Untested against real servers. No "Connect to Server" UI. |
| Tags (xattr `user.xdg.tags`, Baloo-compatible) | ❌ | |
| Comments (`user.xdg.comment`) and rating | ❌ | |
| Git status indicators | ❌ | |
| Search (names + contents, filters) | ❌ | Milestone 5 |
| Live folder updates | ✅ | Create, delete and rename from outside the app appear immediately |
| Keyboard-only operation | 🟡 | Every Milestone 1 action has a shortcut (see Keyboard Shortcuts, Ctrl+?), but not yet verified with keyboard only |
| Customizable shortcuts | ❌ | Shortcuts come from one table (`SHORTCUTS` in `main.rs`) but can't be edited yet |

## Beyond Dolphin (this project)
| Feature | Status | Notes |
|---|---|---|
| Double-click archive extraction | ❌ | Milestone 3 |
| Compress from context menu | ❌ | |
| Quick Look (Space) | ❌ | Milestone 4 |
| Indexed search < 50 ms / 1M files | ❌ | Milestone 5 |
| Teku app installation | ❌ | Milestone 6 |
| 10 themes + accent colours + user themes | ❌ | Milestone 7. Follows the system light/dark scheme today. |
| Icon theme: Normal / Material | ❌ | Milestone 7. Uses the system icon theme today. |
| 100,000-item folders | 🟡 | See `docs/BENCHMARKS.md`. Loads in 0.6–2.3 s; the UI blocks during the final insert. Scrolling is virtualized, but frame times haven't been measured. |
