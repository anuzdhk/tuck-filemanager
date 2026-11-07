// SPDX-License-Identifier: GPL-3.0-or-later
//! Column view, like macOS Finder: the pane's folder is the first column;
//! selecting a folder opens it in the next column, selecting a file shows a
//! preview at the end. The first column uses the pane's own model (sorting,
//! hidden files, the filter bar, live updates); deeper columns list their
//! folder with GTK's monitored DirectoryList through the pane's sorter and
//! filter, so they stay live too.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use tuck_core::format;

use crate::item::{ATTRS, FileItem};

pub type SetupList = Box<dyn Fn(&gtk::Widget, Option<gio::File>)>;

/// What the pane lends the columns.
pub struct Hooks {
    pub sorter: gtk::Sorter,
    pub filter: gtk::Filter,
    /// Double-click or Enter on an item (open a file, enter a folder).
    pub activate: Box<dyn Fn(&FileItem)>,
    /// Keys the file views share (Space, Delete, F2, type-ahead…).
    pub key: Box<dyn Fn(gdk::Key, gdk::ModifierType) -> glib::Propagation>,
    /// The selection changed.
    pub changed: Box<dyn Fn()>,
    /// Per-row setup the other views do too (drag source).
    pub setup_cell: Box<dyn Fn(&gtk::Widget)>,
    /// Tag a row widget with its item (right-click and drops find it).
    pub bind_cell: fn(&gtk::Widget, &FileItem),
    /// Per-column setup (drop target; the folder is None for the first).
    pub setup_list: SetupList,
}

struct Col {
    /// None for the first column (the pane's own folder).
    dir: Option<gio::File>,
    sel: gtk::MultiSelection,
    view: gtk::ListView,
    scroll: gtk::ScrolledWindow,
    /// Slides the column in and out.
    widget: gtk::Revealer,
}

pub struct Inner {
    root: gtk::ScrolledWindow,
    strip: gtk::Box,
    cols: RefCell<Vec<Col>>,
    preview: RefCell<Option<gtk::Revealer>>,
    active: Cell<usize>,
    /// Shown right now; the first column's model is shared with the other views.
    attached: Cell<bool>,
    hooks: Hooks,
    icon_px: Cell<i32>,
}

#[derive(Clone)]
pub struct Columns(Rc<Inner>);

const WIDTH: i32 = 240;

impl Columns {
    pub fn new(first: gtk::MultiSelection, hooks: Hooks) -> Self {
        let strip = gtk::Box::builder().css_classes(["columns-strip"]).build();
        let root = gtk::ScrolledWindow::builder()
            .child(&strip)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true)
            .vexpand(true)
            .build();
        root.update_property(&[gtk::accessible::Property::Label("Columns")]);
        let c = Columns(Rc::new(Inner { root, strip, cols: RefCell::default(), preview: RefCell::default(), active: Cell::new(0), attached: Cell::new(false), hooks, icon_px: Cell::new(20) }));
        c.push(None, first);
        c
    }

    pub fn widget(&self) -> gtk::Widget {
        self.0.root.clone().upcast()
    }

    /// Icon size, from the pane's zoom.
    pub fn set_icon_size(&self, px: i32) {
        self.0.icon_px.set(px.clamp(16, 48));
    }

    /// Drop every column but the first (the pane moved to another folder).
    pub fn reset(&self) {
        self.truncate(0);
        self.0.active.set(0);
    }

    /// The first column shows the pane's model only while this view is
    /// visible; hidden views would otherwise pay for every change.
    pub fn attach(&self, on: bool, first: &gtk::MultiSelection) {
        self.0.attached.set(on);
        if let Some(c) = self.0.cols.borrow().first() {
            c.view.set_model(on.then_some(first));
        }
        if !on {
            self.reset();
        }
    }

    fn push(&self, dir: Option<gio::File>, sel: gtk::MultiSelection) {
        let idx = self.0.cols.borrow().len();
        let factory = gtk::SignalListItemFactory::new();
        let weak = Rc::downgrade(&self.0);
        factory.connect_setup(move |_, obj| {
            let li = obj.downcast_ref::<gtk::ListItem>().expect("ListItem");
            let px = weak.upgrade().map(|i| i.icon_px.get()).unwrap_or(20);
            let row = gtk::Box::builder().spacing(8).css_classes(["column-row"]).build();
            row.append(&gtk::Image::builder().pixel_size(px).build());
            row.append(&gtk::Label::builder().xalign(0.0).hexpand(true).ellipsize(gtk::pango::EllipsizeMode::Middle).build());
            row.append(&gtk::Image::builder().icon_name("tuck-chevron-symbolic").pixel_size(12).css_classes(["dim-label"]).build());
            if let Some(i) = weak.upgrade() {
                (i.hooks.setup_cell)(row.upcast_ref());
            }
            li.set_child(Some(&row));
        });
        let weak = Rc::downgrade(&self.0);
        factory.connect_bind(move |_, obj| {
            let li = obj.downcast_ref::<gtk::ListItem>().expect("ListItem");
            let Some(item) = li.item().and_downcast::<FileItem>() else { return };
            let Some(row) = li.child() else { return };
            let icon = row.first_child().and_downcast::<gtk::Image>().expect("icon");
            let label = icon.next_sibling().and_downcast::<gtk::Label>().expect("label");
            let chevron = row.last_child().expect("chevron");
            icon.set_from_gicon(&item.icon());
            label.set_label(&item.name());
            chevron.set_opacity(if item.is_dir() { 1.0 } else { 0.0 });
            row.set_tooltip_text(Some(&item.name()));
            if let Some(i) = weak.upgrade() {
                (i.hooks.bind_cell)(&row, &item);
            }
        });
        let view = gtk::ListView::builder().model(&sel).factory(&factory).css_classes(["navigation-sidebar", "column-list"]).build();
        view.update_property(&[gtk::accessible::Property::Label(&match &dir {
            Some(d) => format!("{} column", crate::pane::folder_title(d)),
            None => "First column".into(),
        })]);
        (self.0.hooks.setup_list)(view.upcast_ref(), dir.clone());
        let scroll = gtk::ScrolledWindow::builder().child(&view).width_request(WIDTH).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();
        let frame = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        frame.append(&scroll);
        frame.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        let rev = slide_in(&frame, idx > 0);
        self.0.strip.append(&rev);

        // Selecting opens the next column; double-click or Enter activates.
        let weak = Rc::downgrade(&self.0);
        sel.connect_selection_changed(move |_, _, _| {
            if let Some(i) = weak.upgrade() {
                Columns(i).selection_changed(idx);
            }
        });
        let weak = Rc::downgrade(&self.0);
        view.connect_activate(move |v, pos| {
            let Some(i) = weak.upgrade() else { return };
            if let Some(item) = v.model().and_then(|m| m.item(pos)).and_downcast::<FileItem>() {
                (i.hooks.activate)(&item);
            }
        });
        // This column becomes the one actions apply to when it has focus.
        let focus = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(&self.0);
        focus.connect_enter(move |_| {
            if let Some(i) = weak.upgrade() {
                i.active.set(idx);
                (i.hooks.changed)();
            }
        });
        view.add_controller(focus);
        // Left / Right move between columns; other keys go to the pane.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&self.0);
        keys.connect_key_pressed(move |_, key, _, state| {
            let Some(i) = weak.upgrade() else { return glib::Propagation::Proceed };
            let c = Columns(i);
            let plain = (state & gtk::accelerator_get_default_mod_mask()).is_empty();
            match key {
                gdk::Key::Right if plain => {
                    c.focus_column(idx + 1, true);
                    glib::Propagation::Stop
                }
                gdk::Key::Left if plain => {
                    if idx > 0 {
                        c.focus_column(idx - 1, false);
                    }
                    glib::Propagation::Stop
                }
                _ => (c.0.hooks.key)(key, state),
            }
        });
        view.add_controller(keys);
        self.0.cols.borrow_mut().push(Col { dir, sel, view, scroll, widget: rev.clone() });
        rev.set_reveal_child(true);
        self.relayout();
    }

    /// One column fills the width like a plain list; with more, earlier
    /// columns keep their width and the last one (or the preview) fills.
    fn relayout(&self) {
        let cols = self.0.cols.borrow();
        let has_preview = self.0.preview.borrow().is_some();
        for (i, c) in cols.iter().enumerate() {
            c.scroll.set_hexpand(i + 1 == cols.len() && !has_preview);
        }
    }

    /// How many columns are open (not counting the preview).
    pub fn depth(&self) -> usize {
        self.0.cols.borrow().len()
    }

    /// Back in the column view: close the preview, else the rightmost
    /// column. False when only the first column is left.
    pub fn back(&self) -> bool {
        let n = self.0.cols.borrow().len();
        let has_preview = self.0.preview.borrow().is_some();
        // Unselecting in a column closes everything to its right.
        let target = if has_preview { n.checked_sub(1) } else { n.checked_sub(2) };
        let Some(idx) = target else { return false };
        let sel = self.0.cols.borrow()[idx].sel.clone();
        sel.unselect_all();
        self.focus_column(idx, false);
        true
    }

    fn truncate(&self, keep: usize) {
        let removed: Vec<Col> = {
            let mut cols = self.0.cols.borrow_mut();
            let at = (keep + 1).min(cols.len());
            cols.drain(at..).collect()
        };
        for c in removed {
            self.slide_out(&c.widget);
        }
        let preview = self.0.preview.borrow_mut().take();
        if let Some(p) = preview {
            self.slide_out(&p);
        }
        if self.0.active.get() > keep {
            self.0.active.set(keep);
        }
        self.relayout();
    }

    /// Slide a column away, then drop it.
    fn slide_out(&self, rev: &gtk::Revealer) {
        let strip = self.0.strip.clone();
        rev.connect_child_revealed_notify(move |r| {
            if !r.is_child_revealed() && r.parent().is_some() {
                strip.remove(r);
            }
        });
        rev.set_reveal_child(false);
    }

    fn selection_changed(&self, idx: usize) {
        if !self.0.attached.get() {
            return;
        }
        let sel = self.0.cols.borrow().get(idx).map(|c| c.sel.clone());
        let Some(sel) = sel else { return };
        self.0.active.set(idx);
        self.truncate(idx);
        let picked = selected_in(&sel);
        if let [one] = picked.as_slice() {
            if one.is_dir() {
                self.open_dir(one.target());
            } else {
                self.show_preview(one);
            }
        }
        // The pane already reports changes in its own (first) column.
        if idx > 0 {
            (self.0.hooks.changed)();
        }
    }

    fn open_dir(&self, dir: gio::File) {
        let list = gtk::DirectoryList::new(Some(ATTRS), Some(&dir));
        list.set_monitored(true);
        let mapped = gtk::MapListModel::new(Some(list), |obj| {
            let info = obj.downcast_ref::<gio::FileInfo>().expect("file info").clone();
            let file = info.attribute_object("standard::file").and_downcast::<gio::File>().expect("standard::file");
            FileItem::from_info(file, info).upcast()
        });
        let filtered = gtk::FilterListModel::new(Some(mapped), Some(self.0.hooks.filter.clone()));
        let sorted = gtk::SortListModel::new(Some(filtered), Some(self.0.hooks.sorter.clone()));
        let sel = gtk::MultiSelection::new(Some(sorted));
        self.push(Some(dir), sel);
        self.scroll_to_end();
    }

    fn show_preview(&self, item: &FileItem) {
        let b = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).width_request(WIDTH + 40).css_classes(["column-preview"]).build();
        let image = gtk::Image::builder().pixel_size(160).build();
        image.set_from_gicon(&item.icon());
        crate::thumbs_ui::apply(&image, item, 256);
        b.append(&image);
        b.append(&gtk::Label::builder().label(item.name()).wrap(true).wrap_mode(gtk::pango::WrapMode::WordChar).justify(gtk::Justification::Center).css_classes(["title-4"]).build());
        let meta = item.meta().clone();
        let kind = if meta.type_name.is_empty() { "Document".to_string() } else { meta.type_name.clone() };
        b.append(&gtk::Label::builder().label(format!("{kind} · {}", format::size(meta.size))).css_classes(["dim-label"]).build());
        b.append(&gtk::Label::builder().label(format!("Modified {}", format::date_full(meta.modified))).wrap(true).justify(gtk::Justification::Center).css_classes(["dim-label", "caption"]).build());
        b.set_hexpand(true);
        let rev = slide_in(&b, true);
        self.0.strip.append(&rev);
        rev.set_reveal_child(true);
        *self.0.preview.borrow_mut() = Some(rev);
        self.relayout();
        self.scroll_to_end();
    }

    fn scroll_to_end(&self) {
        let adj = self.0.root.hadjustment();
        glib::idle_add_local_once(move || adj.set_value(adj.upper() - adj.page_size()));
    }

    /// Move the keyboard to column `idx`; `pick_first` selects its first
    /// item when nothing there is selected yet.
    fn focus_column(&self, idx: usize, pick_first: bool) {
        let col = self.0.cols.borrow().get(idx).map(|c| (c.view.clone(), c.sel.clone()));
        let Some((view, sel)) = col else { return };
        if pick_first && sel.selection().is_empty() && sel.n_items() > 0 {
            sel.select_item(0, true);
        }
        self.0.active.set(idx);
        if sel.n_items() == 0 {
            // An empty folder: nothing to focus inside, keep the list itself.
            view.grab_focus();
            return;
        }
        let pos = if sel.selection().is_empty() { 0 } else { sel.selection().minimum() };
        view.scroll_to(pos, gtk::ListScrollFlags::FOCUS, None);
    }

    pub fn focus(&self) {
        self.focus_column(self.0.active.get(), false);
    }

    /// The selection model of the column actions apply to.
    pub fn active_sel(&self) -> Option<gtk::MultiSelection> {
        let cols = self.0.cols.borrow();
        cols.get(self.0.active.get()).or_else(|| cols.first()).map(|c| c.sel.clone())
    }

    /// The folder of the column actions apply to (None = the pane's own).
    pub fn active_dir(&self) -> Option<gio::File> {
        self.0.cols.borrow().get(self.0.active.get()).and_then(|c| c.dir.clone())
    }

    /// Make the column holding `item` the active one (right-click there).
    pub fn activate_column_of(&self, item: &FileItem) {
        let found = self.0.cols.borrow().iter().position(|c| (0..c.sel.n_items()).any(|p| c.sel.item(p).as_ref() == Some(item.upcast_ref())));
        if let Some(idx) = found {
            self.0.active.set(idx);
        }
    }

    /// Select `name` in column `idx` (test hook).
    pub fn select_name(&self, idx: usize, name: &str) {
        let sel = self.0.cols.borrow().get(idx).map(|c| c.sel.clone());
        if let Some(sel) = sel
            && let Some(pos) = (0..sel.n_items()).find(|&p| sel.item(p).and_downcast::<FileItem>().is_some_and(|i| i.name() == name))
        {
            sel.select_item(pos, true);
        }
    }

    pub fn scroll_to(&self, pos: u32) {
        if let Some(c) = self.0.cols.borrow().get(self.0.active.get())
            && pos < c.sel.n_items()
        {
            c.view.scroll_to(pos, gtk::ListScrollFlags::FOCUS, None);
        }
    }
}

/// A column that slides in from the left edge (quick, eased).
fn slide_in(child: &impl IsA<gtk::Widget>, animate: bool) -> gtk::Revealer {
    gtk::Revealer::builder()
        .child(child)
        .transition_type(gtk::RevealerTransitionType::SlideRight)
        .transition_duration(if animate { 180 } else { 0 })
        .build()
}

fn selected_in(sel: &gtk::MultiSelection) -> Vec<FileItem> {
    let set = sel.selection();
    (0..set.size()).filter_map(|i| sel.item(set.nth(i as u32)).and_downcast::<FileItem>()).collect()
}
