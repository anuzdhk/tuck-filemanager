// SPDX-License-Identifier: GPL-3.0-or-later
//! Information panel: large icon, name and key facts about the selection, or
//! about the current folder when nothing is selected.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};
use tuck_core::{format, git, meta, paths};

use crate::item::{ATTRS, FileItem};

pub struct Inner {
    root: gtk::ScrolledWindow,
    image: gtk::Image,
    name: gtk::Label,
    kind: gtk::Label,
    grid: gtk::Grid,
    /// Rating, tags and comment of the shown file.
    meta_box: gtk::Box,
    git_task: RefCell<Option<glib::JoinHandle<()>>>,
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
        let meta_box = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).css_classes(["info-meta"]).build();
        b.append(&meta_box);
        let root = gtk::ScrolledWindow::builder()
            .child(&b)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .width_request(260)
            .build();
        root.update_property(&[gtk::accessible::Property::Label("Information")]);
        InfoPanel(Rc::new(Inner { root, image, name, kind, grid, meta_box, git_task: RefCell::new(None), task: RefCell::new(None) }))
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
        if let Some(t) = self.0.git_task.borrow_mut().take() {
            t.abort();
        }
        self.clear_rows();
        while let Some(c) = self.0.meta_box.first_child() {
            self.0.meta_box.remove(&c);
        }
        match selection {
            [] => self.show_folder(folder),
            [one] => self.show_item(one),
            many => self.show_many(many),
        }
        let single = match selection {
            [] => folder.path(),
            [one] => one.file().path(),
            _ => None,
        };
        if let Some(p) = single {
            self.show_meta(p.clone());
            self.show_git(p);
        }
    }

    // ------------------------------------------- tags, rating, comment

    fn heading(&self, text: &str) {
        let l = gtk::Label::builder().label(text).xalign(0.0).css_classes(["info-heading"]).margin_top(14).build();
        self.0.meta_box.append(&l);
    }

    fn show_meta(&self, path: PathBuf) {
        let m = match meta::read(&path) {
            Ok(m) => m,
            Err(e) if e.raw_os_error() == Some(libc::ENOTSUP) => {
                self.heading("Tags");
                let l = gtk::Label::builder()
                    .label("This drive can't store tags, ratings or comments.")
                    .xalign(0.0)
                    .wrap(true)
                    .css_classes(["info-key"])
                    .build();
                self.0.meta_box.append(&l);
                return;
            }
            Err(_) => return,
        };
        let path = Rc::new(path);
        let error = gtk::Label::builder().xalign(0.0).wrap(true).visible(false).css_classes(["error"]).build();
        let report = {
            let error = error.clone();
            move |r: std::io::Result<()>| match r {
                Ok(()) => error.set_visible(false),
                Err(e) => {
                    error.set_label(&format!("Couldn't save: {e}"));
                    error.set_visible(true);
                }
            }
        };

        // Rating: five stars, each worth 2 (Baloo counts half stars).
        self.heading("Rating");
        let stars = gtk::Box::builder().spacing(2).css_classes(["rating"]).build();
        stars.update_property(&[gtk::accessible::Property::Label("Rating")]);
        let rating = Rc::new(std::cell::Cell::new(m.rating));
        let buttons: Rc<RefCell<Vec<gtk::Button>>> = Rc::default();
        let paint = {
            let buttons = buttons.clone();
            move |r: u8| {
                for (i, b) in buttons.borrow().iter().enumerate() {
                    let on = (i as u8 + 1) * 2 <= r || (i as u8) * 2 + 1 == r;
                    b.set_icon_name(if on { "tuck-star-filled-symbolic" } else { "tuck-star-symbolic" });
                    if on { b.add_css_class("on") } else { b.remove_css_class("on") }
                }
            }
        };
        for n in 1..=5u8 {
            let b = gtk::Button::builder().css_classes(["flat", "star"]).tooltip_text(format!("{n} of 5")).build();
            b.update_property(&[gtk::accessible::Property::Label(&format!("Rate {n} of 5"))]);
            let (path, rating, paint, report) = (path.clone(), rating.clone(), paint.clone(), report.clone());
            b.connect_clicked(move |_| {
                // Clicking the current rating clears it.
                let r = if rating.get() == n * 2 { 0 } else { n * 2 };
                rating.set(r);
                paint(r);
                report(meta::write_rating(&path, r));
            });
            stars.append(&b);
            buttons.borrow_mut().push(b);
        }
        paint(m.rating);
        self.0.meta_box.append(&stars);

        // Tags: chips with a remove button, and an entry to add more.
        self.heading("Tags");
        let chips = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .column_spacing(6)
            .row_spacing(6)
            .max_children_per_line(8)
            .css_classes(["tag-chips"])
            .build();
        let tags = Rc::new(RefCell::new(m.tags.clone()));
        type Fill = Rc<RefCell<Option<Rc<dyn Fn()>>>>;
        let fill: Fill = Rc::default();
        {
            let (chips, tags, path, report, fill2) = (chips.clone(), tags.clone(), path.clone(), report.clone(), fill.clone());
            *fill.borrow_mut() = Some(Rc::new(move || {
                chips.remove_all();
                for t in tags.borrow().iter() {
                    let chip = gtk::Box::builder().spacing(2).css_classes(["tag-chip"]).build();
                    chip.append(&gtk::Label::new(Some(t)));
                    let x = gtk::Button::builder().icon_name("tuck-close-symbolic").css_classes(["flat", "circular"]).tooltip_text("Remove tag").build();
                    x.update_property(&[gtk::accessible::Property::Label(&format!("Remove tag {t}"))]);
                    let (t, tags, path, report, fill) = (t.clone(), tags.clone(), path.clone(), report.clone(), fill2.clone());
                    x.connect_clicked(move |_| {
                        tags.borrow_mut().retain(|x| *x != t);
                        report(meta::write_tags(&path, &tags.borrow()));
                        crate::search_ui::tags_changed(&path);
                        let f = fill.borrow().clone();
                        if let Some(f) = f {
                            glib::idle_add_local_once(move || f());
                        }
                    });
                    chip.append(&x);
                    chips.append(&chip);
                }
                chips.set_visible(!tags.borrow().is_empty());
            }));
        }
        let refill = fill.borrow().clone().expect("fill");
        refill();
        self.0.meta_box.append(&chips);
        let add = gtk::Entry::builder().placeholder_text("Add tags, separated by commas").build();
        add.update_property(&[gtk::accessible::Property::Label("Add tags")]);
        {
            let (tags, path, report) = (tags.clone(), path.clone(), report.clone());
            add.connect_activate(move |e| {
                let new = meta::parse_tags(&e.text());
                if new.is_empty() {
                    return;
                }
                tags.borrow_mut().extend(new);
                let merged = meta::parse_tags(&tags.borrow().join(","));
                *tags.borrow_mut() = merged;
                report(meta::write_tags(&path, &tags.borrow()));
                crate::search_ui::tags_changed(&path);
                e.set_text("");
                refill();
            });
        }
        self.0.meta_box.append(&add);

        // Comment: saved on Enter or when focus leaves.
        self.heading("Comment");
        let comment = gtk::Entry::builder().placeholder_text("Add a comment").text(&m.comment).build();
        comment.update_property(&[gtk::accessible::Property::Label("Comment")]);
        let saved = Rc::new(RefCell::new(m.comment.clone()));
        let save = {
            let (path, saved, report) = (path.clone(), saved.clone(), report.clone());
            move |e: &gtk::Entry| {
                let t = e.text().trim().to_string();
                if *saved.borrow() != t {
                    report(meta::write_comment(&path, &t));
                    *saved.borrow_mut() = t;
                }
            }
        };
        let s2 = save.clone();
        comment.connect_activate(move |e| s2(e));
        let focus = gtk::EventControllerFocus::new();
        let c2 = comment.clone();
        focus.connect_leave(move |_| save(&c2));
        comment.add_controller(focus);
        self.0.meta_box.append(&comment);
        self.0.meta_box.append(&error);
    }

    /// "Git: Modified · main", when the file is in a work tree.
    fn show_git(&self, path: PathBuf) {
        let weak = Rc::downgrade(&self.0);
        let task = glib::MainContext::default().spawn_local(async move {
            let dir = if path.is_dir() { path.clone() } else { path.parent().map(Path::to_path_buf).unwrap_or_default() };
            let Ok(Some(st)) = gio::spawn_blocking(move || git::status(&dir)).await else { return };
            let Some(inner) = weak.upgrade() else { return };
            let state = st.state_of(&path);
            let text = if st.branch.is_empty() { state.label().to_string() } else { format!("{} · {}", state.label(), st.branch) };
            InfoPanel(inner).row("Git", &text);
        });
        *self.0.git_task.borrow_mut() = Some(task);
    }

    fn show_item(&self, item: &FileItem) {
        let meta = item.meta().clone();
        self.0.image.set_from_gicon(&item.icon());
        crate::thumbs_ui::reset(&self.0.image);
        crate::thumbs_ui::apply(&self.0.image, item, 256);
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
        let size = match (dirs, files) {
            (_, 0) => "Folders: see Properties (Alt+Enter)".to_string(),
            (0, _) => format::size(bytes),
            _ => format!("{} in files, plus folders", format::size(bytes)),
        };
        self.row("Size", &size);
    }

    fn show_folder(&self, folder: &gio::File) {
        self.0.image.set_from_gicon(&crate::theme_ui::file_icon("folder", true, folder.path().as_deref()));
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
