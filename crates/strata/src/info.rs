// SPDX-License-Identifier: GPL-3.0-or-later
//! Information panel: large icon, name and key facts about the selection, or
//! about the current folder when nothing is selected.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};
use strata_core::{format, paths};

use crate::item::{ATTRS, FileItem};

pub struct Inner {
    root: gtk::ScrolledWindow,
    image: gtk::Image,
    name: gtk::Label,
    kind: gtk::Label,
    grid: gtk::Grid,
    task: RefCell<Option<glib::JoinHandle<()>>>,
}

#[derive(Clone)]
pub struct InfoPanel(Rc<Inner>);

impl InfoPanel {
    pub fn new() -> Self {
        let image = gtk::Image::builder().pixel_size(128).css_classes(["info-icon"]).build();
        let name = gtk::Label::builder()
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .justify(gtk::Justification::Center)
            .selectable(true)
            .css_classes(["info-name"])
            .build();
        let kind = gtk::Label::builder().css_classes(["info-kind"]).build();
        let heading = gtk::Label::builder().label("Information").xalign(0.0).css_classes(["info-heading"]).build();
        let grid = gtk::Grid::builder().row_spacing(10).column_spacing(16).css_classes(["info-grid"]).build();
        let b = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .css_classes(["info-panel"])
            .build();
        b.append(&image);
        b.append(&name);
        b.append(&kind);
        b.append(&gtk::Separator::builder().margin_top(16).margin_bottom(12).build());
        b.append(&heading);
        b.append(&grid);
        let root = gtk::ScrolledWindow::builder()
            .child(&b)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .width_request(260)
            .build();
        root.update_property(&[gtk::accessible::Property::Label("Information")]);
        InfoPanel(Rc::new(Inner { root, image, name, kind, grid, task: RefCell::new(None) }))
    }

    pub fn widget(&self) -> gtk::Widget {
        self.0.root.clone().upcast()
    }

    fn clear_rows(&self) {
        while let Some(c) = self.0.grid.first_child() {
            self.0.grid.remove(&c);
        }
    }

    fn row(&self, key: &str, value: &str) {
        let r = {
            let mut n = 0;
            let mut c = self.0.grid.first_child();
            while let Some(w) = c {
                n += 1;
                c = w.next_sibling();
            }
            n / 2
        };
        let k = gtk::Label::builder().label(key).xalign(0.0).yalign(0.0).css_classes(["info-key"]).build();
        let v = gtk::Label::builder()
            .label(value)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .selectable(true)
            .hexpand(true)
            .css_classes(["info-value"])
            .build();
        self.0.grid.attach(&k, 0, r, 1, 1);
        self.0.grid.attach(&v, 1, r, 1, 1);
    }

    /// Show the selection, or the folder itself when nothing is selected.
    pub fn show(&self, selection: &[FileItem], folder: &gio::File) {
        if let Some(t) = self.0.task.borrow_mut().take() {
            t.abort();
        }
        self.clear_rows();
        match selection {
            [] => self.show_folder(folder),
            [one] => self.show_item(one),
            many => self.show_many(many),
        }
    }

    fn show_item(&self, item: &FileItem) {
        let meta = item.meta().clone();
        self.0.image.set_from_gicon(&item.icon());
        self.0.name.set_label(&meta.name);
        self.0.kind.set_label(if meta.type_name.is_empty() { "Unknown" } else { &meta.type_name });
        if item.is_dir() {
            self.row("Contains", "Counting…");
            self.count_children(item.target());
        } else {
            self.row("Size", &format!("{} ({} bytes)", format::size(meta.size), group_digits(meta.size)));
        }
        self.row("Modified", &format::date_full(meta.modified));
        if let Some(c) = item.created() {
            self.row("Created", &format::date_full(c));
        }
        if let Some(t) = item.link_target() {
            self.row("Link to", &t);
        }
        if let Some(parent) = item.file().parent() {
            self.row("Where", &where_text(&parent));
        }
    }

    fn show_many(&self, items: &[FileItem]) {
        self.0.image.set_from_gicon(&items[0].icon());
        self.0.name.set_label(&format!("{} items selected", items.len()));
        let dirs = items.iter().filter(|i| i.is_dir()).count();
        let files = items.len() - dirs;
        self.0.kind.set_label(&match (dirs, files) {
            (0, f) => format!("{f} files"),
            (d, 0) => format!("{d} folders"),
            (d, f) => format!("{d} folders, {f} files"),
        });
        let bytes: u64 = items.iter().filter(|i| !i.is_dir()).map(|i| i.meta().size).sum();
        self.row("Size", &if dirs > 0 { format!("{} (files only)", format::size(bytes)) } else { format::size(bytes) });
    }

    fn show_folder(&self, folder: &gio::File) {
        self.0.image.set_icon_name(Some("folder"));
        self.0.name.set_label(&crate::pane::folder_title(folder));
        self.0.kind.set_label("Folder");
        let weak = Rc::downgrade(&self.0);
        let folder = folder.clone();
        let task = glib::MainContext::default().spawn_local(async move {
            let info = folder
                .query_info_future(ATTRS, gio::FileQueryInfoFlags::NONE, glib::Priority::LOW)
                .await;
            let Some(inner) = weak.upgrade() else { return };
            let panel = InfoPanel(inner);
            if let Ok(info) = info {
                if let Some(icon) = info.icon() {
                    panel.0.image.set_from_gicon(&icon);
                }
                if let Some(m) = info.modification_date_time() {
                    panel.row("Modified", &format::date_full(m.to_unix()));
                }
            }
            if let Some(parent) = folder.parent() {
                panel.row("Where", &where_text(&parent));
            }
        });
        *self.0.task.borrow_mut() = Some(task);
    }

    fn count_children(&self, dir: gio::File) {
        let weak = Rc::downgrade(&self.0);
        let task = glib::MainContext::default().spawn_local(async move {
            let mut count = 0u64;
            if let Ok(en) = dir
                .enumerate_children_future("standard::name", gio::FileQueryInfoFlags::NONE, glib::Priority::LOW)
                .await
            {
                while let Ok(batch) = en.next_files_future(1000, glib::Priority::LOW).await {
                    if batch.is_empty() {
                        break;
                    }
                    count += batch.len() as u64;
                }
            }
            let Some(inner) = weak.upgrade() else { return };
            // Replace the "Counting…" value in place (row 0, column 1).
            if let Some(v) = inner.grid.child_at(1, 0).and_downcast::<gtk::Label>() {
                v.set_label(&format::item_count(count));
            }
        });
        *self.0.task.borrow_mut() = Some(task);
    }
}

fn where_text(dir: &gio::File) -> String {
    match dir.path() {
        Some(p) => paths::display(&p, &paths::home()),
        None => dir.uri().to_string(),
    }
}

fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}
