// SPDX-License-Identifier: GPL-3.0-or-later
//! Folders panel (F7): a tree of folders that follows the current location,
//! like Dolphin's. Folders load lazily and update live; hidden folders are
//! left out.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};

const ATTRS: &str = "standard::name,standard::display-name,standard::type,standard::is-hidden,standard::is-backup";

type OnOpen = Rc<dyn Fn(gio::File)>;

pub struct Inner {
    root: gtk::ScrolledWindow,
    view: gtk::ListView,
    selection: gtk::SingleSelection,
    roots: gio::ListStore,
    tree: gtk::TreeListModel,
    /// The folder to reveal once the tree has loaded far enough.
    target: RefCell<Option<gio::File>>,
    stepping: Cell<bool>,
    on_open: RefCell<Option<OnOpen>>,
}

#[derive(Clone)]
pub struct FoldersPanel(Rc<Inner>);

fn info_file(info: &gio::FileInfo) -> Option<gio::File> {
    info.attribute_object("standard::file").and_downcast::<gio::File>()
}

fn row_file(obj: &glib::Object) -> Option<gio::File> {
    let row = obj.downcast_ref::<gtk::TreeListRow>()?;
    info_file(&row.item()?.downcast::<gio::FileInfo>().ok()?)
}

/// Sub-folders of `dir`, sorted naturally, loaded off the main thread by GIO.
fn children(dir: &gio::File) -> gio::ListModel {
    let list = gtk::DirectoryList::new(Some(ATTRS), Some(dir));
    list.set_monitored(true);
    let filter = gtk::CustomFilter::new(|o| {
        let i = o.downcast_ref::<gio::FileInfo>().expect("file info");
        i.file_type() == gio::FileType::Directory && !i.is_hidden() && !i.is_backup()
    });
    let sorter = gtk::CustomSorter::new(|a, b| {
        let a = a.downcast_ref::<gio::FileInfo>().expect("file info").display_name();
        let b = b.downcast_ref::<gio::FileInfo>().expect("file info").display_name();
        tuck_core::sort::natural_cmp(&a, &b).into()
    });
    let filtered = gtk::FilterListModel::new(Some(list), Some(filter));
    gtk::SortListModel::new(Some(filtered), Some(sorter)).upcast()
}

impl FoldersPanel {
    pub fn new() -> Self {
        let roots = gio::ListStore::new::<gio::FileInfo>();
        let tree = gtk::TreeListModel::new(roots.clone(), false, false, |item| {
            let info = item.downcast_ref::<gio::FileInfo>()?;
            info_file(info).map(|f| children(&f))
        });
        let selection = gtk::SingleSelection::builder().model(&tree).autoselect(false).can_unselect(true).build();
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, li| {
            let li = li.downcast_ref::<gtk::ListItem>().expect("list item");
            let icon = gtk::Image::builder().pixel_size(16).build();
            let label = gtk::Label::builder().xalign(0.0).build();
            let b = gtk::Box::builder().spacing(8).build();
            b.append(&icon);
            b.append(&label);
            let ex = gtk::TreeExpander::builder().child(&b).indent_for_icon(true).build();
            li.set_child(Some(&ex));
        });
        factory.connect_bind(|_, li| {
            let li = li.downcast_ref::<gtk::ListItem>().expect("list item");
            let Some(row) = li.item().and_downcast::<gtk::TreeListRow>() else { return };
            let Some(ex) = li.child().and_downcast::<gtk::TreeExpander>() else { return };
            ex.set_list_row(Some(&row));
            let Some(info) = row.item().and_downcast::<gio::FileInfo>() else { return };
            let b = ex.child().and_downcast::<gtk::Box>().expect("row box");
            let icon = b.first_child().and_downcast::<gtk::Image>().expect("icon");
            let label = b.last_child().and_downcast::<gtk::Label>().expect("label");
            let name = info.display_name();
            let path = info_file(&info).and_then(|f| f.path());
            icon.set_from_gicon(&crate::theme_ui::file_icon(&name, true, path.as_deref()));
            label.set_text(&name);
            li.set_accessible_label(&name);
        });
        let view = gtk::ListView::builder()
            .model(&selection)
            .factory(&factory)
            .single_click_activate(true)
            .css_classes(["navigation-sidebar", "folders-tree"])
            .build();
        view.update_property(&[gtk::accessible::Property::Label("Folders")]);
        let root = gtk::ScrolledWindow::builder().child(&view).vexpand(true).hscrollbar_policy(gtk::PolicyType::Automatic).build();
        let panel = FoldersPanel(Rc::new(Inner {
            root,
            view,
            selection,
            roots,
            tree,
            target: RefCell::new(None),
            stepping: Cell::new(false),
            on_open: RefCell::new(None),
        }));
        panel.wire();
        panel
    }

    pub fn widget(&self) -> gtk::Widget {
        self.0.root.clone().upcast()
    }

    pub fn connect_open(&self, f: impl Fn(gio::File) + 'static) {
        *self.0.on_open.borrow_mut() = Some(Rc::new(f));
    }

    fn wire(&self) {
        let weak = Rc::downgrade(&self.0);
        self.0.view.connect_activate(move |_, pos| {
            let Some(i) = weak.upgrade() else { return };
            let Some(file) = i.tree.item(pos).and_then(|o| row_file(&o)) else { return };
            let cb = i.on_open.borrow().clone();
            if let Some(cb) = cb {
                cb(file);
            }
        });
        // Rows appear as folders load: keep walking towards the target.
        let weak = Rc::downgrade(&self.0);
        self.0.tree.connect_items_changed(move |_, _, _, _| {
            let Some(i) = weak.upgrade() else { return };
            if i.target.borrow().is_some() && !i.stepping.get() {
                i.stepping.set(true);
                let weak = Rc::downgrade(&i);
                glib::idle_add_local_once(move || {
                    if let Some(i) = weak.upgrade() {
                        i.stepping.set(false);
                        FoldersPanel(i).step();
                    }
                });
            }
        });
    }

    /// Show `dir` in the tree: pick the right root, expand down to it and
    /// select it.
    pub fn reveal(&self, dir: &gio::File) {
        let home = gio::File::for_path(tuck_core::paths::home());
        let root = if dir.has_prefix(&home) || dir.equal(&home) {
            home
        } else if dir.path().is_some() {
            gio::File::for_path("/")
        } else {
            // Network and other non-local places: the place itself.
            let mut top = dir.clone();
            while let Some(p) = top.parent() {
                top = p;
            }
            top
        };
        let current = self.0.roots.item(0).and_downcast::<gio::FileInfo>().and_then(|i| info_file(&i));
        if !current.is_some_and(|c| c.equal(&root)) {
            let info = gio::FileInfo::new();
            let name = if root.path().as_deref() == Some(std::path::Path::new("/")) {
                "Computer".to_string()
            } else if root.equal(&gio::File::for_path(tuck_core::paths::home())) {
                "Home".to_string()
            } else {
                root.parse_name().to_string()
            };
            info.set_display_name(&name);
            info.set_name(&name);
            info.set_file_type(gio::FileType::Directory);
            info.set_attribute_object("standard::file", &root);
            self.0.roots.remove_all();
            self.0.roots.append(&info);
            if let Some(r) = self.0.tree.row(0) {
                r.set_expanded(true);
            }
        }
        *self.0.target.borrow_mut() = Some(dir.clone());
        self.step();
    }

    fn step(&self) {
        let Some(target) = self.0.target.borrow().clone() else { return };
        let n = self.0.tree.n_items();
        for pos in 0..n {
            let Some(row) = self.0.tree.row(pos) else { continue };
            let Some(file) = row.item().and_downcast::<gio::FileInfo>().and_then(|i| info_file(&i)) else { continue };
            if file.equal(&target) {
                *self.0.target.borrow_mut() = None;
                self.0.selection.set_selected(pos);
                // After layout, so the row lands in view (centred if far).
                let view = self.0.view.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(60), move || {
                    view.scroll_to(pos, gtk::ListScrollFlags::NONE, None);
                });
                return;
            }
            if target.has_prefix(&file) && !row.is_expanded() {
                // Children load in the background; items-changed resumes us.
                row.set_expanded(true);
                return;
            }
        }
        // Not there (yet, or hidden): show nothing selected.
        self.0.selection.set_selected(gtk::INVALID_LIST_POSITION);
    }
}
