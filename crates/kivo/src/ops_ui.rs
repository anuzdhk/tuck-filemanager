// SPDX-License-Identifier: GPL-3.0-or-later
//! File-operation actions for a window: clipboard, paste, trash, delete,
//! rename, duplicate, links, new items, terminal, open/open with, drops and
//! undo. Heavy work runs as background jobs; results are reported in plain
//! language and recorded in the undo history.

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use kivo_core::names;
use kivo_core::ops::{self, Mode, OpError};
use kivo_core::paths::{UserDir, UserDirs};
use kivo_core::trash::{Trash, TrashedItem};
use kivo_core::undo::{self, Action, History};

use crate::item::FileItem;
use crate::window::Window;

thread_local! {
    static HISTORY: RefCell<History> = RefCell::new(History::default());
}

fn trash() -> Trash {
    Trash::for_user()
}

fn record(a: Action) {
    HISTORY.with(|h| h.borrow_mut().push(a));
    // Keep search results fresh after changes made here.
    crate::search_ui::nudge();
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

fn names_of(paths: &[PathBuf]) -> Vec<OsString> {
    paths.iter().filter_map(|p| p.file_name().map(|n| n.to_owned())).collect()
}

fn count(n: usize) -> String {
    if n == 1 { "1 item".into() } else { format!("{n} items") }
}

impl Window {
    pub(crate) fn setup_file_actions(&self) {
        let add = |name: &str, f: fn(&Window)| {
            let a = gio::SimpleAction::new(name, None);
            let weak = self.downgrade();
            a.connect_activate(move |_, _| {
                if let Some(w) = Window::from_weak(&weak) {
                    f(&w);
                }
            });
            self.gtk_window().add_action(&a);
        };
        add("open", |w| w.op_open(false));
        add("open-with", |w| w.op_open(true));
        add("open-new-tab", |w| {
            for i in w.selection() {
                if i.is_dir() {
                    w.open_tab(i.target(), false);
                }
            }
        });
        add("copy", |w| w.op_clipboard(false));
        add("cut", |w| w.op_clipboard(true));
        add("paste", |w| w.op_paste(None));
        add("paste-into", |w| {
            let target = w.selection().into_iter().find(|i| i.is_dir()).map(|i| i.target());
            w.op_paste(target);
        });
        add("trash", |w| w.op_trash());
        add("delete", |w| w.op_delete());
        add("restore", |w| w.op_restore());
        add("empty-trash", |w| w.op_empty_trash());
        add("rename", |w| w.op_rename());
        add("duplicate", |w| w.op_duplicate());
        add("link", |w| w.op_link());
        add("new-folder", |w| w.op_new_folder());
        add("copy-path", |w| w.op_copy_path());
        add("open-terminal", |w| {
            if let Some(dir) = w.active_pane().and_then(|p| p.path()) {
                w.op_terminal(&dir);
            }
        });
        add("open-terminal-here", |w| {
            if let Some(dir) = w.selection().into_iter().find(|i| i.is_dir()).and_then(|i| i.path()) {
                w.op_terminal(&dir);
            }
        });
        add("properties", |w| {
            let sel = w.selection();
            if sel.is_empty() {
                w.folder_properties();
            } else {
                crate::properties::show(&w.gtk_window().upcast(), &sel);
            }
        });
        add("folder-properties", |w| w.folder_properties());
        add("undo", |w| w.op_undo());
        add("extract", |w| {
            for i in w.selection() {
                if let (Some(p), true) = (i.path(), looks_like_archive(&i)) {
                    w.extract_archive(p, None);
                }
            }
        });
        add("compress", |w| w.op_compress());
        add("install", |w| {
            if let Some(p) = w.selection().into_iter().find_map(|i| i.path().filter(|p| kivo_core::install::is_installable(p))) {
                w.install_file(p);
            }
        });
        add("installed-apps", |w| w.show_installed_apps());

        // New file: target is a template name, or "" for an empty file.
        let a = gio::SimpleAction::new("new-file", Some(glib::VariantTy::STRING));
        let weak = self.downgrade();
        a.connect_activate(move |_, v| {
            if let (Some(w), Some(t)) = (Window::from_weak(&weak), v.and_then(|v| v.get::<String>())) {
                w.op_new_file(&t);
            }
        });
        self.gtk_window().add_action(&a);
    }

    fn selection(&self) -> Vec<FileItem> {
        self.active_pane().map(|p| p.selected_items()).unwrap_or_default()
    }

    fn parent_widget(&self) -> gtk::Widget {
        self.gtk_window().upcast()
    }

    /// Local paths of the selection, or a toast explaining why not.
    fn selected_paths(&self) -> Option<Vec<PathBuf>> {
        let sel = self.selection();
        if sel.is_empty() {
            return None;
        }
        let paths: Vec<PathBuf> = sel.iter().filter_map(|i| i.path()).collect();
        if paths.len() != sel.len() {
            self.toast("This action isn't available for items in this location yet.");
            return None;
        }
        Some(paths)
    }

    fn current_dir(&self) -> Option<PathBuf> {
        let p = self.active_pane()?.path();
        if p.is_none() {
            self.toast("You can't create or paste items in this location.");
        }
        p
    }

    /// Select `paths` in the active pane if they live in its folder.
    fn select_results(&self, paths: &[PathBuf]) {
        if let Some(pane) = self.active_pane()
            && let Some(dir) = pane.path() {
                let mine: Vec<PathBuf> = paths.iter().filter(|p| p.parent() == Some(dir.as_path())).cloned().collect();
                pane.select_names(names_of(&mine));
            }
    }

    // ------------------------------------------------------------ opening

    fn op_open(&self, choose_app: bool) {
        let sel = self.selection();
        let (dirs, files): (Vec<_>, Vec<_>) = sel.into_iter().partition(|i| i.is_dir());
        if !choose_app {
            if let (Some(d), true) = (dirs.first(), files.is_empty()) {
                if let Some(p) = self.active_pane() {
                    p.navigate(d.target());
                }
                return;
            }
            for d in dirs {
                self.open_tab(d.target(), false);
            }
        }
        self.open_files(files, choose_app);
    }

    /// Open files with their default app (or ask which app). Archives are
    /// extracted instead, like a double-click on macOS.
    pub(crate) fn open_files(&self, files: Vec<FileItem>, choose_app: bool) {
        let window = self.gtk_window();
        for item in files {
            if !choose_app && let Some(p) = item.path() {
                use kivo_core::install::{self, FileType, appimage};
                // An AppImage that's already installed runs; others install.
                if install::file_type(&p) == FileType::AppImage && appimage::is_installed(&appimage::Dirs::user(), &p) {
                    if let Err(e) = gtk::gio::SubprocessLauncher::new(gtk::gio::SubprocessFlags::NONE).spawn(&[p.as_os_str()]) {
                        self.toast(&format!("Couldn't start {}: {}", item.name(), e.message()));
                    }
                    continue;
                }
                if install::is_installable(&p) {
                    self.install_file(p);
                    continue;
                }
            }
            if !choose_app && looks_like_archive(&item)
                && let Some(p) = item.path() {
                    self.extract_archive(p, None);
                    continue;
                }
            let launcher = gtk::FileLauncher::new(Some(&item.target()));
            launcher.set_always_ask(choose_app);
            let weak = self.downgrade();
            let name = item.name();
            launcher.launch(Some(&window), gio::Cancellable::NONE, move |res| {
                if let Err(e) = res {
                    if e.matches(gtk::DialogError::Dismissed) || e.matches(gtk::DialogError::Cancelled) {
                        return;
                    }
                    if let Some(w) = Window::from_weak(&weak) {
                        w.toast(&format!("Couldn't open “{name}”. No app is set to open this kind of file — try Open With."));
                    }
                }
            });
        }
    }

    fn op_terminal(&self, dir: &Path) {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").ok();
        let env_term = std::env::var("TERMINAL").ok();
        let Some(cmd) = kivo_core::terminal::command_for(dir, env_term.as_deref(), desktop.as_deref(), &kivo_core::terminal::on_path) else {
            self.toast("No terminal app was found. Install one (for example GNOME Console or Konsole).");
            return;
        };
        let launcher = gio::SubprocessLauncher::new(gio::SubprocessFlags::NONE);
        launcher.set_cwd(dir);
        let mut argv: Vec<&std::ffi::OsStr> = vec![cmd.program.as_ref()];
        argv.extend(cmd.args.iter().map(|a| std::ffi::OsStr::new(a.as_str())));
        if let Err(e) = launcher.spawn(&argv) {
            self.toast(&format!("Couldn't start {}: {}", cmd.program, e.message()));
        }
    }

    // ---------------------------------------------------------- clipboard

    fn op_clipboard(&self, cut: bool) {
        let sel = self.selection();
        if sel.is_empty() || self.active_pane().is_some_and(|p| p.in_trash()) {
            return;
        }
        let files: Vec<gio::File> = sel.iter().map(|i| i.target()).collect();
        let uris: Vec<String> = files.iter().map(|f| f.uri().to_string()).collect();
        let paths: Vec<String> = files.iter().map(|f| f.path().map(|p| p.display().to_string()).unwrap_or_else(|| f.uri().to_string())).collect();
        // Several formats so Nautilus, Dolphin, Thunar and editors all understand it.
        let gnome = format!("{}\n{}", if cut { "cut" } else { "copy" }, uris.join("\n"));
        let uri_list = format!("{}\r\n", uris.join("\r\n"));
        let providers = [
            gdk::ContentProvider::for_value(&gdk::FileList::from_array(&files).to_value()),
            gdk::ContentProvider::for_bytes("x-special/gnome-copied-files", &glib::Bytes::from_owned(gnome.into_bytes())),
            gdk::ContentProvider::for_bytes("text/uri-list", &glib::Bytes::from_owned(uri_list.into_bytes())),
            gdk::ContentProvider::for_bytes("application/x-kde-cutselection", &glib::Bytes::from_static(if cut { b"1" } else { b"0" })),
            gdk::ContentProvider::for_bytes("text/plain;charset=utf-8", &glib::Bytes::from_owned(paths.join("\n").into_bytes())),
        ];
        self.gtk_window().clipboard().set_content(Some(&gdk::ContentProvider::new_union(&providers))).ok();
        let what = if cut { "Cut" } else { "Copied" };
        self.toast(&format!("{what} {}. Paste with Ctrl+V.", count(sel.len())));
    }

    fn op_paste(&self, into: Option<gio::File>) {
        let dest = match into.and_then(|f| f.path()) {
            Some(d) => d,
            None => match self.current_dir() {
                Some(d) => d,
                None => return,
            },
        };
        let cb = self.gtk_window().clipboard();
        let weak = self.downgrade();
        glib::MainContext::default().spawn_local(async move {
            let parsed = read_clipboard_files(&cb).await;
            if std::env::var_os("KIVO_DEBUG_TIMING").is_some() {
                eprintln!("paste: formats={} local={} parsed={:?}", cb.formats().to_str(), cb.is_local(), parsed.as_ref().map(|(f, c)| (f.len(), *c)));
            }
            let Some(w) = Window::from_weak(&weak) else { return };
            let Some((files, cut)) = parsed else {
                w.toast("There's nothing to paste. Copy or cut some files first.");
                return;
            };
            let paths: Vec<PathBuf> = files.iter().filter_map(|f| f.path()).collect();
            if paths.len() != files.len() {
                w.toast("Some copied items aren't local files and can't be pasted yet.");
            }
            if paths.is_empty() {
                return;
            }
            let mode = if cut { Mode::Move } else { Mode::Copy };
            let cb2 = cb.clone();
            w.start_transfer(mode, paths, dest, move || {
                if cut {
                    // Cut items have moved; don't paste them twice.
                    cb2.set_content(None::<&gdk::ContentProvider>).ok();
                }
            });
        });
    }

    /// Copy or move in the background, then select the results.
    pub(crate) fn start_transfer(&self, mode: Mode, sources: Vec<PathBuf>, dest: PathBuf, on_success: impl FnOnce() + 'static) {
        let verb = if mode == Mode::Copy { "Copying" } else { "Moving" };
        let title = format!("{verb} {} to “{}”", count(sources.len()), name_of(&dest));
        let weak = self.downgrade();
        let d = dest.clone();
        // Journal the destination so a crash can't leave hidden partial files.
        let journal = ops::journal::file();
        ops::journal::add(&journal, &dest);
        let jd = dest.clone();
        self.jobs().run(
            &self.parent_widget(),
            &title,
            move |obs, cancel| ops::transfer(mode, &sources, &d, obs, cancel),
            move |report| {
                ops::journal::remove(&ops::journal::file(), &jd);
                let Some(w) = Window::from_weak(&weak) else { return };
                let created: Vec<PathBuf> = report.done.iter().map(|(_, d)| d.clone()).collect();
                if !report.done.is_empty() {
                    record(Action::Transferred { mode, pairs: report.done });
                    w.select_results(&created);
                }
                if report.errors.is_empty() && !report.cancelled {
                    on_success();
                }
                let what = if mode == Mode::Copy { "Copied" } else { "Moved" };
                if report.cancelled {
                    w.toast(&format!("{verb} was cancelled. {} finished; nothing was left half-done.", count(created.len())));
                } else if !created.is_empty() && report.errors.is_empty() {
                    w.toast_undo(&format!("{what} {} to “{}”", count(created.len()), name_of(&dest)));
                }
                let title = if mode == Mode::Copy { "Some Items Couldn't Be Copied" } else { "Some Items Couldn't Be Moved" };
                crate::dialogs::show_errors(&w.parent_widget(), title, &report.errors);
            },
        );
    }

    // -------------------------------------------------------------- trash

    fn op_trash(&self) {
        if self.active_pane().is_some_and(|p| p.in_trash()) {
            self.op_delete();
            return;
        }
        let Some(paths) = self.selected_paths() else { return };
        let weak = self.downgrade();
        let title = format!("Moving {} to the Trash", count(paths.len()));
        self.jobs().run(
            &self.parent_widget(),
            &title,
            move |_, cancel| {
                let t = trash();
                let mut done: Vec<TrashedItem> = Vec::new();
                let mut failed: Vec<(PathBuf, OpError)> = Vec::new();
                for p in &paths {
                    if cancel.is_cancelled() {
                        break;
                    }
                    match t.trash(p) {
                        Ok(i) => done.push(i),
                        Err(e) => failed.push((p.clone(), e)),
                    }
                }
                (done, failed)
            },
            move |(done, failed)| {
                let Some(w) = Window::from_weak(&weak) else { return };
                if !done.is_empty() {
                    let n = done.len();
                    record(Action::Trashed(done));
                    w.toast_undo(&format!("Moved {} to the Trash", count(n)));
                }
                if failed.is_empty() {
                    return;
                }
                // Some drives have no trash: offer permanent deletion instead.
                let no_trash: Vec<PathBuf> = failed.iter().filter(|(_, e)| e.hint.contains("delete it permanently")).map(|(p, _)| p.clone()).collect();
                let other: Vec<OpError> = failed.iter().filter(|(_, e)| !e.hint.contains("delete it permanently")).map(|(_, e)| e.clone()).collect();
                crate::dialogs::show_errors(&w.parent_widget(), "Some Items Couldn't Be Moved to the Trash", &other);
                if !no_trash.is_empty() {
                    let weak = w.downgrade();
                    glib::MainContext::default().spawn_local(async move {
                        let Some(w) = Window::from_weak(&weak) else { return };
                        let body = format!(
                            "{} can't be moved to the trash because the drive doesn't support it. Delete permanently instead? This can't be undone.",
                            match no_trash.as_slice() {
                                [one] => format!("“{}”", name_of(one)),
                                many => count(many.len()),
                            }
                        );
                        if crate::dialogs::confirm(&w.parent_widget(), "Delete Permanently?", &body, "_Delete Permanently").await {
                            w.run_delete(no_trash);
                        }
                    });
                }
            },
        );
    }

    fn op_delete(&self) {
        let pane = self.active_pane();
        if pane.as_ref().is_some_and(|p| p.in_trash()) {
            // In the trash: purge the selected trashed items.
            let sel = self.selection();
            let items: Vec<(PathBuf, Option<String>)> = sel.iter().filter_map(|i| i.trash_info()).collect();
            if items.is_empty() {
                return;
            }
            let names: Vec<String> = sel.iter().map(|i| i.name()).collect();
            let weak = self.downgrade();
            glib::MainContext::default().spawn_local(async move {
                let Some(w) = Window::from_weak(&weak) else { return };
                if !crate::dialogs::confirm_delete(&w.parent_widget(), &names).await {
                    return;
                }
                let weak = w.downgrade();
                w.jobs().run(
                    &w.parent_widget(),
                    "Deleting from the Trash",
                    move |_, _| {
                        let t = trash();
                        items
                            .iter()
                            .filter_map(|(orig, date)| t.find(orig, date.as_deref()))
                            .filter_map(|i| t.purge(&i).err())
                            .collect::<Vec<_>>()
                    },
                    move |errors| {
                        if let Some(w) = Window::from_weak(&weak) {
                            crate::dialogs::show_errors(&w.parent_widget(), "Some Items Couldn't Be Deleted", &errors);
                        }
                    },
                );
            });
            return;
        }
        let Some(paths) = self.selected_paths() else { return };
        let names: Vec<String> = paths.iter().map(|p| name_of(p)).collect();
        let weak = self.downgrade();
        glib::MainContext::default().spawn_local(async move {
            let Some(w) = Window::from_weak(&weak) else { return };
            if crate::dialogs::confirm_delete(&w.parent_widget(), &names).await {
                w.run_delete(paths);
            }
        });
    }

    fn run_delete(&self, paths: Vec<PathBuf>) {
        let weak = self.downgrade();
        let title = format!("Deleting {}", count(paths.len()));
        self.jobs().run(
            &self.parent_widget(),
            &title,
            move |obs, cancel| ops::delete(&paths, obs, cancel),
            move |report| {
                let Some(w) = Window::from_weak(&weak) else { return };
                if !report.done.is_empty() && report.errors.is_empty() {
                    w.toast(&format!("Deleted {} permanently", count(report.done.len())));
                }
                crate::dialogs::show_errors(&w.parent_widget(), "Some Items Couldn't Be Deleted", &report.errors);
            },
        );
    }

    fn op_restore(&self) {
        let infos: Vec<(PathBuf, Option<String>)> = self.selection().iter().filter_map(|i| i.trash_info()).collect();
        if infos.is_empty() {
            return;
        }
        let weak = self.downgrade();
        self.jobs().run(
            &self.parent_widget(),
            "Restoring from the Trash",
            move |_, _| {
                let t = trash();
                let mut restored = Vec::new();
                let mut errors = Vec::new();
                for (orig, date) in infos {
                    match t.find(&orig, date.as_deref()) {
                        Some(item) => match t.restore(&item) {
                            Ok(p) => restored.push(p),
                            Err(e) => errors.push(e),
                        },
                        None => errors.push(OpError::new(&orig, format!("“{}” is no longer in the trash.", name_of(&orig)), "It may have been restored or deleted already.")),
                    }
                }
                (restored, errors)
            },
            move |(restored, errors): (Vec<PathBuf>, Vec<OpError>)| {
                let Some(w) = Window::from_weak(&weak) else { return };
                if let [one] = restored.as_slice() {
                    w.toast(&format!("Restored “{}” to {}", name_of(one), one.parent().map(|p| kivo_core::paths::display(p, &kivo_core::paths::home())).unwrap_or_default()));
                } else if !restored.is_empty() {
                    w.toast(&format!("Restored {}", count(restored.len())));
                }
                crate::dialogs::show_errors(&w.parent_widget(), "Some Items Couldn't Be Restored", &errors);
            },
        );
    }

    fn op_empty_trash(&self) {
        let weak = self.downgrade();
        glib::MainContext::default().spawn_local(async move {
            let Some(w) = Window::from_weak(&weak) else { return };
            let n = trash().list().len();
            if n == 0 {
                w.toast("The trash is already empty.");
                return;
            }
            let ok = crate::dialogs::confirm(
                &w.parent_widget(),
                "Empty the Trash?",
                &format!("All {} in the trash will be deleted permanently. This can't be undone.", count(n)),
                "_Empty Trash",
            )
            .await;
            if !ok {
                return;
            }
            let weak = w.downgrade();
            w.jobs().run(
                &w.parent_widget(),
                "Emptying the Trash",
                move |_, cancel| {
                    let t = trash();
                    let mut errors = Vec::new();
                    for item in t.list() {
                        if cancel.is_cancelled() {
                            break;
                        }
                        if let Err(e) = t.purge(&item) {
                            errors.push(e);
                        }
                    }
                    errors
                },
                move |errors| {
                    if let Some(w) = Window::from_weak(&weak) {
                        if errors.is_empty() {
                            w.toast("The trash is empty");
                        }
                        crate::dialogs::show_errors(&w.parent_widget(), "Some Items Couldn't Be Deleted", &errors);
                    }
                },
            );
        });
    }

    // ------------------------------------------------------------- rename

    fn op_rename(&self) {
        let Some(paths) = self.selected_paths() else { return };
        let sel = self.selection();
        let weak = self.downgrade();
        if paths.len() == 1 {
            let path = paths[0].clone();
            let is_dir = sel[0].is_dir();
            let current = name_of(&path);
            glib::MainContext::default().spawn_local(async move {
                let Some(w) = Window::from_weak(&weak) else { return };
                let parent = path.parent().unwrap_or(Path::new("/")).to_path_buf();
                let cur = current.clone();
                let validate = move |n: &str| {
                    let taken = n != cur && n.to_lowercase() != cur.to_lowercase() && parent.join(n).symlink_metadata().is_ok();
                    taken.then(|| format!("An item named “{n}” already exists here."))
                };
                let Some(new) = crate::dialogs::ask_name(&w.parent_widget(), "Rename", "_Rename", &current, is_dir, false, validate).await else { return };
                match ops::rename(&path, &new) {
                    Ok(to) => {
                        record(Action::Renamed(vec![(path, to.clone())]));
                        w.select_results(&[to]);
                    }
                    Err(e) => crate::dialogs::show_errors(&w.parent_widget(), "Couldn't Rename", &[e]),
                }
            });
        } else {
            let current: Vec<String> = paths.iter().map(|p| name_of(p)).collect();
            glib::MainContext::default().spawn_local(async move {
                let Some(w) = Window::from_weak(&weak) else { return };
                let Some(new) = crate::dialogs::ask_batch_rename(&w.parent_widget(), current).await else { return };
                let items: Vec<(PathBuf, String)> = paths.into_iter().zip(new).collect();
                match ops::rename_many(&items) {
                    Ok(done) => {
                        let finals: Vec<PathBuf> = done.iter().map(|(_, n)| n.clone()).collect();
                        record(Action::Renamed(done));
                        w.select_results(&finals);
                        w.toast_undo(&format!("Renamed {}", count(finals.len())));
                    }
                    Err(e) => crate::dialogs::show_errors(&w.parent_widget(), "Couldn't Rename", &[e]),
                }
            });
        }
    }

    // ------------------------------------------------------------- create

    fn op_duplicate(&self) {
        let Some(paths) = self.selected_paths() else { return };
        // Each item is duplicated beside itself.
        let Some(dir) = paths[0].parent().map(Path::to_path_buf) else { return };
        if paths.iter().any(|p| p.parent() != Some(dir.as_path())) {
            return;
        }
        self.start_transfer(Mode::Copy, paths, dir, || {});
    }

    fn op_link(&self) {
        let Some(paths) = self.selected_paths() else { return };
        let Some(dir) = self.current_dir() else { return };
        let mut made = Vec::new();
        let mut errors = Vec::new();
        for p in &paths {
            let name = names::unique_name(&dir, &format!("Link to {}", name_of(p)), &|_| false);
            match ops::create_link(p, &dir, &name) {
                Ok(l) => made.push(l),
                Err(e) => errors.push(e),
            }
        }
        if !made.is_empty() {
            self.select_results(&made);
            record(Action::Created(made));
        }
        crate::dialogs::show_errors(&self.parent_widget(), "Couldn't Create Link", &errors);
    }

    fn op_new_folder(&self) {
        let Some(dir) = self.current_dir() else { return };
        if !self.active_pane().is_some_and(|p| p.is_writable()) {
            self.toast("You don't have permission to create items in this folder.");
            return;
        }
        let weak = self.downgrade();
        glib::MainContext::default().spawn_local(async move {
            let Some(w) = Window::from_weak(&weak) else { return };
            let initial = names::unique_name(&dir, "New Folder", &|_| false);
            let d = dir.clone();
            let validate = move |n: &str| d.join(n).symlink_metadata().is_ok().then(|| format!("An item named “{n}” already exists here."));
            let Some(name) = crate::dialogs::ask_name(&w.parent_widget(), "New Folder", "_Create", &initial, true, true, validate).await else { return };
            match ops::create_folder(&dir, &name) {
                Ok(p) => {
                    w.select_results(std::slice::from_ref(&p));
                    record(Action::Created(vec![p]));
                }
                Err(e) => crate::dialogs::show_errors(&w.parent_widget(), "Couldn't Create Folder", &[e]),
            }
        });
    }

    fn op_new_file(&self, template: &str) {
        let Some(dir) = self.current_dir() else { return };
        let tpl = (!template.is_empty())
            .then(|| UserDirs::load().get(UserDir::Templates).map(|t| t.join(template)))
            .flatten();
        let base = if template.is_empty() { "New File".to_string() } else { template.to_string() };
        let weak = self.downgrade();
        glib::MainContext::default().spawn_local(async move {
            let Some(w) = Window::from_weak(&weak) else { return };
            let initial = names::unique_name(&dir, &base, &|_| false);
            let d = dir.clone();
            let validate = move |n: &str| d.join(n).symlink_metadata().is_ok().then(|| format!("An item named “{n}” already exists here."));
            let Some(name) = crate::dialogs::ask_name(&w.parent_widget(), "New Document", "_Create", &initial, false, true, validate).await else { return };
            match ops::create_file(&dir, &name, tpl.as_deref()) {
                Ok(p) => {
                    w.select_results(std::slice::from_ref(&p));
                    record(Action::Created(vec![p]));
                }
                Err(e) => crate::dialogs::show_errors(&w.parent_widget(), "Couldn't Create Document", &[e]),
            }
        });
    }

    fn op_copy_path(&self) {
        let sel = self.selection();
        let files: Vec<gio::File> = if sel.is_empty() {
            self.active_pane().map(|p| vec![p.location()]).unwrap_or_default()
        } else {
            sel.iter().map(|i| i.target()).collect()
        };
        let text: Vec<String> = files.iter().map(|f| f.path().map(|p| p.display().to_string()).unwrap_or_else(|| f.uri().to_string())).collect();
        self.gtk_window().clipboard().set_text(&text.join("\n"));
        self.toast(if text.len() == 1 { "Copied the path" } else { "Copied the paths" });
    }

    fn folder_properties(&self) {
        let Some(pane) = self.active_pane() else { return };
        let loc = pane.location();
        let weak = self.downgrade();
        glib::MainContext::default().spawn_local(async move {
            let info = loc.query_info_future(crate::item::ATTRS, gio::FileQueryInfoFlags::NONE, glib::Priority::DEFAULT).await;
            let (Some(w), Ok(info)) = (Window::from_weak(&weak), info) else { return };
            let item = FileItem::from_info(loc, info);
            crate::properties::show(&w.gtk_window().upcast(), &[item]);
        });
    }

    // ------------------------------------------------------------ archives

    /// Extract next to the archive, asking for a password only if needed.
    pub(crate) fn extract_archive(&self, archive: PathBuf, password: Option<String>) {
        use kivo_core::archive::{self, ArchiveError, Limits};
        let name = name_of(&archive);
        let weak = self.downgrade();
        let a = archive.clone();
        let pw = password.clone();
        self.jobs().run(
            &self.parent_widget(),
            &format!("Extracting “{name}”"),
            move |obs, cancel| archive::extract(&a, pw.as_deref(), Limits::default(), obs, cancel),
            move |result| {
                let Some(w) = Window::from_weak(&weak) else { return };
                match result {
                    Ok(done) => {
                        w.select_results(std::slice::from_ref(&done.path));
                        let mut text = format!("Extracted “{}”", name_of(&done.path));
                        if done.skipped_links > 0 {
                            text.push_str(&format!(" · {} unsafe link{} left out", done.skipped_links, if done.skipped_links == 1 { "" } else { "s" }));
                        }
                        record(Action::Created(vec![done.path]));
                        w.toast_undo(&text);
                    }
                    Err(e @ (ArchiveError::NeedPassword | ArchiveError::WrongPassword)) => {
                        let wrong = e == ArchiveError::WrongPassword;
                        let weak = w.downgrade();
                        glib::MainContext::default().spawn_local(async move {
                            let Some(w) = Window::from_weak(&weak) else { return };
                            if let Some(pw) = crate::dialogs::ask_password(&w.parent_widget(), &name, wrong).await {
                                w.extract_archive(archive, Some(pw));
                            }
                        });
                    }
                    Err(ArchiveError::Cancelled) => w.toast("Extraction cancelled. Nothing was left behind."),
                    Err(e) => {
                        let err = OpError { path: archive.clone(), message: e.message(&name), hint: e.hint() };
                        crate::dialogs::show_errors(&w.parent_widget(), "Couldn't Extract", &[err]);
                    }
                }
            },
        );
    }

    fn op_compress(&self) {
        let Some(paths) = self.selected_paths() else { return };
        let Some(dir) = paths[0].parent().map(Path::to_path_buf) else { return };
        let base = match paths.as_slice() {
            [one] => {
                let n = name_of(one);
                if one.is_dir() { n } else { names::split_ext(&n).0.to_string() }
            }
            _ => "Archive".to_string(),
        };
        let weak = self.downgrade();
        glib::MainContext::default().spawn_local(async move {
            let Some(w) = Window::from_weak(&weak) else { return };
            let Some(choice) = crate::dialogs::ask_compress(&w.parent_widget(), &base, dir.clone()).await else { return };
            let weak = w.downgrade();
            let title = format!("Compressing {}", count(paths.len()));
            let name = choice.name.clone();
            w.jobs().run(
                &w.parent_widget(),
                &title,
                move |obs, cancel| {
                    kivo_core::archive::compress(&paths, &dir, &choice.name, choice.format, choice.password.as_deref(), obs, cancel)
                },
                move |result| {
                    let Some(w) = Window::from_weak(&weak) else { return };
                    match result {
                        Ok(p) => {
                            w.select_results(std::slice::from_ref(&p));
                            w.toast_undo(&format!("Created “{}”", name_of(&p)));
                            record(Action::Created(vec![p]));
                        }
                        Err(kivo_core::archive::ArchiveError::Cancelled) => w.toast("Compression cancelled. Nothing was left behind."),
                        Err(e) => {
                            let err = OpError { path: PathBuf::from(&name), message: e.message(&name), hint: e.hint() };
                            crate::dialogs::show_errors(&w.parent_widget(), "Couldn't Compress", &[err]);
                        }
                    }
                },
            );
        });
    }

    // --------------------------------------------------------------- drop

    pub(crate) fn handle_drop(&self, files: Vec<gio::File>, target: gio::File, action: gdk::DragAction) {
        let Some(dest) = target.path() else {
            self.toast("Items can't be dropped here.");
            return;
        };
        let paths: Vec<PathBuf> = files.iter().filter_map(|f| f.path()).collect();
        if paths.len() != files.len() {
            self.toast("Some dropped items aren't local files and were skipped.");
        }
        if paths.is_empty() {
            return;
        }
        if action == gdk::DragAction::LINK {
            let mut made = Vec::new();
            let mut errors = Vec::new();
            for p in &paths {
                let name = names::unique_name(&dest, &name_of(p), &|_| false);
                match ops::create_link(p, &dest, &name) {
                    Ok(l) => made.push(l),
                    Err(e) => errors.push(e),
                }
            }
            if !made.is_empty() {
                self.select_results(&made);
                record(Action::Created(made));
            }
            crate::dialogs::show_errors(&self.parent_widget(), "Couldn't Create Links", &errors);
            return;
        }
        let mode = if action == gdk::DragAction::MOVE { Mode::Move } else { Mode::Copy };
        self.start_transfer(mode, paths, dest, || {});
    }

    // --------------------------------------------------------------- undo

    fn op_undo(&self) {
        let Some(action) = HISTORY.with(|h| h.borrow_mut().pop()) else {
            self.toast("There's nothing to undo.");
            return;
        };
        let label = action.label();
        let weak = self.downgrade();
        self.jobs().run(
            &self.parent_widget(),
            &format!("Undoing {label}"),
            move |_, _| undo::undo(&action, &trash()),
            move |result| {
                let Some(w) = Window::from_weak(&weak) else { return };
                match result {
                    Ok(restored) => {
                        w.select_results(&restored);
                        w.toast(&format!("Undid {label}"));
                    }
                    Err(errors) => crate::dialogs::show_errors(&w.parent_widget(), &format!("Couldn't Fully Undo {label}"), &errors),
                }
            },
        );
    }
}

/// Files on the clipboard and whether they were cut. Understands GNOME,
/// KDE and plain uri-list clipboards.
async fn read_clipboard_files(cb: &gdk::Clipboard) -> Option<(Vec<gio::File>, bool)> {
    let formats = cb.formats();
    if formats.contain_mime_type("x-special/gnome-copied-files")
        && let Some(text) = read_mime(cb, "x-special/gnome-copied-files").await {
            let mut lines = text.lines();
            let cut = lines.next().map(str::trim) == Some("cut");
            let files: Vec<gio::File> = lines.filter(|l| !l.trim().is_empty()).map(|l| gio::File::for_uri(l.trim())).collect();
            if !files.is_empty() {
                return Some((files, cut));
            }
        }
    let cut = if formats.contain_mime_type("application/x-kde-cutselection") {
        read_mime(cb, "application/x-kde-cutselection").await.is_some_and(|t| t.trim() == "1")
    } else {
        false
    };
    if formats.contains_type(gdk::FileList::static_type())
        && let Ok(v) = cb.read_value_future(gdk::FileList::static_type(), glib::Priority::DEFAULT).await
            && let Ok(list) = v.get::<gdk::FileList>() {
                let files = list.files();
                if !files.is_empty() {
                    return Some((files, cut));
                }
            }
    if formats.contain_mime_type("text/uri-list")
        && let Some(text) = read_mime(cb, "text/uri-list").await {
            let files: Vec<gio::File> = text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(gio::File::for_uri)
                .collect();
            if !files.is_empty() {
                return Some((files, cut));
            }
        }
    None
}

async fn read_mime(cb: &gdk::Clipboard, mime: &str) -> Option<String> {
    let (stream, _) = cb.read_future(&[mime], glib::Priority::DEFAULT).await.ok()?;
    let mut out = Vec::new();
    loop {
        let bytes = stream.read_bytes_future(65536, glib::Priority::DEFAULT).await.ok()?;
        if bytes.is_empty() {
            break;
        }
        out.extend_from_slice(&bytes);
    }
    String::from_utf8(out).ok()
}


/// Is this item an archive that a double-click should extract? Decided by
/// content (magic bytes), so renamed archives work and .docx files don't.
pub fn looks_like_archive(item: &FileItem) -> bool {
    use kivo_core::archive;
    let Some(p) = item.path() else { return false };
    if item.is_dir() {
        return false;
    }
    archive::is_extractable(&p, archive::detect(&p).ok().flatten())
}
