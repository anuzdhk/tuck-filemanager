// SPDX-License-Identifier: GPL-3.0-or-later
//! One folder view: its model chain, three view modes, history, live updates,
//! filter bar and type-ahead. A tab holds one pane, or two in split view.
//!
//! Model chain:
//!   ListStore<FileItem> → FilterListModel (hidden + filter bar)
//!     → SortListModel (sorter + section sorter for groups)
//!       → MultiSelection            (Icons, Compact)
//!       → TreeListModel → MultiSelection   (Details, expandable folders)

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::time::Instant;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use strata_core::format;
use strata_core::group::compare_groups;
use strata_core::settings::{FolderViews, ViewMode, ViewSettings, save_json};
use strata_core::listing::{self, RawEntry};
use strata_core::sort::{SortKey, SortOptions, compare};
use strata_core::typeahead;

use crate::item::{ATTRS, FileItem};

#[derive(Debug, Clone)]
pub enum PaneEvent {
    /// Location, history or loading state changed.
    Location,
    Selection,
    Counts,
    Focused,
    Settings,
    Toast(String),
    /// Middle-click or Ctrl+Enter on a folder.
    OpenInNewTab(gio::File),
}

type Listener = Rc<dyn Fn(&Pane, PaneEvent)>;

pub struct Inner {
    root: gtk::Box,
    title: gtk::Label,
    subtitle: gtk::Label,
    stack: gtk::Stack,
    status: adw::StatusPage,
    icons: gtk::GridView,
    compact: gtk::GridView,
    details: gtk::ColumnView,
    store: RefCell<gio::ListStore>,
    filter_model: gtk::FilterListModel,
    filter: gtk::CustomFilter,
    sorter: gtk::CustomSorter,
    section_sorter: gtk::CustomSorter,
    sort_model: gtk::SortListModel,
    flat_sel: gtk::MultiSelection,
    tree_sel: gtk::MultiSelection,
    filter_bar: gtk::Revealer,
    filter_entry: gtk::SearchEntry,
    settings: Rc<Cell<ViewSettings>>,
    filter_text: Rc<RefCell<String>>,
    folder_views: Rc<RefCell<FolderViews>>,
    location: RefCell<gio::File>,
    back: RefCell<Vec<gio::File>>,
    forward: RefCell<Vec<gio::File>>,
    load_task: RefCell<Option<glib::JoinHandle<()>>>,
    monitor: RefCell<Option<gio::FileMonitor>>,
    loading: Cell<bool>,
    typeahead: RefCell<typeahead::Buffer>,
    listener: RefCell<Option<Listener>>,
    /// Name to select once the folder has loaded (after going up).
    select_after_load: RefCell<Option<String>>,
    syncing_sort: Cell<bool>,
}

#[derive(Clone)]
pub struct Pane(Rc<Inner>);

impl Pane {
    pub fn new(folder_views: Rc<RefCell<FolderViews>>, location: gio::File) -> Self {
        let settings = Rc::new(Cell::new(ViewSettings::default()));
        let filter_text = Rc::new(RefCell::new(String::new()));

        let store = gio::ListStore::new::<FileItem>();

        let filter = {
            let settings = settings.clone();
            let text = filter_text.clone();
            gtk::CustomFilter::new(move |obj| {
                let item = obj.downcast_ref::<FileItem>().expect("FileItem");
                if !settings.get().show_hidden && item.is_hidden() {
                    return false;
                }
                let t = text.borrow();
                t.is_empty() || item.meta().name.to_lowercase().contains(t.as_str())
            })
        };
        let filter_model = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        filter_model.set_incremental(false);

        let sorter = {
            let settings = settings.clone();
            gtk::CustomSorter::new(move |a, b| {
                let a = a.downcast_ref::<FileItem>().expect("FileItem");
                let b = b.downcast_ref::<FileItem>().expect("FileItem");
                compare(&a.meta(), &b.meta(), &settings.get().sort).into()
            })
        };
        let section_sorter = {
            let settings = settings.clone();
            gtk::CustomSorter::new(move |a, b| {
                let s = settings.get();
                if !s.grouped {
                    return gtk::Ordering::Equal;
                }
                let a = a.downcast_ref::<FileItem>().expect("FileItem");
                let b = b.downcast_ref::<FileItem>().expect("FileItem");
                if s.sort.folders_first && a.is_dir() != b.is_dir() {
                    return if a.is_dir() { gtk::Ordering::Smaller } else { gtk::Ordering::Larger };
                }
                let key = s.sort.key;
                let ord = a.with_group(key, |ga| b.with_group(key, |gb| compare_groups(ga, gb)));
                if s.sort.descending { ord.reverse().into() } else { ord.into() }
            })
        };
        let sort_model = gtk::SortListModel::new(Some(filter_model.clone()), Some(sorter.clone()));
        sort_model.set_section_sorter(Some(&section_sorter));

        let flat_sel = gtk::MultiSelection::new(Some(sort_model.clone()));

        // Expandable folders in Details: every expanded folder gets its own
        // store, filtered and sorted with the same filter and sorter.
        let tree = {
            let filter = filter.clone();
            let sorter = sorter.clone();
            let settings = settings.clone();
            gtk::TreeListModel::new(sort_model.clone(), false, false, move |obj| {
                let item = obj.downcast_ref::<FileItem>()?;
                if !item.is_dir() {
                    return None;
                }
                let child_store = gio::ListStore::new::<FileItem>();
                spawn_list_into(item.target(), child_store.clone(), settings.get().sort);
                let f = gtk::FilterListModel::new(Some(child_store), Some(filter.clone()));
                let s = gtk::SortListModel::new(Some(f), Some(sorter.clone()));
                Some(s.upcast())
            })
        };
        let tree_sel = gtk::MultiSelection::new(Some(tree));

        let icons = gtk::GridView::builder()
            .model(&flat_sel)
            .max_columns(64)
            .min_columns(1)
            .enable_rubberband(true)
            .css_classes(["file-grid", "icons"])
            .build();
        let compact = gtk::GridView::builder()
            .model(&flat_sel)
            .max_columns(64)
            .min_columns(1)
            .enable_rubberband(true)
            .css_classes(["file-grid", "compact"])
            .build();
        let details = gtk::ColumnView::builder()
            .model(&tree_sel)
            .enable_rubberband(true)
            .show_column_separators(false)
            .reorderable(true)
            .css_classes(["file-details"])
            .build();
        icons.update_property(&[gtk::accessible::Property::Label("Files")]);
        compact.update_property(&[gtk::accessible::Property::Label("Files")]);
        details.update_property(&[gtk::accessible::Property::Label("Files")]);

        let stack = gtk::Stack::builder().vexpand(true).hexpand(true).build();
        for (name, view) in [
            ("icons", icons.clone().upcast::<gtk::Widget>()),
            ("compact", compact.clone().upcast()),
            ("details", details.clone().upcast()),
        ] {
            let sw = gtk::ScrolledWindow::builder()
                .child(&view)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .build();
            if name == "details" {
                sw.set_hscrollbar_policy(gtk::PolicyType::Automatic);
            }
            stack.add_named(&sw, Some(name));
        }
        let status = adw::StatusPage::builder().css_classes(["compact"]).build();
        stack.add_named(&status, Some("status"));

        // Heading: folder name + counts, like a document title.
        let title = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .css_classes(["pane-title"])
            .build();
        let subtitle = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["pane-subtitle"])
            .build();
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 2);
        titles.set_hexpand(true);
        titles.append(&title);
        titles.append(&subtitle);
        let sort_button = gtk::MenuButton::builder()
            .icon_name("view-sort-ascending-symbolic")
            .menu_model(&sort_menu())
            .tooltip_text("Sort and Group")
            .css_classes(["flat", "circular"])
            .valign(gtk::Align::Center)
            .build();
        sort_button.update_property(&[gtk::accessible::Property::Label("Sort and Group")]);
        let heading = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .css_classes(["pane-heading"])
            .build();
        heading.append(&titles);
        heading.append(&sort_button);

        let filter_entry = gtk::SearchEntry::builder()
            .placeholder_text("Filter by name")
            .hexpand(true)
            .build();
        filter_entry.update_property(&[gtk::accessible::Property::Label("Filter by name")]);
        let filter_close = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Close Filter")
            .css_classes(["flat", "circular"])
            .build();
        filter_close.update_property(&[gtk::accessible::Property::Label("Close Filter")]);
        let filter_box = gtk::Box::builder()
            .spacing(6)
            .css_classes(["filter-bar"])
            .build();
        filter_box.append(&filter_entry);
        filter_box.append(&filter_close);
        let filter_bar = gtk::Revealer::builder()
            .child(&filter_box)
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .build();

        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["pane"])
            .hexpand(true)
            .vexpand(true)
            .build();
        root.append(&heading);
        root.append(&stack);
        root.append(&filter_bar);

        let pane = Pane(Rc::new(Inner {
            root,
            title,
            subtitle,
            stack,
            status,
            icons,
            compact,
            details,
            store: RefCell::new(store),
            filter_model: filter_model.clone(),
            filter,
            sorter,
            section_sorter,
            sort_model,
            flat_sel,
            tree_sel,
            filter_bar,
            filter_entry,
            settings,
            filter_text,
            folder_views,
            location: RefCell::new(location.clone()),
            back: RefCell::new(Vec::new()),
            forward: RefCell::new(Vec::new()),
            load_task: RefCell::new(None),
            monitor: RefCell::new(None),
            loading: Cell::new(false),
            typeahead: RefCell::new(typeahead::Buffer::default()),
            listener: RefCell::new(None),
            select_after_load: RefCell::new(None),
            syncing_sort: Cell::new(false),
        }));
        pane.build_factories();
        pane.build_columns();
        pane.wire(filter_close);
        pane.load(location);
        pane
    }

    fn downgrade(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    fn from_weak(w: &Weak<Inner>) -> Option<Pane> {
        w.upgrade().map(Pane)
    }

    pub fn ptr_eq(&self, other: &Pane) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }

    pub fn widget(&self) -> gtk::Widget {
        self.0.root.clone().upcast()
    }

    pub fn connect(&self, f: impl Fn(&Pane, PaneEvent) + 'static) {
        *self.0.listener.borrow_mut() = Some(Rc::new(f));
    }

    fn emit(&self, ev: PaneEvent) {
        let l = self.0.listener.borrow().clone();
        if let Some(l) = l {
            l(self, ev);
        }
    }

    // ---------------------------------------------------------------- state

    pub fn location(&self) -> gio::File {
        self.0.location.borrow().clone()
    }

    pub fn path(&self) -> Option<PathBuf> {
        self.location().path()
    }

    pub fn settings(&self) -> ViewSettings {
        self.0.settings.get()
    }

    pub fn is_loading(&self) -> bool {
        self.0.loading.get()
    }

    pub fn can_go_back(&self) -> bool {
        !self.0.back.borrow().is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.0.forward.borrow().is_empty()
    }

    pub fn can_go_up(&self) -> bool {
        self.location().parent().is_some()
    }

    /// Display name of the current folder.
    pub fn title(&self) -> String {
        folder_title(&self.location())
    }

    /// Details uses the tree (expandable folders) unless grouping is on:
    /// GtkTreeListModel doesn't expose sections, so groups need the flat model.
    fn details_sel(&self) -> &gtk::MultiSelection {
        if self.settings().grouped { &self.0.flat_sel } else { &self.0.tree_sel }
    }

    /// Selected items, in view order.
    pub fn selected_items(&self) -> Vec<FileItem> {
        let mode = self.settings().mode;
        let sel = if mode == ViewMode::Details { self.details_sel() } else { &self.0.flat_sel };
        let set = sel.selection();
        let mut out = Vec::with_capacity(set.size() as usize);
        for i in 0..set.size() {
            let pos = set.nth(i as u32);
            if let Some(obj) = sel.item(pos)
                && let Some(item) = unwrap_item(&obj) {
                    out.push(item);
                }
        }
        out
    }

    // ----------------------------------------------------------- navigation

    pub fn navigate(&self, file: gio::File) {
        if file.equal(&self.location()) {
            return;
        }
        self.0.back.borrow_mut().push(self.location());
        self.0.forward.borrow_mut().clear();
        self.load(file);
    }

    pub fn go_back(&self) {
        let prev = self.0.back.borrow_mut().pop();
        if let Some(prev) = prev {
            self.0.forward.borrow_mut().push(self.location());
            self.remember_child_of(&prev);
            self.load(prev);
        }
    }

    pub fn go_forward(&self) {
        let next = self.0.forward.borrow_mut().pop();
        if let Some(next) = next {
            self.0.back.borrow_mut().push(self.location());
            self.load(next);
        }
    }

    pub fn go_up(&self) {
        if let Some(parent) = self.location().parent() {
            self.remember_child_of(&parent);
            self.navigate(parent);
        }
    }

    /// When going to an ancestor, select the folder we came from.
    fn remember_child_of(&self, ancestor: &gio::File) {
        let here = self.location();
        let mut cur = Some(here);
        while let Some(f) = cur {
            if let Some(p) = f.parent() {
                if p.equal(ancestor) {
                    *self.0.select_after_load.borrow_mut() =
                        f.basename().map(|b| b.to_string_lossy().into_owned());
                    return;
                }
                cur = Some(p);
            } else {
                break;
            }
        }
    }

    pub fn reload(&self) {
        self.load(self.location());
    }

    fn load(&self, file: gio::File) {
        if let Some(task) = self.0.load_task.borrow_mut().take() {
            task.abort();
        }
        *self.0.monitor.borrow_mut() = None;
        *self.0.location.borrow_mut() = file.clone();

        // Per-folder view settings, keeping the filter bar's text.
        let key = settings_key(&file);
        let v = self.0.folder_views.borrow().get(&key);
        self.apply_settings(v, false);

        // A fresh store: emptying a 100k-item one costs as much as filling it.
        self.replace_store(gio::ListStore::new::<FileItem>());
        self.0.loading.set(true);
        self.0.title.set_label(&self.title());
        self.update_subtitle();
        self.show_view();
        self.emit(PaneEvent::Location);

        let weak = self.downgrade();
        let task = glib::MainContext::default().spawn_local(async move {
            let started = Instant::now();
            let sort = Pane::from_weak(&weak).map(|p| p.settings().sort).unwrap_or_default();
            let result = list_dir(&file, sort, |batch| {
                if let Some(p) = Pane::from_weak(&weak) {
                    p.add_batch(batch);
                }
            })
            .await;
            let Some(pane) = Pane::from_weak(&weak) else { return };
            pane.0.loading.set(false);
            match result {
                Ok(()) => {
                    pane.start_monitor(&file);
                    if std::env::var_os("STRATA_DEBUG_TIMING").is_some() {
                        eprintln!(
                            "listed {} items in {:?}",
                            pane.store().n_items(),
                            started.elapsed()
                        );
                    }
                }
                Err(e) => pane.show_error(&e),
            }
            pane.show_view();
            pane.update_subtitle();
            pane.select_pending();
            pane.emit(PaneEvent::Location);
        });
        *self.0.load_task.borrow_mut() = Some(task);
    }

    fn store(&self) -> gio::ListStore {
        self.0.store.borrow().clone()
    }

    fn replace_store(&self, store: gio::ListStore) {
        self.with_focus_guard(|| {
            *self.0.store.borrow_mut() = store.clone();
            self.0.filter_model.set_model(Some(&store));
        });
    }

    /// Run `f`, which may destroy the focused row widget, then put focus back
    /// on the current view. Without this GTK keeps tracking a dead widget.
    fn with_focus_guard(&self, f: impl FnOnce()) {
        let focused = self.has_focus_within();
        if focused
            && let Some(w) = self.0.root.root() {
                w.set_focus(None::<&gtk::Widget>);
            }
        f();
        if focused {
            self.focus_view();
        }
    }

    fn has_focus_within(&self) -> bool {
        let Some(root) = self.0.root.root() else { return false };
        let Some(focus) = root.focus() else { return false };
        focus.is_ancestor(&self.0.root)
    }

    /// Only the visible view is attached to the model; hidden views would
    /// otherwise pay for every change too.
    fn attach_views(&self) {
        let none = None::<&gtk::MultiSelection>;
        let mode = self.settings().mode;
        self.0.icons.set_model(if mode == ViewMode::Icons { Some(&self.0.flat_sel) } else { none });
        self.0.compact.set_model(if mode == ViewMode::Compact { Some(&self.0.flat_sel) } else { none });
        self.0.details.set_model(if mode == ViewMode::Details { Some(self.details_sel()) } else { none });
    }

    /// Add listed items. Big batches build a new store and swap it in: GTK's
    /// models handle one wholesale change far faster than a huge in-place
    /// splice (which walks per-item bookkeeping and accessibility state).
    fn add_batch(&self, batch: Vec<FileItem>) {
        let store = self.store();
        if batch.len() < 1000 {
            store.extend_from_slice(&batch);
            return;
        }
        let fresh = gio::ListStore::new::<FileItem>();
        let existing: Vec<glib::Object> = (0..store.n_items()).filter_map(|i| store.item(i)).collect();
        fresh.extend_from_slice(&existing);
        fresh.extend_from_slice(&batch);
        self.replace_store(fresh);
    }

    fn select_pending(&self) {
        let Some(name) = self.0.select_after_load.borrow_mut().take() else { return };
        let model = &self.0.sort_model;
        for i in 0..model.n_items() {
            if let Some(item) = model.item(i).and_downcast::<FileItem>()
                && item.raw_name().to_string_lossy() == name {
                    self.select_and_reveal(i);
                    return;
                }
        }
    }

    /// Select the item at `pos` in sort order and scroll it into view.
    fn select_and_reveal(&self, pos: u32) {
        let flags = gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT;
        match self.settings().mode {
            ViewMode::Icons => self.0.icons.scroll_to(pos, flags, None),
            ViewMode::Compact => self.0.compact.scroll_to(pos, flags, None),
            ViewMode::Details => {
                // Root items keep their position only if nothing above is
                // expanded; find the row for this item instead.
                let target = self.0.sort_model.item(pos);
                let tree = self.details_sel();
                for row in 0..tree.n_items() {
                    if let Some(obj) = tree.item(row)
                        && unwrap_item(&obj).map(|i| i.upcast::<glib::Object>()) == target {
                            self.0.details.scroll_to(row, None::<&gtk::ColumnViewColumn>, flags, None);
                            return;
                        }
                }
            }
        }
    }

    fn show_error(&self, e: &glib::Error) {
        let (title, hint) = describe_error(e);
        self.0.status.set_icon_name(Some("dialog-warning-symbolic"));
        self.0.status.set_title(&title);
        self.0.status.set_description(Some(&hint));
        self.0.stack.set_visible_child_name("status");
    }

    fn show_view(&self) {
        let empty = self.0.sort_model.n_items() == 0;
        if self.0.stack.visible_child_name().as_deref() == Some("status")
            && self.0.status.icon_name().as_deref() == Some("dialog-warning-symbolic")
            && !self.0.loading.get()
            && self.store().n_items() == 0
        {
            // Keep showing the error page.
            return;
        }
        if empty && !self.0.loading.get() {
            let filtered = !self.0.filter_text.borrow().is_empty();
            let hidden_only = self.store().n_items() > 0;
            self.0.status.set_icon_name(Some("folder-symbolic"));
            if filtered {
                self.0.status.set_title("No Matches");
                self.0.status.set_description(Some("No items in this folder match the filter."));
            } else if hidden_only {
                self.0.status.set_title("Only Hidden Items");
                self.0
                    .status
                    .set_description(Some("Press Ctrl+H to show hidden files."));
            } else {
                self.0.status.set_title("Folder Is Empty");
                self.0.status.set_description(None);
            }
            self.0.stack.set_visible_child_name("status");
        } else {
            self.0.stack.set_visible_child_name(self.settings().mode.id());
        }
    }

    fn update_subtitle(&self) {
        if self.0.loading.get() && self.store().n_items() == 0 {
            self.0.subtitle.set_label("Loading…");
            return;
        }
        let shown = self.0.sort_model.n_items() as u64;
        let mut text = format::item_count(shown);
        let hidden = self.store().n_items() as u64 - shown;
        if hidden > 0 && self.0.filter_text.borrow().is_empty() {
            text.push_str(&format!(" · {hidden} hidden"));
        }
        let sel = self.selected_items();
        if !sel.is_empty() {
            let bytes: u64 = sel.iter().filter(|i| !i.is_dir()).map(|i| i.meta().size).sum();
            text.push_str(&format!(" · {} selected", sel.len()));
            if bytes > 0 {
                text.push_str(&format!(" ({})", format::size(bytes)));
            }
        }
        self.0.subtitle.set_label(&text);
    }

    // ------------------------------------------------------- live updates

    fn start_monitor(&self, dir: &gio::File) {
        let Ok(monitor) =
            dir.monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        else {
            return;
        };
        monitor.set_rate_limit(250);
        let weak = self.downgrade();
        monitor.connect_changed(move |_, file, other, event| {
            let Some(pane) = Pane::from_weak(&weak) else { return };
            use gio::FileMonitorEvent as E;
            match event {
                E::Created | E::MovedIn => pane.upsert(file.clone()),
                E::Deleted | E::MovedOut => pane.remove(file),
                E::Renamed => {
                    pane.remove(file);
                    if let Some(o) = other {
                        pane.upsert(o.clone());
                    }
                }
                E::ChangesDoneHint | E::AttributeChanged => pane.upsert(file.clone()),
                _ => {}
            }
        });
        *self.0.monitor.borrow_mut() = Some(monitor);
    }

    fn position_of(&self, file: &gio::File) -> Option<u32> {
        let name = file.basename()?;
        let store = self.store();
        (0..store.n_items()).find(|&i| {
            store.item(i).and_downcast::<FileItem>().is_some_and(|it| it.raw_name_eq(name.as_os_str()))
        })
    }

    fn remove(&self, file: &gio::File) {
        if let Some(pos) = self.position_of(file) {
            self.store().remove(pos);
        }
    }

    /// Add `file`, or refresh it if it's already listed.
    fn upsert(&self, file: gio::File) {
        let weak = self.downgrade();
        glib::MainContext::default().spawn_local(async move {
            let info = file
                .query_info_future(ATTRS, gio::FileQueryInfoFlags::NONE, glib::Priority::DEFAULT)
                .await;
            let Some(pane) = Pane::from_weak(&weak) else { return };
            // Ignore late events for a folder we've left.
            if !file.parent().is_some_and(|p| p.equal(&pane.location())) {
                return;
            }
            let Ok(info) = info else { return };
            let item = FileItem::from_info(file.clone(), info);
            match pane.position_of(&file) {
                Some(pos) => pane.store().splice(pos, 1, &[item]),
                None => pane.store().append(&item),
            }
        });
    }

    // ----------------------------------------------------------- settings

    /// Apply view settings. `persist` saves them for this folder.
    pub fn apply_settings(&self, v: ViewSettings, persist: bool) {
        let old = self.0.settings.replace(v);
        if old.show_hidden != v.show_hidden {
            self.0.filter.changed(gtk::FilterChange::Different);
        }
        if old.sort != v.sort || old.grouped != v.grouped {
            self.0.sorter.changed(gtk::SorterChange::Different);
            self.0.section_sorter.changed(gtk::SorterChange::Different);
        }
        if old.grouped != v.grouped || old.mode != v.mode {
            self.with_focus_guard(|| self.attach_views());
            self.update_headers();
        }
        if old.icon_size() != v.icon_size() || old.mode != v.mode {
            self.build_factories();
        }
        if old.mode != v.mode {
            self.show_view();
        }
        self.sync_column_sort();
        if persist {
            let key = settings_key(&self.location());
            let mut fv = self.0.folder_views.borrow_mut();
            fv.set(&key, v);
            let _ = save_json(&crate::views_file(), &*fv);
        }
        self.emit(PaneEvent::Settings);
    }

    pub fn update_settings(&self, f: impl FnOnce(&mut ViewSettings)) {
        let mut v = self.settings();
        f(&mut v);
        self.apply_settings(v, true);
        self.update_subtitle();
        self.show_view();
    }

    pub fn set_filter_text(&self, text: &str) {
        let t = text.to_lowercase();
        if *self.0.filter_text.borrow() == t {
            return;
        }
        *self.0.filter_text.borrow_mut() = t;
        self.0.filter.changed(gtk::FilterChange::Different);
        self.update_subtitle();
        self.show_view();
    }

    pub fn toggle_filter_bar(&self) {
        let r = &self.0.filter_bar;
        if r.reveals_child() && self.0.filter_entry.has_focus() {
            self.close_filter_bar();
        } else {
            r.set_reveal_child(true);
            self.0.filter_entry.grab_focus();
        }
    }

    fn close_filter_bar(&self) {
        self.0.filter_entry.set_text("");
        self.0.filter_bar.set_reveal_child(false);
        self.focus_view();
    }

    pub fn focus_view(&self) {
        let _ = match self.settings().mode {
            ViewMode::Icons => self.0.icons.grab_focus(),
            ViewMode::Compact => self.0.compact.grab_focus(),
            ViewMode::Details => self.0.details.grab_focus(),
        };
    }

    pub fn set_active(&self, active: bool) {
        if active {
            self.0.root.add_css_class("active");
            self.0.root.remove_css_class("inactive");
        } else {
            self.0.root.remove_css_class("active");
            self.0.root.add_css_class("inactive");
        }
    }

    pub fn select_all(&self) {
        match self.settings().mode {
            ViewMode::Details => self.details_sel().select_all(),
            _ => self.0.flat_sel.select_all(),
        };
    }

    pub fn invert_selection(&self) {
        let sel = if self.settings().mode == ViewMode::Details {
            self.details_sel()
        } else {
            &self.0.flat_sel
        };
        let n = sel.n_items();
        let current = sel.selection();
        let inverted = gtk::Bitset::new_range(0, n);
        inverted.subtract(&current);
        sel.set_selection(&inverted, &gtk::Bitset::new_range(0, n));
    }

    // ------------------------------------------------------------- wiring

    fn wire(&self, filter_close: gtk::Button) {
        let weak = self.downgrade();
        self.0.filter_entry.connect_search_changed(move |e| {
            if let Some(p) = Pane::from_weak(&weak) {
                p.set_filter_text(&e.text());
            }
        });
        let weak = self.downgrade();
        self.0.filter_entry.connect_stop_search(move |_| {
            if let Some(p) = Pane::from_weak(&weak) {
                p.close_filter_bar();
            }
        });
        let weak = self.downgrade();
        filter_close.connect_clicked(move |_| {
            if let Some(p) = Pane::from_weak(&weak) {
                p.close_filter_bar();
            }
        });

        // Counts and empty state follow the visible model.
        let weak = self.downgrade();
        self.0.sort_model.connect_items_changed(move |_, _, _, _| {
            if let Some(p) = Pane::from_weak(&weak) {
                p.update_subtitle();
                if !p.is_loading() {
                    p.show_view();
                }
                p.emit(PaneEvent::Counts);
            }
        });
        for sel in [&self.0.flat_sel, &self.0.tree_sel] {
            let weak = self.downgrade();
            sel.connect_selection_changed(move |_, _, _| {
                if let Some(p) = Pane::from_weak(&weak) {
                    p.update_subtitle();
                    p.emit(PaneEvent::Selection);
                }
            });
        }

        // Activation.
        let weak = self.downgrade();
        self.0.icons.connect_activate(move |_, pos| {
            if let Some(p) = Pane::from_weak(&weak) {
                p.activate_flat(pos);
            }
        });
        let weak = self.downgrade();
        self.0.compact.connect_activate(move |_, pos| {
            if let Some(p) = Pane::from_weak(&weak) {
                p.activate_flat(pos);
            }
        });
        let weak = self.downgrade();
        self.0.details.connect_activate(move |_, pos| {
            if let Some(p) = Pane::from_weak(&weak)
                && let Some(item) = p.details_sel().item(pos).and_then(|o| unwrap_item(&o)) {
                    p.activate_item(&item);
                }
        });

        // Keyboard: type-ahead, Backspace, Ctrl+scroll zoom, middle click.
        for view in [
            self.0.icons.clone().upcast::<gtk::Widget>(),
            self.0.compact.clone().upcast(),
            self.0.details.clone().upcast(),
        ] {
            let keys = gtk::EventControllerKey::new();
            let weak = self.downgrade();
            keys.connect_key_pressed(move |_, key, _, state| {
                let Some(p) = Pane::from_weak(&weak) else { return glib::Propagation::Proceed };
                p.on_key(key, state)
            });
            view.add_controller(keys);

            let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
            let weak = self.downgrade();
            scroll.connect_scroll(move |c, _, dy| {
                if !c.current_event_state().contains(gdk::ModifierType::CONTROL_MASK) {
                    return glib::Propagation::Proceed;
                }
                if let Some(p) = Pane::from_weak(&weak) {
                    p.zoom(if dy < 0.0 { 1 } else { -1 });
                }
                glib::Propagation::Stop
            });
            view.add_controller(scroll);
        }

        // Pane becomes active on focus or click.
        let focus = gtk::EventControllerFocus::new();
        let weak = self.downgrade();
        focus.connect_enter(move |_| {
            if let Some(p) = Pane::from_weak(&weak) {
                p.emit(PaneEvent::Focused);
            }
        });
        self.0.root.add_controller(focus);
        let click = gtk::GestureClick::builder()
            .button(0)
            .propagation_phase(gtk::PropagationPhase::Capture)
            .build();
        let weak = self.downgrade();
        click.connect_pressed(move |g, _, _, _| {
            if let Some(p) = Pane::from_weak(&weak) {
                p.emit(PaneEvent::Focused);
                if g.current_button() == 2 {
                    // Middle click opens folders in a new tab.
                    if let Some(item) = p.item_under_pointer(g)
                        && item.is_dir() {
                            p.emit(PaneEvent::OpenInNewTab(item.target()));
                            g.set_state(gtk::EventSequenceState::Claimed);
                        }
                }
            }
        });
        self.0.root.add_controller(click);

        // Column header clicks change our sort settings.
        if let Some(sorter) = self.0.details.sorter() {
            let weak = self.downgrade();
            sorter.connect_changed(move |s, _| {
                let Some(p) = Pane::from_weak(&weak) else { return };
                if p.0.syncing_sort.get() {
                    return;
                }
                let Some(cv) = s.downcast_ref::<gtk::ColumnViewSorter>() else { return };
                let Some(col) = cv.primary_sort_column() else { return };
                let Some(key) = col.id().and_then(|id| SortKey::from_id(&id)) else { return };
                let descending = cv.primary_sort_order() == gtk::SortType::Descending;
                p.update_settings(|v| {
                    v.sort.key = key;
                    v.sort.descending = descending;
                });
            });
        }
    }

    fn item_under_pointer(&self, g: &gtk::GestureClick) -> Option<FileItem> {
        let (x, y) = g.point(None)?;
        let mut w = self.0.root.pick(x, y, gtk::PickFlags::DEFAULT)?;
        loop {
            if let Some(item) = unsafe { w.data::<glib::WeakRef<FileItem>>("strata-item") } {
                // SAFETY: only ever set to this type in bind_item.
                return unsafe { item.as_ref() }.upgrade();
            }
            w = w.parent()?;
        }
    }

    fn on_key(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        let mods = state & gtk::accelerator_get_default_mod_mask();
        if key == gdk::Key::BackSpace && mods.is_empty() {
            self.go_back();
            return glib::Propagation::Stop;
        }
        if (key == gdk::Key::Return || key == gdk::Key::KP_Enter)
            && mods == gdk::ModifierType::CONTROL_MASK
        {
            for item in self.selected_items() {
                if item.is_dir() {
                    self.emit(PaneEvent::OpenInNewTab(item.target()));
                }
            }
            return glib::Propagation::Stop;
        }
        if key == gdk::Key::Escape && self.0.filter_bar.reveals_child() {
            self.close_filter_bar();
            return glib::Propagation::Stop;
        }
        if !mods.is_empty() && mods != gdk::ModifierType::SHIFT_MASK {
            return glib::Propagation::Proceed;
        }
        let Some(c) = key.to_unicode() else { return glib::Propagation::Proceed };
        if c.is_control() || c == ' ' {
            return glib::Propagation::Proceed;
        }
        let now = glib::monotonic_time() as u64 / 1000;
        let query = self.0.typeahead.borrow_mut().push(c, now).to_string();
        self.typeahead_select(&query);
        glib::Propagation::Stop
    }

    fn typeahead_select(&self, query: &str) {
        let details = self.settings().mode == ViewMode::Details;
        let model: gio::ListModel =
            if details { self.details_sel().clone().upcast() } else { self.0.sort_model.clone().upcast() };
        let n = model.n_items();
        let names: Vec<String> = (0..n)
            .map(|i| model.item(i).and_then(|o| unwrap_item(&o)).map(|i| i.name()).unwrap_or_default())
            .collect();
        // Start at the current item so repeated letters cycle forward.
        let sel = if details { self.details_sel() } else { &self.0.flat_sel };
        let set = sel.selection();
        let start = if set.is_empty() {
            0
        } else if query.chars().count() == 1 {
            set.minimum() + 1
        } else {
            set.minimum()
        };
        let Some(pos) = typeahead::find(&names, query, start as usize) else { return };
        let flags = gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT;
        match self.settings().mode {
            ViewMode::Icons => self.0.icons.scroll_to(pos as u32, flags, None),
            ViewMode::Compact => self.0.compact.scroll_to(pos as u32, flags, None),
            ViewMode::Details => {
                self.0.details.scroll_to(pos as u32, None::<&gtk::ColumnViewColumn>, flags, None)
            }
        }
    }

    pub fn zoom(&self, delta: i32) {
        let mut v = self.settings();
        if v.zoom_by(delta) {
            self.apply_settings(v, true);
        }
    }

    fn activate_flat(&self, pos: u32) {
        if let Some(item) = self.0.flat_sel.item(pos).and_downcast::<FileItem>() {
            self.activate_item(&item);
        }
    }

    fn activate_item(&self, item: &FileItem) {
        if item.is_dir() {
            self.navigate(item.target());
            return;
        }
        let file = item.target();
        let window = self.0.root.root().and_downcast::<gtk::Window>();
        let weak = self.downgrade();
        let name = item.name();
        let launcher = gtk::FileLauncher::new(Some(&file));
        launcher.launch(window.as_ref(), gio::Cancellable::NONE, move |res| {
            if let Err(e) = res {
                if e.matches(gtk::DialogError::Dismissed) || e.matches(gtk::DialogError::Cancelled) {
                    return;
                }
                if let Some(p) = Pane::from_weak(&weak) {
                    p.emit(PaneEvent::Toast(format!(
                        "Couldn't open “{name}”. No app is set to open this kind of file."
                    )));
                }
            }
        });
    }

    // ---------------------------------------------------------- factories

    fn update_headers(&self) {
        let grouped = self.settings().grouped;
        let header = grouped.then(|| {
            let f = gtk::SignalListItemFactory::new();
            f.connect_setup(|_, obj| {
                let h = obj.downcast_ref::<gtk::ListHeader>().expect("ListHeader");
                let label = gtk::Label::builder().xalign(0.0).css_classes(["group-header"]).build();
                h.set_child(Some(&label));
            });
            let settings = self.0.settings.clone();
            f.connect_bind(move |_, obj| {
                let h = obj.downcast_ref::<gtk::ListHeader>().expect("ListHeader");
                let Some(label) = h.child().and_downcast::<gtk::Label>() else { return };
                let Some(item) = h.item().and_then(|o| unwrap_item(&o)) else { return };
                let s = settings.get();
                let text = if s.sort.folders_first && item.is_dir() && s.sort.key != SortKey::Size {
                    "Folders".to_string()
                } else {
                    item.with_group(s.sort.key, |g| g.label.clone())
                };
                label.set_label(&text);
            });
            f
        });
        self.0.details.set_header_factory(header.as_ref());
    }

    fn build_factories(&self) {
        let size = self.settings().icon_size() as i32;

        // Icons: large icon above a two-line name.
        let f = gtk::SignalListItemFactory::new();
        f.connect_setup(move |_, obj| {
            let li = obj.downcast_ref::<gtk::ListItem>().expect("ListItem");
            let image = gtk::Image::builder().pixel_size(size).build();
            let emblem = gtk::Image::builder()
                .icon_name("emblem-symbolic-link")
                .pixel_size((size / 3).max(12))
                .halign(gtk::Align::End)
                .valign(gtk::Align::End)
                .visible(false)
                .build();
            let overlay = gtk::Overlay::builder().child(&image).halign(gtk::Align::Center).build();
            overlay.add_overlay(&emblem);
            let label = gtk::Label::builder()
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .lines(2)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .justify(gtk::Justification::Center)
                .max_width_chars(1)
                .hexpand(true)
                .css_classes(["tile-name"])
                .build();
            let tile = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(8)
                .width_request((size + 48).max(104))
                .css_classes(["tile"])
                .build();
            tile.append(&overlay);
            tile.append(&label);
            li.set_child(Some(&tile));
        });
        f.connect_bind(|_, obj| {
            let li = obj.downcast_ref::<gtk::ListItem>().expect("ListItem");
            let Some(item) = li.item().and_downcast::<FileItem>() else { return };
            let Some(tile) = li.child() else { return };
            let overlay = tile.first_child().and_downcast::<gtk::Overlay>().expect("overlay");
            let image = overlay.child().and_downcast::<gtk::Image>().expect("image");
            let emblem = overlay.last_child().and_downcast::<gtk::Image>().expect("emblem");
            let label = tile.last_child().and_downcast::<gtk::Label>().expect("label");
            bind_item(&tile, &item);
            image.set_from_gicon(&item.icon());
            emblem.set_visible(item.is_symlink());
            label.set_label(&item.name());
            tile.set_tooltip_text(Some(&item.name()));
        });
        self.0.icons.set_factory(Some(&f));

        // Compact: small icon beside a one-line name.
        let csize = size.min(48);
        let f = gtk::SignalListItemFactory::new();
        f.connect_setup(move |_, obj| {
            let li = obj.downcast_ref::<gtk::ListItem>().expect("ListItem");
            let image = gtk::Image::builder().pixel_size(csize).build();
            let label = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .width_chars(18)
                .max_width_chars(28)
                .hexpand(true)
                .css_classes(["tile-name"])
                .build();
            let row = gtk::Box::builder().spacing(8).css_classes(["tile", "compact-tile"]).build();
            row.append(&image);
            row.append(&label);
            li.set_child(Some(&row));
        });
        f.connect_bind(|_, obj| {
            let li = obj.downcast_ref::<gtk::ListItem>().expect("ListItem");
            let Some(item) = li.item().and_downcast::<FileItem>() else { return };
            let Some(row) = li.child() else { return };
            let image = row.first_child().and_downcast::<gtk::Image>().expect("image");
            let label = row.last_child().and_downcast::<gtk::Label>().expect("label");
            bind_item(&row, &item);
            image.set_from_gicon(&item.icon());
            label.set_label(&item.name());
            row.set_tooltip_text(Some(&item.name()));
        });
        self.0.compact.set_factory(Some(&f));

        // Details: only the name column depends on zoom.
        if let Some(col) = self.0.details.columns().item(0).and_downcast::<gtk::ColumnViewColumn>() {
            col.set_factory(Some(&name_column_factory(size.min(64))));
        }
    }

    fn build_columns(&self) {
        let size = self.settings().icon_size() as i32;
        let name = gtk::ColumnViewColumn::builder()
            .title("Name")
            .id("name")
            .expand(true)
            .resizable(true)
            .fixed_width(220)
            .factory(&name_column_factory(size.min(64)))
            .sorter(&dummy_sorter())
            .build();
        let size_col = text_column("Size", "size", true, 90, |item| {
            if item.is_dir() { "—".into() } else { format::size(item.meta().size) }
        });
        let modified = text_column("Date Modified", "modified", false, 150, |item| {
            format::date_relative(item.meta().modified, chrono::Local::now())
        });
        let kind = text_column("Kind", "type", false, 150, |item| item.meta().type_name.clone());
        for c in [&name, &size_col, &modified, &kind] {
            self.0.details.append_column(c);
        }
        self.update_headers();
        self.sync_column_sort();
    }

    /// Reflect our sort settings in the Details column headers.
    fn sync_column_sort(&self) {
        let s = self.settings().sort;
        let cols = self.0.details.columns();
        for i in 0..cols.n_items() {
            let Some(col) = cols.item(i).and_downcast::<gtk::ColumnViewColumn>() else { continue };
            if col.id().as_deref() == Some(s.key.id()) {
                self.0.syncing_sort.set(true);
                self.0.details.sort_by_column(None, gtk::SortType::Ascending);
                let order =
                    if s.descending { gtk::SortType::Descending } else { gtk::SortType::Ascending };
                self.0.details.sort_by_column(Some(&col), order);
                self.0.syncing_sort.set(false);
                return;
            }
        }
        self.0.syncing_sort.set(true);
        self.0.details.sort_by_column(None, gtk::SortType::Ascending);
        self.0.syncing_sort.set(false);
    }
}

// ------------------------------------------------------------------ helpers

/// The item behind a list position, unwrapping tree rows.
pub fn unwrap_item(obj: &glib::Object) -> Option<FileItem> {
    if let Some(row) = obj.downcast_ref::<gtk::TreeListRow>() {
        return row.item().and_downcast::<FileItem>();
    }
    obj.downcast_ref::<FileItem>().cloned()
}

/// Tag a cell widget with its item, for middle-click lookup.
fn bind_item(widget: &gtk::Widget, item: &FileItem) {
    // SAFETY: the key is only ever read back as WeakRef<FileItem>.
    unsafe {
        widget.set_data("strata-item", item.downgrade());
    }
    widget.set_opacity(if item.is_hidden() { 0.6 } else { 1.0 });
}

fn dummy_sorter() -> gtk::CustomSorter {
    // Makes a column header clickable; real sorting happens in our model.
    gtk::CustomSorter::new(|_, _| gtk::Ordering::Equal)
}

fn name_column_factory(size: i32) -> gtk::SignalListItemFactory {
    let f = gtk::SignalListItemFactory::new();
    f.connect_setup(move |_, obj| {
        let li = obj.downcast_ref::<gtk::ColumnViewCell>().expect("ColumnViewCell");
        let image = gtk::Image::builder().pixel_size(size).build();
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .hexpand(true)
            .build();
        let b = gtk::Box::builder().spacing(8).build();
        b.append(&image);
        b.append(&label);
        let expander = gtk::TreeExpander::builder().child(&b).indent_for_icon(true).build();
        li.set_child(Some(&expander));
    });
    f.connect_bind(|_, obj| {
        let li = obj.downcast_ref::<gtk::ColumnViewCell>().expect("ColumnViewCell");
        let Some(obj) = li.item() else { return };
        let Some(item) = unwrap_item(&obj) else { return };
        let Some(expander) = li.child().and_downcast::<gtk::TreeExpander>() else { return };
        let row = obj.downcast::<gtk::TreeListRow>().ok();
        expander.set_indent_for_icon(row.is_some());
        expander.set_list_row(row.as_ref());
        let b = expander.child().expect("box");
        let image = b.first_child().and_downcast::<gtk::Image>().expect("image");
        let label = b.last_child().and_downcast::<gtk::Label>().expect("label");
        bind_item(&b, &item);
        image.set_from_gicon(&item.icon());
        label.set_label(&item.name());
    });
    f
}

fn text_column(
    title: &str,
    id: &str,
    numeric: bool,
    width: i32,
    text: impl Fn(&FileItem) -> String + 'static,
) -> gtk::ColumnViewColumn {
    let f = gtk::SignalListItemFactory::new();
    f.connect_setup(move |_, obj| {
        let li = obj.downcast_ref::<gtk::ColumnViewCell>().expect("ColumnViewCell");
        let label = gtk::Label::builder()
            .xalign(if numeric { 1.0 } else { 0.0 })
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["dim-label", "numeric"])
            .build();
        li.set_child(Some(&label));
    });
    f.connect_bind(move |_, obj| {
        let li = obj.downcast_ref::<gtk::ColumnViewCell>().expect("ColumnViewCell");
        let Some(item) = li.item().and_then(|o| unwrap_item(&o)) else { return };
        if let Some(label) = li.child().and_downcast::<gtk::Label>() {
            label.set_label(&text(&item));
        }
    });
    gtk::ColumnViewColumn::builder()
        .title(title)
        .id(id)
        .resizable(true)
        .fixed_width(width)
        .factory(&f)
        .sorter(&dummy_sorter())
        .build()
}

fn sort_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let by = gio::Menu::new();
    for k in SortKey::ALL {
        by.append(Some(k.label()), Some(&format!("win.sort-by::{}", k.id())));
    }
    menu.append_section(Some("Sort By"), &by);
    let order = gio::Menu::new();
    order.append(Some("Descending"), Some("win.sort-descending"));
    order.append(Some("Folders First"), Some("win.folders-first"));
    menu.append_section(None, &order);
    let show = gio::Menu::new();
    show.append(Some("Show in Groups"), Some("win.show-groups"));
    show.append(Some("Show Hidden Files"), Some("win.show-hidden"));
    menu.append_section(None, &show);
    menu
}

/// Key for per-folder settings: the local path, or the URI for remote places.
fn settings_key(file: &gio::File) -> PathBuf {
    file.path().unwrap_or_else(|| PathBuf::from(file.uri().as_str()))
}

pub fn folder_title(file: &gio::File) -> String {
    let home = strata_core::paths::home();
    if let Some(p) = file.path() {
        if p == home {
            return "Home".into();
        }
        if p.as_os_str() == "/" {
            return "Computer".into();
        }
    }
    match file.uri_scheme().as_deref() {
        Some("trash") if file.parent().is_none() => return "Trash".into(),
        Some("recent") => return "Recent".into(),
        _ => {}
    }
    file.query_info(
        "standard::display-name",
        gio::FileQueryInfoFlags::NONE,
        gio::Cancellable::NONE,
    )
    .ok()
    .map(|i| i.display_name().to_string())
    .or_else(|| file.basename().map(|b| b.to_string_lossy().into_owned()))
    .unwrap_or_else(|| file.uri().to_string())
}

/// Plain-language error for a failed listing, with what to do next.
pub fn describe_error(e: &glib::Error) -> (String, String) {
    use gio::IOErrorEnum as E;
    match e.kind::<E>() {
        Some(E::PermissionDenied) => (
            "You Don't Have Permission to Open This Folder".into(),
            "Ask the folder's owner for access, or check its permissions in Properties.".into(),
        ),
        Some(E::NotFound) => (
            "This Folder No Longer Exists".into(),
            "It may have been moved, renamed or deleted. Go back or choose another folder.".into(),
        ),
        Some(E::NotDirectory) => (
            "This Isn't a Folder".into(),
            "Open it from its parent folder instead.".into(),
        ),
        Some(E::NotMounted) => (
            "This Location Isn't Connected".into(),
            "Mount the drive or reconnect to the server, then try again.".into(),
        ),
        Some(E::NotSupported) => (
            "This Location Isn't Supported".into(),
            "Install the GVfs backends package for network and special locations.".into(),
        ),
        Some(E::TimedOut | E::HostNotFound | E::HostUnreachable | E::NetworkUnreachable) => (
            "Can't Reach the Server".into(),
            "Check your network connection and the server address, then try again.".into(),
        ),
        _ => ("Can't Open This Folder".into(), e.message().to_string()),
    }
}

/// List `dir`, calling `on_batch` with a small first batch (so something paints
/// at once) and then with the rest in a few large, pre-sorted chunks. Each
/// append makes the sort model merge, so fewer, sorted appends are far cheaper.
///
/// Local folders are read by the core's lister on a worker thread; other
/// locations (SFTP, SMB, trash, …) go through GIO.
pub async fn list_dir(
    dir: &gio::File,
    sort: SortOptions,
    mut on_batch: impl FnMut(Vec<FileItem>),
) -> Result<(), glib::Error> {
    let dbg = std::env::var_os("STRATA_DEBUG_TIMING").is_some();
    let started = Instant::now();
    let mut t_main = 0.0;
    if let Some(path) = dir.path() {
        let (tx, rx) = async_channel::bounded::<std::io::Result<Vec<RawEntry>>>(4);
        std::thread::Builder::new()
            .name("strata-list".into())
            .spawn(move || list_on_thread(&path, sort, &tx))
            .map_err(|e| io_to_glib(&e))?;
        while let Ok(msg) = rx.recv().await {
            let batch = msg.map_err(|e| io_to_glib(&e))?;
            let t = Instant::now();
            let items: Vec<FileItem> = batch.into_iter().map(|e| FileItem::from_raw(dir, e)).collect();
            let built = t.elapsed().as_secs_f64();
            on_batch(items);
            t_main += t.elapsed().as_secs_f64();
            if dbg {
                eprintln!("  batch: build {built:.3}s, append {:.3}s", t.elapsed().as_secs_f64() - built);
            }
        }
    } else {
        let en = dir
            .enumerate_children_future(ATTRS, gio::FileQueryInfoFlags::NONE, glib::Priority::DEFAULT)
            .await?;
        let mut first = true;
        loop {
            let infos = en.next_files_future(if first { 300 } else { 2000 }, glib::Priority::DEFAULT).await?;
            if infos.is_empty() {
                break;
            }
            first = false;
            let t = Instant::now();
            on_batch(infos.into_iter().map(|i| FileItem::from_child(dir, i)).collect());
            t_main += t.elapsed().as_secs_f64();
        }
    }
    if dbg {
        eprintln!(
            "list_dir: total {:.3}s, main thread {:.3}s",
            started.elapsed().as_secs_f64(),
            t_main
        );
    }
    Ok(())
}

fn list_on_thread(
    path: &std::path::Path,
    sort: SortOptions,
    tx: &async_channel::Sender<std::io::Result<Vec<RawEntry>>>,
) {
    const FLUSH_EVERY: std::time::Duration = std::time::Duration::from_millis(400);
    let cmp = |a: &RawEntry, b: &RawEntry| compare(&a.meta, &b.meta, &sort);
    let mut first = true;
    let mut pending: Vec<RawEntry> = Vec::new();
    let mut last = Instant::now();
    let result = listing::list_dir(path, 300, 2000, |mut batch| {
        if tx.is_closed() {
            return false;
        }
        if first {
            first = false;
            batch.sort_by(cmp);
            last = Instant::now();
            return tx.send_blocking(Ok(batch)).is_ok();
        }
        pending.append(&mut batch);
        if last.elapsed() >= FLUSH_EVERY {
            let mut out = std::mem::take(&mut pending);
            out.sort_by(cmp);
            last = Instant::now();
            return tx.send_blocking(Ok(out)).is_ok();
        }
        true
    });
    match result {
        Ok(()) => {
            if let Some(ms) = std::env::var("STRATA_LIST_DELAY_MS").ok().and_then(|v| v.parse().ok()) {
                std::thread::sleep(std::time::Duration::from_millis(ms));
            }
            if !pending.is_empty() {
                pending.sort_by(cmp);
                let _ = tx.send_blocking(Ok(pending));
            }
        }
        Err(e) => {
            let _ = tx.send_blocking(Err(e));
        }
    }
}

/// Map an I/O error onto the GIO error kinds the UI explains.
fn io_to_glib(e: &std::io::Error) -> glib::Error {
    use std::io::ErrorKind as K;
    let kind = match e.kind() {
        K::PermissionDenied => gio::IOErrorEnum::PermissionDenied,
        K::NotFound => gio::IOErrorEnum::NotFound,
        K::NotADirectory => gio::IOErrorEnum::NotDirectory,
        K::TimedOut => gio::IOErrorEnum::TimedOut,
        _ => gio::IOErrorEnum::Failed,
    };
    glib::Error::new(kind, &e.to_string())
}

/// Fill `store` with the contents of `dir` in the background.
fn spawn_list_into(dir: gio::File, store: gio::ListStore, sort: SortOptions) {
    glib::MainContext::default().spawn_local(async move {
        let _ = list_dir(&dir, sort, |batch| store.extend_from_slice(&batch)).await;
    });
}
