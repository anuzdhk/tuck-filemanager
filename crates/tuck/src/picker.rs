// SPDX-License-Identifier: GPL-3.0-or-later
//! A Tuck window as another app's file picker: the usual window with a bar
//! along the bottom (Cancel, a name field when saving, Open or Save).
//! Opening a file (double-click, Enter) picks it. The answer goes back
//! through `done` once: the chosen URIs, or None when cancelled.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::item::FileItem;
use crate::window::Window;

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Open { multiple: bool, directory: bool },
    Save { name: String },
    /// Save several files into a folder the user picks.
    SaveFiles { names: Vec<String> },
}

#[derive(Debug, Clone)]
pub struct Request {
    pub mode: Mode,
    pub title: String,
    pub accept_label: Option<String>,
    pub folder: Option<PathBuf>,
}

type Done = Box<dyn FnOnce(Option<Vec<String>>)>;

pub struct Picker {
    mode: Mode,
    done: RefCell<Option<Done>>,
    accept: gtk::Button,
    name: gtk::Entry,
}

/// Open a picker window for `req`.
pub fn open(app: &adw::Application, folder_views: Rc<RefCell<tuck_core::settings::FolderViews>>, req: Request, done: impl FnOnce(Option<Vec<String>>) + 'static) -> Window {
    let w = Window::new(app, folder_views);
    let start = req.folder.clone().filter(|p| p.is_dir()).unwrap_or_else(tuck_core::paths::home);
    w.open_tab(gio::File::for_path(start), true);

    let (verb, default_title) = match &req.mode {
        Mode::Open { directory: true, .. } => ("_Select", "Select a Folder"),
        Mode::Open { multiple: true, .. } => ("_Open", "Open Files"),
        Mode::Open { .. } => ("_Open", "Open a File"),
        Mode::Save { .. } => ("_Save", "Save File"),
        Mode::SaveFiles { .. } => ("_Save Here", "Save Files"),
    };
    let label = req.accept_label.clone().filter(|l| !l.is_empty()).unwrap_or_else(|| verb.to_string());
    let accept = gtk::Button::builder().label(&label).use_underline(true).css_classes(["suggested-action"]).build();
    let cancel = gtk::Button::builder().label("_Cancel").use_underline(true).build();
    let name = gtk::Entry::builder().hexpand(true).activates_default(true).placeholder_text("File name").build();
    name.update_property(&[gtk::accessible::Property::Label("File name")]);
    let bar = gtk::Box::builder().spacing(8).css_classes(["toolbar", "picker-bar"]).build();
    bar.append(&cancel);
    match &req.mode {
        Mode::Save { name: n } => {
            name.set_text(n);
            bar.append(&name);
        }
        _ => {
            let hint = match &req.mode {
                Mode::Open { directory: true, .. } => "Pick a folder, or open one and select nothing",
                Mode::SaveFiles { .. } => "Open the folder to save into",
                Mode::Open { multiple: true, .. } => "Select one or more files",
                _ => "Select a file, or double-click it",
            };
            bar.append(&gtk::Label::builder().label(hint).hexpand(true).xalign(0.0).css_classes(["dim-label"]).build());
        }
    }
    bar.append(&accept);

    let picker = Rc::new(Picker { mode: req.mode.clone(), done: RefCell::new(Some(Box::new(done))), accept: accept.clone(), name: name.clone() });
    w.set_picker(picker.clone(), bar.upcast_ref());
    let gw = w.gtk_window();
    gw.set_title(Some(if req.title.is_empty() { default_title } else { &req.title }));
    gw.set_default_widget(Some(&accept));

    let weak = w.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(w) = Window::from_weak(&weak) {
            w.gtk_window().close();
        }
    });
    let (weak, pk) = (w.downgrade(), picker.clone());
    accept.connect_clicked(move |_| {
        if let Some(w) = Window::from_weak(&weak) {
            pk.accept(&w);
        }
    });
    let (weak, pk) = (w.downgrade(), picker.clone());
    name.connect_changed(move |_| {
        if let Some(w) = Window::from_weak(&weak) {
            pk.sync(&w);
        }
    });
    picker.sync(&w);
    w.present();
    if matches!(req.mode, Mode::Save { .. }) {
        // Ready to type, with the name selected up to its extension.
        let stem = tuck_core::names::split_ext(&name.text()).0.chars().count() as i32;
        name.grab_focus();
        name.select_region(0, stem);
    }
    w
}

fn uri(p: &std::path::Path) -> String {
    gio::File::for_path(p).uri().to_string()
}

impl Picker {
    fn selection(&self, w: &Window) -> Vec<FileItem> {
        w.active_pane().map(|p| p.selected_items()).unwrap_or_default()
    }

    fn folder(&self, w: &Window) -> Option<PathBuf> {
        w.active_pane().and_then(|p| p.path())
    }

    /// Enable the button when pressing it would do something.
    pub fn sync(&self, w: &Window) {
        let sel = self.selection(w);
        let ok = match &self.mode {
            Mode::Open { directory: true, .. } => self.folder(w).is_some() || sel.iter().any(|i| i.is_dir()),
            Mode::Open { .. } => !sel.is_empty(),
            Mode::Save { .. } => tuck_core::names::validate(self.name.text().trim()).is_ok() && self.folder(w).is_some(),
            Mode::SaveFiles { .. } => self.folder(w).is_some(),
        };
        self.accept.set_sensitive(ok);
    }

    /// The window was closed without picking.
    pub fn cancel(&self) {
        if let Some(d) = self.done.borrow_mut().take() {
            d(None);
        }
    }

    fn finish(&self, w: &Window, uris: Vec<String>) {
        if let Some(d) = self.done.borrow_mut().take() {
            d(Some(uris));
        }
        w.gtk_window().close();
    }

    /// Double-click or Enter on files.
    pub fn activated(&self, w: &Window, files: Vec<FileItem>) {
        match &self.mode {
            Mode::Open { directory: false, multiple } => {
                let mut uris: Vec<String> = files.iter().map(|f| f.target().uri().to_string()).collect();
                if !multiple {
                    uris.truncate(1);
                }
                if !uris.is_empty() {
                    self.finish(w, uris);
                }
            }
            // Saving: a file's name becomes the new name (to replace it).
            Mode::Save { .. } => {
                if let Some(f) = files.first() {
                    self.name.set_text(&f.name());
                    self.name.grab_focus();
                }
            }
            _ => {}
        }
    }

    fn accept(&self, w: &Window) {
        let sel = self.selection(w);
        match &self.mode {
            Mode::Open { directory: false, multiple } => {
                // Only folders selected: go into the first, like double-click.
                if sel.iter().all(|i| i.is_dir()) {
                    if let (Some(d), Some(p)) = (sel.first(), w.active_pane()) {
                        p.navigate(d.target());
                    }
                    return;
                }
                let mut uris: Vec<String> = sel.iter().filter(|i| !i.is_dir()).map(|i| i.target().uri().to_string()).collect();
                if !multiple {
                    uris.truncate(1);
                }
                self.finish(w, uris);
            }
            Mode::Open { directory: true, multiple } => {
                let mut uris: Vec<String> = sel.iter().filter(|i| i.is_dir()).map(|i| i.target().uri().to_string()).collect();
                if uris.is_empty()
                    && let Some(p) = w.active_pane()
                {
                    uris.push(p.location().uri().to_string());
                }
                if !multiple {
                    uris.truncate(1);
                }
                self.finish(w, uris);
            }
            Mode::Save { .. } => {
                let Some(dir) = self.folder(w) else { return };
                let name = self.name.text().trim().to_string();
                let target = dir.join(&name);
                if target.is_dir() {
                    // Typed a folder's name: open it.
                    if let Some(p) = w.active_pane() {
                        p.navigate(gio::File::for_path(&target));
                    }
                    return;
                }
                self.confirm_then(w, vec![target]);
            }
            Mode::SaveFiles { names } => {
                let dir = match sel.as_slice() {
                    [one] if one.is_dir() => one.path(),
                    _ => self.folder(w),
                };
                let Some(dir) = dir else { return };
                self.confirm_then(w, names.iter().map(|n| dir.join(n)).collect());
            }
        }
    }

    /// Ask before replacing files that exist, then answer.
    fn confirm_then(&self, w: &Window, targets: Vec<PathBuf>) {
        let existing: Vec<String> = targets.iter().filter(|t| t.exists()).filter_map(|t| t.file_name()).map(|n| n.to_string_lossy().into_owned()).collect();
        let uris: Vec<String> = targets.iter().map(|t| uri(t)).collect();
        if existing.is_empty() {
            self.finish(w, uris);
            return;
        }
        let (heading, body) = match existing.as_slice() {
            [one] => (format!("Replace “{one}”?"), "A file with this name already exists. Replacing it overwrites its contents.".to_string()),
            more => (format!("Replace {} Files?", more.len()), format!("These already exist: {}.", more.join(", "))),
        };
        let weak = w.downgrade();
        let picker = w.picker();
        glib::MainContext::default().spawn_local(async move {
            let (Some(w), Some(pk)) = (Window::from_weak(&weak), picker) else { return };
            if crate::dialogs::confirm(&w.gtk_window().upcast(), &heading, &body, "_Replace").await {
                pk.finish(&w, uris);
            }
        });
    }
}
