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
use kivo_core::format;
use kivo_core::group::compare_groups;
use kivo_core::settings::{FolderViews, ViewMode, ViewSettings, save_json};
use kivo_core::listing::{self, RawEntry};
use kivo_core::sort::{SortKey, SortOptions, compare};
use kivo_core::typeahead;

use crate::item::{ATTRS, FileItem};

#[derive(Debug, Clone)]
pub enum PaneEvent {
    /// Location, history or loading state changed.
    Location,
    Selection,
    Counts,
    Focused,
    Settings,
    /// Middle-click or Ctrl+Enter on a folder.
    OpenInNewTab(gio::File),
    /// Double-click / Enter on files (not folders).
    Open(Vec<FileItem>),
    /// Files dropped onto `target` (a folder).
    Drop { files: Vec<gio::File>, target: gio::File, action: gdk::DragAction },
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
    searching: Rc<Cell<bool>>,
    search_status: RefCell<String>,
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
    /// Names to select as soon as they appear (after paste, new folder, …).
    pending_select: RefCell<std::collections::HashSet<std::ffi::OsString>>,
    menu: RefCell<Option<gtk::PopoverMenu>>,
    drop_hover: RefCell<Option<gtk::Widget>>,
    drop_action: Cell<gdk::DragAction>,
}

#[derive(Clone)]
pub struct Pane(Rc<Inner>);

impl Pane {
    pub fn new(folder_views: Rc<RefCell<FolderViews>>, location: gio::File) -> Self {
        let settings = Rc::new(Cell::new(ViewSettings::default()));
        let filter_text = Rc::new(RefCell::new(String::new()));
        let searching = Rc::new(Cell::new(false));

        let store = gio::ListStore::new::<FileItem>();

        let filter = {
            let settings = settings.clone();
            let text = filter_text.clone();
            let searching = searching.clone();
            gtk::CustomFilter::new(move |obj| {
                let item = obj.downcast_ref::<FileItem>().expect("FileItem");
                // Search already decided about hidden items.
                if !searching.get() && !settings.get().show_hidden && item.is_hidden() {
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
            let searching = searching.clone();
            gtk::CustomSorter::new(move |a, b| {
                let a = a.downcast_ref::<FileItem>().expect("FileItem");
                let b = b.downcast_ref::<FileItem>().expect("FileItem");
                if searching.get() {
                    // Search results: best match first.
                    return b.score().cmp(&a.score()).then_with(|| compare(&a.meta(), &b.meta(), &settings.get().sort)).into();
                }
                compare(&a.meta(), &b.meta(), &settings.get().sort).into()
            })
        };
        let section_sorter = {
            let settings = settings.clone();
            let searching = searching.clone();
            gtk::CustomSorter::new(move |a, b| {
                let s = settings.get();
                if !s.grouped || searching.get() {
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
            .icon_name("kivo-sort-symbolic")
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
            .icon_name("kivo-close-symbolic")
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
            searching,
            search_status: RefCell::new(String::new()),
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
            pending_select: RefCell::new(Default::default()),
            menu: RefCell::new(None),
            drop_hover: RefCell::new(None),
            drop_action: Cell::new(gdk::DragAction::empty()),
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
        self.set_searching(false);

        // Per-folder view settings, keeping the filter bar's text.
        let key = settings_key(&file);
        let v = self.0.folder_views.borrow().get(&key);
        self.apply_settings(v, false);

        self.0.pending_select.borrow_mut().clear();
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
                    pane.refresh_git();
                    if std::env::var_os("KIVO_DEBUG_TIMING").is_some() {
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
            pane.apply_pending_selection();
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
        self.0.status.set_icon_name(Some("kivo-warning-symbolic"));
        self.0.status.set_title(&title);
        self.0.status.set_description(Some(&hint));
        self.0.stack.set_visible_child_name("status");
    }

    fn show_view(&self) {
        let empty = self.0.sort_model.n_items() == 0;
        if self.0.stack.visible_child_name().as_deref() == Some("status")
            && self.0.status.icon_name().as_deref() == Some("kivo-warning-symbolic")
            && !self.0.loading.get()
            && self.store().n_items() == 0
        {
            // Keep showing the error page.
            return;
        }
        if empty && !self.0.loading.get() {
            let filtered = !self.0.filter_text.borrow().is_empty();
            let hidden_only = self.store().n_items() > 0;
            self.0.status.set_icon_name(Some("kivo-folder-symbolic"));
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
        if self.0.searching.get() {
            let mut text = self.0.search_status.borrow().clone();
            let sel = self.selected_items();
            if !sel.is_empty() {
                text.push_str(&format!(" · {} selected", sel.len()));
            }
            self.0.subtitle.set_label(&text);
            return;
        }
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

    // ------------------------------------------------------------ search

    fn set_searching(&self, on: bool) {
        if self.0.searching.replace(on) == on {
            return;
        }
        // The "Where" column only makes sense for results from many folders.
        let cols = self.0.details.columns();
        for i in 0..cols.n_items() {
            if let Some(c) = cols.item(i).and_downcast::<gtk::ColumnViewColumn>()
                && c.id().as_deref() == Some("where") {
                    c.set_visible(on);
                }
        }
        self.0.filter.changed(gtk::FilterChange::Different);
        self.0.sorter.changed(gtk::SorterChange::Different);
        self.0.section_sorter.changed(gtk::SorterChange::Different);
        // Results are ranked by relevance: no column shows a sort arrow.
        if on {
            self.0.syncing_sort.set(true);
            self.0.details.sort_by_column(None, gtk::SortType::Ascending);
            self.0.syncing_sort.set(false);
        } else {
            self.sync_column_sort();
        }
    }

    /// Replace the view's contents with search results.
    pub fn show_results(&self, items: Vec<FileItem>, status: &str) {
        if let Some(task) = self.0.load_task.borrow_mut().take() {
            task.abort();
        }
        *self.0.monitor.borrow_mut() = None;
        self.0.loading.set(false);
        self.set_searching(true);
        *self.0.search_status.borrow_mut() = status.to_string();
        self.0.title.set_label("Search Results");
        let store = gio::ListStore::new::<FileItem>();
        store.extend_from_slice(&items);
        self.replace_store(store);
        self.show_search_view();
        self.update_subtitle();
        self.emit(PaneEvent::Counts);
    }

    /// More results from a streaming (live) search.
    pub fn append_results(&self, items: Vec<FileItem>, status: &str) {
        if !self.0.searching.get() {
            return;
        }
        *self.0.search_status.borrow_mut() = status.to_string();
        self.store().extend_from_slice(&items);
        self.show_search_view();
        self.update_subtitle();
    }

    fn show_search_view(&self) {
        if self.0.sort_model.n_items() == 0 {
            self.0.status.set_icon_name(Some("kivo-search-symbolic"));
            self.0.status.set_title("No Results");
            self.0.status.set_description(Some("Try fewer words, check the spelling, or search everywhere instead of this folder."));
            self.0.stack.set_visible_child_name("status");
        } else {
            self.0.stack.set_visible_child_name(self.settings().mode.id());
        }
    }

    /// Rebind every cell (icon style or accent changed).
    pub fn refresh_icons(&self) {
        self.build_factories();
    }

    /// Leave search results and show the folder again.
    pub fn end_search(&self) {
        if self.0.searching.get() {
            self.load(self.location());
        }
    }

    // ------------------------------------------------------- live updates

    /// Fetch the folder's git status off the main thread and badge items.
    fn refresh_git(&self) {
        let Some(dir) = self.location().path() else { return };
        let weak = self.downgrade();
        glib::MainContext::default().spawn_local(async move {
            let d = dir.clone();
            let Ok(Some(st)) = gio::spawn_blocking(move || kivo_core::git::status(&d)).await else { return };
            let Some(pane) = Pane::from_weak(&weak) else { return };
            if pane.location().path().as_deref() != Some(dir.as_path()) {
                return;
            }
            let store = pane.store();
            for i in 0..store.n_items() {
                if let Some(item) = store.item(i).and_downcast::<FileItem>()
                    && let Some(p) = item.file().path() {
                        item.set_git(Some(st.state_of(&p)));
                    }
            }
            for w in [pane.0.icons.upcast_ref::<gtk::Widget>(), pane.0.compact.upcast_ref(), pane.0.details.upcast_ref()] {
                refresh_git_badges(w);
            }
        });
    }

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
            self.with_focus_guard(|| self.store().remove(pos));
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
                Some(pos) => pane.with_focus_guard(|| pane.store().splice(pos, 1, &[item])),
                None => pane.store().append(&item),
            }
            pane.apply_pending_selection();
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
        if old.icon_size() != v.icon_size() || old.mode != v.mode || old.previews != v.previews {
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

        // Right-click menus.
        let rclick = gtk::GestureClick::builder().button(gdk::BUTTON_SECONDARY).build();
        let weak = self.downgrade();
        rclick.connect_pressed(move |g, _, x, y| {
            let Some(p) = Pane::from_weak(&weak) else { return };
            // Only inside the file area, not the heading.
            let in_view = p.0.root.pick(x, y, gtk::PickFlags::DEFAULT).is_some_and(|w| w.is_ancestor(&p.0.stack) || w == p.0.stack.clone().upcast::<gtk::Widget>());
            if !in_view {
                return;
            }
            g.set_state(gtk::EventSequenceState::Claimed);
            let item = item_at(p.0.root.upcast_ref(), x, y).map(|(i, _)| i);
            p.show_context_menu(x, y, item);
        });
        self.0.root.add_controller(rclick);
        for view in [
            self.0.icons.clone().upcast::<gtk::Widget>(),
            self.0.compact.clone().upcast(),
            self.0.details.clone().upcast(),
        ] {
            self.setup_drop_target(&view);
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
        item_at(self.0.root.upcast_ref(), x, y).map(|(i, _)| i)
    }

    // ------------------------------------------------------- selection API

    fn current_sel(&self) -> &gtk::MultiSelection {
        if self.settings().mode == ViewMode::Details { self.details_sel() } else { &self.0.flat_sel }
    }

    fn position_in_view(&self, item: &FileItem) -> Option<u32> {
        let sel = self.current_sel();
        (0..sel.n_items()).find(|&i| sel.item(i).and_then(|o| unwrap_item(&o)).as_ref() == Some(item))
    }

    fn is_selected(&self, item: &FileItem) -> bool {
        self.position_in_view(item).is_some_and(|p| self.current_sel().is_selected(p))
    }

    /// Select only `item` (used when right-clicking or dragging an
    /// unselected item).
    fn select_only(&self, item: &FileItem) {
        if let Some(p) = self.position_in_view(item) {
            self.current_sel().select_item(p, true);
        }
    }

    /// Move the selection by `delta` items (Quick Look's arrow keys) and
    /// return the newly selected item.
    pub fn neighbor(&self, delta: i32) -> Option<FileItem> {
        let sel = self.current_sel().clone();
        let n = sel.n_items();
        if n == 0 {
            return None;
        }
        let set = sel.selection();
        let cur = if set.is_empty() { 0 } else { set.minimum() };
        let new = (cur as i64 + delta as i64).clamp(0, n as i64 - 1) as u32;
        if new == cur && !set.is_empty() {
            return None;
        }
        let flags = gtk::ListScrollFlags::SELECT | gtk::ListScrollFlags::FOCUS;
        match self.settings().mode {
            ViewMode::Icons => self.0.icons.scroll_to(new, flags, None),
            ViewMode::Compact => self.0.compact.scroll_to(new, flags, None),
            ViewMode::Details => self.0.details.scroll_to(new, None::<&gtk::ColumnViewColumn>, flags, None),
        }
        sel.item(new).and_then(|o| unwrap_item(&o))
    }

    /// Select these on-disk names in the current folder, now or as soon as
    /// they appear. Replaces the current selection.
    pub fn select_names(&self, names: Vec<std::ffi::OsString>) {
        if names.is_empty() {
            return;
        }
        self.current_sel().unselect_all();
        *self.0.pending_select.borrow_mut() = names.into_iter().collect();
        self.apply_pending_selection();
    }

    fn apply_pending_selection(&self) {
        if self.0.pending_select.borrow().is_empty() {
            return;
        }
        let sel = self.current_sel().clone();
        let mut first = None;
        for i in 0..sel.n_items() {
            let Some(item) = sel.item(i).and_then(|o| unwrap_item(&o)) else { continue };
            if self.0.pending_select.borrow_mut().remove(&item.raw_name()) {
                sel.select_item(i, false);
                first.get_or_insert(i);
            }
        }
        if let Some(i) = first {
            let flags = gtk::ListScrollFlags::FOCUS;
            match self.settings().mode {
                ViewMode::Icons => self.0.icons.scroll_to(i, flags, None),
                ViewMode::Compact => self.0.compact.scroll_to(i, flags, None),
                ViewMode::Details => self.0.details.scroll_to(i, None::<&gtk::ColumnViewColumn>, flags, None),
            }
        }
    }

    /// Is the current folder somewhere we can create files?
    pub fn is_writable(&self) -> bool {
        let loc = self.location();
        if loc.uri_scheme().as_deref() == Some("trash") {
            return false;
        }
        match loc.path() {
            Some(p) => {
                use std::os::unix::ffi::OsStrExt;
                let c = std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap_or_default();
                // SAFETY: NUL-terminated path.
                unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
            }
            None => true,
        }
    }

    pub fn in_trash(&self) -> bool {
        self.location().uri_scheme().as_deref() == Some("trash")
    }

    // -------------------------------------------------------- context menu

    pub(crate) fn show_context_menu(&self, x: f64, y: f64, on_item: Option<FileItem>) {
        let menu = match &on_item {
            Some(item) => {
                if !self.is_selected(item) {
                    self.select_only(item);
                }
                let sel = self.selected_items();
                crate::menus::item_menu(&crate::menus::Selection {
                    count: sel.len(),
                    has_archive: sel.iter().any(|i| !i.is_dir() && crate::ops_ui::looks_like_archive(i)),
                    has_installable: sel.iter().any(|i| !i.is_dir() && i.path().is_some_and(|p| kivo_core::install::is_installable(&p))),
                    has_dir: sel.iter().any(|i| i.is_dir()),
                    has_file: sel.iter().any(|i| !i.is_dir()),
                    in_trash: self.in_trash(),
                    services: if self.in_trash() { Vec::new() } else { crate::services_ui::applicable(&sel) },
                })
            }
            None => {
                self.current_sel().unselect_all();
                crate::menus::background_menu(self.in_trash(), self.is_writable())
            }
        };
        if let Some(old) = self.0.menu.borrow_mut().take() {
            old.unparent();
        }
        let pop = gtk::PopoverMenu::from_model(Some(&menu));
        pop.set_parent(&self.0.root);
        pop.set_has_arrow(false);
        pop.set_halign(gtk::Align::Start);
        pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        let weak = self.downgrade();
        pop.connect_closed(move |p| {
            let p = p.clone();
            let weak = weak.clone();
            // Unparent after the menu action has run.
            glib::idle_add_local_once(move || {
                if let Some(pane) = Pane::from_weak(&weak) {
                    let mut m = pane.0.menu.borrow_mut();
                    if m.as_ref() == Some(&p) {
                        *m = None;
                    }
                }
                if p.parent().is_some() {
                    p.unparent();
                }
            });
        });
        pop.popup();
        *self.0.menu.borrow_mut() = Some(pop);
    }

    /// The open context menu, if any (test hooks capture it).
    pub fn open_menu(&self) -> Option<gtk::PopoverMenu> {
        self.0.menu.borrow().clone().filter(|m| m.is_visible())
    }

    /// Context menu for the selection, opened from the keyboard.
    pub fn keyboard_menu(&self) {
        let sel = self.selected_items();
        let (w, h) = (self.0.root.width() as f64, self.0.root.height() as f64);
        self.show_context_menu(w / 3.0, h / 3.0, sel.first().cloned());
    }

    // ------------------------------------------------------- drag and drop

    fn setup_drag_source(&self, cell: &gtk::Widget) {
        let src = gtk::DragSource::builder().actions(gdk::DragAction::COPY | gdk::DragAction::MOVE | gdk::DragAction::LINK).build();
        let weak = self.downgrade();
        let cell_weak = cell.downgrade();
        src.connect_prepare(move |src, _, _| {
            let pane = Pane::from_weak(&weak)?;
            let cell = cell_weak.upgrade()?;
            // SAFETY: only ever set to WeakRef<FileItem> in bind_item.
            let item = unsafe { cell.data::<glib::WeakRef<FileItem>>("kivo-item") }
                .and_then(|w| unsafe { w.as_ref() }.upgrade())?;
            if pane.in_trash() {
                return None;
            }
            if !pane.is_selected(&item) {
                pane.select_only(&item);
            }
            let files: Vec<gio::File> = pane.selected_items().iter().map(|i| i.target()).collect();
            let theme = gtk::IconTheme::for_display(&cell.display());
            let icon = theme.lookup_by_gicon(&item.icon(), 48, 1, gtk::TextDirection::None, gtk::IconLookupFlags::empty());
            src.set_icon(Some(&icon), 24, 24);
            let list = gdk::FileList::from_array(&files);
            Some(gdk::ContentProvider::for_value(&list.to_value()))
        });
        cell.add_controller(src);
    }

    fn setup_drop_target(&self, view: &gtk::Widget) {
        let target = gtk::DropTarget::new(
            gdk::FileList::static_type(),
            gdk::DragAction::COPY | gdk::DragAction::MOVE | gdk::DragAction::LINK,
        );
        target.set_preload(true);
        let weak = self.downgrade();
        let v = view.downgrade();
        target.connect_motion(move |t, x, y| {
            let (Some(pane), Some(view)) = (Pane::from_weak(&weak), v.upgrade()) else {
                return gdk::DragAction::empty();
            };
            let (dir, cell) = match item_at(&view, x, y) {
                Some((item, cell)) if item.is_dir() => (item.target(), Some(cell)),
                _ => (pane.location(), None),
            };
            pane.set_drop_hover(cell);
            let files = t.value().and_then(|v| v.get::<gdk::FileList>().ok()).map(|l| l.files()).unwrap_or_default();
            if files.iter().any(|f| f.equal(&dir)) || pane.in_trash() {
                pane.0.drop_action.set(gdk::DragAction::empty());
                return gdk::DragAction::empty();
            }
            let state = t.current_event_state();
            let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
            let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
            let action = match (ctrl, shift) {
                (true, true) => gdk::DragAction::LINK,
                (true, false) => gdk::DragAction::COPY,
                (false, true) => gdk::DragAction::MOVE,
                _ => {
                    // Like every file manager: move within a drive, copy across.
                    let same = match (files.first().and_then(|f| f.path()), dir.path()) {
                        (Some(a), Some(b)) => same_device(&a, &b),
                        _ => false,
                    };
                    if same { gdk::DragAction::MOVE } else { gdk::DragAction::COPY }
                }
            };
            pane.0.drop_action.set(action);
            action
        });
        let weak = self.downgrade();
        target.connect_leave(move |_| {
            if let Some(p) = Pane::from_weak(&weak) {
                p.set_drop_hover(None);
            }
        });
        let weak = self.downgrade();
        let v = view.downgrade();
        target.connect_drop(move |_, value, x, y| {
            let (Some(pane), Some(view)) = (Pane::from_weak(&weak), v.upgrade()) else { return false };
            pane.set_drop_hover(None);
            let Ok(list) = value.get::<gdk::FileList>() else { return false };
            let dir = match item_at(&view, x, y) {
                Some((item, _)) if item.is_dir() => item.target(),
                _ => pane.location(),
            };
            let action = pane.0.drop_action.get();
            let files = list.files();
            if action.is_empty() || files.is_empty() {
                return false;
            }
            // Dropping into the folder the items already live in does nothing.
            if action == gdk::DragAction::MOVE && files.iter().all(|f| f.parent().is_some_and(|p| p.equal(&dir))) {
                return false;
            }
            pane.emit(PaneEvent::Drop { files, target: dir, action });
            true
        });
        view.add_controller(target);
    }

    fn set_drop_hover(&self, cell: Option<gtk::Widget>) {
        let mut cur = self.0.drop_hover.borrow_mut();
        if *cur == cell {
            return;
        }
        if let Some(old) = cur.take() {
            old.remove_css_class("drop-target");
        }
        if let Some(c) = &cell {
            c.add_css_class("drop-target");
        }
        *cur = cell;
    }

    fn on_key(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        let mods = state & gtk::accelerator_get_default_mod_mask();
        // File shortcuts live here, not on the window, so they never fire
        // while typing in the location bar or a filter.
        // Space: Quick Look, unless the user is typing a name with spaces.
        if key == gdk::Key::space && mods.is_empty() {
            let now = glib::monotonic_time() as u64 / 1000;
            if self.0.typeahead.borrow().active(now) {
                let query = self.0.typeahead.borrow_mut().push(' ', now).to_string();
                self.typeahead_select(&query);
            } else {
                let _ = self.0.root.activate_action("win.quick-look", None);
            }
            return glib::Propagation::Stop;
        }
        let lower = key.to_lower();
        for (action, accels, _, _) in crate::VIEW_SHORTCUTS {
            for accel in *accels {
                if let Some((k, m)) = gtk::accelerator_parse(*accel)
                    && k.to_lower() == lower && m == mods {
                        let (name, target) = gio::Action::parse_detailed_name(action).expect("valid action");
                        let _ = self.0.root.activate_action(&name, target.as_ref());
                        return glib::Propagation::Stop;
                    }
            }
        }
        if key == gdk::Key::Menu || (key == gdk::Key::F10 && mods == gdk::ModifierType::SHIFT_MASK) {
            self.keyboard_menu();
            return glib::Propagation::Stop;
        }
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
        self.emit(PaneEvent::Open(vec![item.clone()]));
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
        let weak_icons = self.downgrade();
        let weak_compact = self.downgrade();
        let weak_details = self.downgrade();
        let previews = self.settings().previews;

        // Icons: large icon above a two-line name.
        let f = gtk::SignalListItemFactory::new();
        f.connect_setup(move |_, obj| {
            let li = obj.downcast_ref::<gtk::ListItem>().expect("ListItem");
            let image = gtk::Image::builder().pixel_size(size).build();
            let emblem = gtk::Image::builder()
                .icon_name("kivo-link-symbolic")
                .pixel_size((size / 3).max(12))
                .halign(gtk::Align::End)
                .valign(gtk::Align::End)
                .visible(false)
                .build();
            let overlay = gtk::Overlay::builder().child(&image).halign(gtk::Align::Center).build();
            overlay.add_overlay(&git_badge(gtk::Align::Start, gtk::Align::End));
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
            // Break long names between characters, but never add hyphens.
            let attrs = gtk::pango::AttrList::new();
            attrs.insert(gtk::pango::AttrInt::new_insert_hyphens(false));
            label.set_attributes(Some(&attrs));
            let tile = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(8)
                .width_request((size + 48).max(104))
                .css_classes(["tile"])
                .build();
            tile.append(&overlay);
            tile.append(&label);
            if let Some(p) = Pane::from_weak(&weak_icons) {
                p.setup_drag_source(tile.upcast_ref());
            }
            li.set_child(Some(&tile));
        });
        f.connect_bind(move |_, obj| {
            let li = obj.downcast_ref::<gtk::ListItem>().expect("ListItem");
            let Some(item) = li.item().and_downcast::<FileItem>() else { return };
            let Some(tile) = li.child() else { return };
            let overlay = tile.first_child().and_downcast::<gtk::Overlay>().expect("overlay");
            let image = overlay.child().and_downcast::<gtk::Image>().expect("image");
            let emblem = overlay.last_child().and_downcast::<gtk::Image>().expect("emblem");
            let label = tile.last_child().and_downcast::<gtk::Label>().expect("label");
            bind_item(&tile, &item);
            image.set_from_gicon(&item.icon());
            crate::thumbs_ui::reset(&image);
            if previews && size >= 48 {
                crate::thumbs_ui::apply(&image, &item, size as u32);
            }
            emblem.set_visible(item.is_symlink());
            if let Some(b) = emblem.prev_sibling() {
                bind_git(&b, &item);
            }
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
            row.append(&git_badge(gtk::Align::Center, gtk::Align::Center));
            if let Some(p) = Pane::from_weak(&weak_compact) {
                p.setup_drag_source(row.upcast_ref());
            }
            li.set_child(Some(&row));
        });
        f.connect_bind(move |_, obj| {
            let li = obj.downcast_ref::<gtk::ListItem>().expect("ListItem");
            let Some(item) = li.item().and_downcast::<FileItem>() else { return };
            let Some(row) = li.child() else { return };
            let image = row.first_child().and_downcast::<gtk::Image>().expect("image");
            let label = image.next_sibling().and_downcast::<gtk::Label>().expect("label");
            bind_item(&row, &item);
            if let Some(b) = row.last_child() {
                bind_git(&b, &item);
            }
            image.set_from_gicon(&item.icon());
            crate::thumbs_ui::reset(&image);
            if previews && csize >= 32 {
                crate::thumbs_ui::apply(&image, &item, csize as u32);
            }
            label.set_label(&item.name());
            row.set_tooltip_text(Some(&item.name()));
        });
        self.0.compact.set_factory(Some(&f));

        // Details: only the name column depends on zoom.
        if let Some(col) = self.0.details.columns().item(0).and_downcast::<gtk::ColumnViewColumn>() {
            col.set_factory(Some(&name_column_factory(size.min(64), weak_details)));
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
            .factory(&name_column_factory(size.min(64), self.downgrade()))
            .sorter(&dummy_sorter())
            .build();
        let size_col = text_column("Size", "size", true, 90, |item| {
            if item.is_dir() { "—".into() } else { format::size(item.meta().size) }
        });
        let modified = text_column("Date Modified", "modified", false, 150, |item| {
            format::date_relative(item.meta().modified, chrono::Local::now())
        });
        let kind = text_column("Kind", "type", false, 150, |item| item.meta().type_name.clone());
        let home = kivo_core::paths::home();
        let where_ = text_column("Where", "where", false, 220, move |item| {
            item.file().parent().and_then(|p| p.path()).map(|p| kivo_core::paths::display(&p, &home)).unwrap_or_default()
        });
        where_.set_visible(false);
        for c in [&name, &size_col, &modified, &kind, &where_] {
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

/// The item (and its cell widget) at (x, y) in `widget`'s coordinates.
fn item_at(widget: &gtk::Widget, x: f64, y: f64) -> Option<(FileItem, gtk::Widget)> {
    let mut w = widget.pick(x, y, gtk::PickFlags::DEFAULT)?;
    loop {
        // SAFETY: only ever set to WeakRef<FileItem> in bind_item.
        if let Some(item) = unsafe { w.data::<glib::WeakRef<FileItem>>("kivo-item") } {
            return unsafe { item.as_ref() }.upgrade().map(|i| (i, w.clone()));
        }
        if &w == widget {
            return None;
        }
        w = w.parent()?;
    }
}

fn same_device(a: &std::path::Path, b: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev(),
        _ => false,
    }
}

/// Tag a cell widget with its item, for middle-click lookup.
/// A small coloured dot showing an item's git state.
fn git_badge(h: gtk::Align, v: gtk::Align) -> gtk::Widget {
    let b = gtk::Box::builder().css_classes(["git-badge"]).halign(h).valign(v).visible(false).build();
    b.upcast()
}

const GIT_CLASSES: &[&str] = &["git-untracked", "git-renamed", "git-added", "git-deleted", "git-modified", "git-conflict"];

fn bind_git(badge: &gtk::Widget, item: &FileItem) {
    for c in GIT_CLASSES {
        badge.remove_css_class(c);
    }
    match item.git().filter(|s| *s != kivo_core::git::State::Clean) {
        Some(s) => {
            badge.add_css_class(&format!("git-{}", s.id()));
            badge.set_tooltip_text(Some(&format!("Git: {}", s.label())));
            badge.update_property(&[gtk::accessible::Property::Label(&format!("Git: {}", s.label()))]);
            badge.set_visible(true);
        }
        None => badge.set_visible(false),
    }
}

/// Re-apply git badges on the cells that are on screen.
fn refresh_git_badges(w: &gtk::Widget) {
    if w.has_css_class("git-badge") {
        let mut a = w.parent();
        while let Some(p) = a {
            // SAFETY: only ever set to WeakRef<FileItem> in bind_item.
            if let Some(item) = unsafe { p.data::<glib::WeakRef<FileItem>>("kivo-item") } {
                if let Some(item) = unsafe { item.as_ref() }.upgrade() {
                    bind_git(w, &item);
                }
                break;
            }
            a = p.parent();
        }
        return;
    }
    let mut c = w.first_child();
    while let Some(ch) = c {
        refresh_git_badges(&ch);
        c = ch.next_sibling();
    }
}

fn bind_item(widget: &gtk::Widget, item: &FileItem) {
    // SAFETY: the key is only ever read back as WeakRef<FileItem>.
    unsafe {
        widget.set_data("kivo-item", item.downgrade());
    }
    widget.set_opacity(if item.is_hidden() { 0.6 } else { 1.0 });
}

fn dummy_sorter() -> gtk::CustomSorter {
    // Makes a column header clickable; real sorting happens in our model.
    gtk::CustomSorter::new(|_, _| gtk::Ordering::Equal)
}

fn name_column_factory(size: i32, pane: Weak<Inner>) -> gtk::SignalListItemFactory {
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
        b.append(&git_badge(gtk::Align::Center, gtk::Align::Center));
        if let Some(p) = Pane::from_weak(&pane) {
            p.setup_drag_source(b.upcast_ref());
        }
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
        let label = image.next_sibling().and_downcast::<gtk::Label>().expect("label");
        bind_item(&b, &item);
        if let Some(g) = b.last_child() {
            bind_git(&g, &item);
        }
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
    show.append(Some("Show Previews"), Some("win.show-previews"));
    menu.append_section(None, &show);
    menu
}

/// Key for per-folder settings: the local path, or the URI for remote places.
fn settings_key(file: &gio::File) -> PathBuf {
    file.path().unwrap_or_else(|| PathBuf::from(file.uri().as_str()))
}

pub fn folder_title(file: &gio::File) -> String {
    let home = kivo_core::paths::home();
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
    let dbg = std::env::var_os("KIVO_DEBUG_TIMING").is_some();
    let started = Instant::now();
    let mut t_main = 0.0;
    if let Some(path) = dir.path() {
        let (tx, rx) = async_channel::bounded::<std::io::Result<Vec<RawEntry>>>(4);
        std::thread::Builder::new()
            .name("kivo-list".into())
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
            if let Some(ms) = std::env::var("KIVO_LIST_DELAY_MS").ok().and_then(|v| v.parse().ok()) {
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
