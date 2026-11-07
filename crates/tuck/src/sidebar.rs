// SPDX-License-Identifier: GPL-3.0-or-later
//! Places sidebar, in foldable sections: Favourites, Bookmarks, Drives
//! (with how full each is), Network and Tags.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use tuck_core::format;
use tuck_core::paths::{UserDir, UserDirs};

#[derive(Clone)]
enum Target {
    File(gio::File),
    /// An unmounted volume; activating it mounts it first.
    Volume(gio::Volume),
    /// A tag: activating it searches for files with it.
    Tag(String),
    /// "Connect to Server…".
    Connect,
    /// A section heading: activating it folds or unfolds the section.
    Section(String),
}

type OnOpen = Rc<dyn Fn(gio::File)>;
type OnMessage = Rc<dyn Fn(String)>;
type OnTag = Rc<dyn Fn(String)>;
type OnDrop = Rc<dyn Fn(Vec<gio::File>, gio::File, gtk::gdk::DragAction)>;
type OnConnect = Rc<dyn Fn()>;

pub struct Inner {
    root: gtk::Box,
    list: gtk::ListBox,
    rows: RefCell<Vec<(gtk::ListBoxRow, Option<Target>)>>,
    monitor: gio::VolumeMonitor,
    stack: gtk::Stack,
    folders: crate::folders_ui::FoldersPanel,
    folders_button: gtk::ToggleButton,
    places_button: gtk::ToggleButton,
    on_open: RefCell<Option<OnOpen>>,
    on_message: RefCell<Option<OnMessage>>,
    on_tag: RefCell<Option<OnTag>>,
    on_drop: RefCell<Option<OnDrop>>,
    on_connect: RefCell<Option<OnConnect>>,
    /// Rows that are user bookmarks, with their URI.
    bookmark_rows: RefCell<Vec<(gtk::ListBoxRow, String)>>,
    bookmarks_monitor: RefCell<Option<gio::FileMonitor>>,
    tags: RefCell<Vec<String>>,
    current: RefCell<Option<gio::File>>,
    /// Which section each row belongs to, for folding.
    sections: RefCell<Vec<(String, gtk::ListBoxRow)>>,
    building: RefCell<String>,
}

#[derive(Clone)]
pub struct Sidebar(Rc<Inner>);

impl Sidebar {
    pub fn new() -> Self {
        let list = gtk::ListBox::builder()
            .css_classes(["navigation-sidebar", "places"])
            .selection_mode(gtk::SelectionMode::Single)
            .build();
        list.update_property(&[gtk::accessible::Property::Label("Places")]);
        let scroller = gtk::ScrolledWindow::builder()
            .child(&list)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();

        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["tuck-sidebar"])
            .build();
        // Places, or the Folders tree (F7).
        let folders = crate::folders_ui::FoldersPanel::new();
        let stack = gtk::Stack::builder().vexpand(true).transition_type(gtk::StackTransitionType::Crossfade).transition_duration(150).build();
        stack.add_named(&scroller, Some("places"));
        stack.add_named(&folders.widget(), Some("folders"));
        root.append(&stack);
        // Bottom bar: [Places | Folders]  ⚙  🧹
        let places_button = gtk::ToggleButton::builder().label("Places").active(true).build();
        let folders_button = gtk::ToggleButton::builder().label("Folders").group(&places_button).tooltip_text("Folders (F7)").build();
        let switcher = gtk::Box::builder().css_classes(["linked", "sidebar-switcher"]).hexpand(true).halign(gtk::Align::Start).valign(gtk::Align::Center).build();
        switcher.append(&places_button);
        switcher.append(&folders_button);
        let tools = gtk::Box::builder().spacing(4).css_classes(["sidebar-tools"]).build();
        tools.append(&switcher);
        for (icon, tip, action) in [("tuck-settings-symbolic", "Preferences (Ctrl+,)", "app.preferences"), ("tuck-cleaner-symbolic", "Cleaner", "win.cleaner")] {
            let b = gtk::Button::builder().icon_name(icon).tooltip_text(tip).action_name(action).valign(gtk::Align::Center).css_classes(["flat", "sidebar-tool"]).build();
            b.update_property(&[gtk::accessible::Property::Label(tip.split(" (").next().unwrap_or(tip))]);
            tools.append(&b);
        }
        root.append(&tools);

        let sb = Sidebar(Rc::new(Inner {
            root,
            list,
            rows: RefCell::new(Vec::new()),
            monitor: gio::VolumeMonitor::get(),
            stack,
            folders,
            folders_button: folders_button.clone(),
            places_button: places_button.clone(),
            on_open: RefCell::new(None),
            on_message: RefCell::new(None),
            on_tag: RefCell::new(None),
            on_drop: RefCell::new(None),
            on_connect: RefCell::new(None),
            bookmark_rows: RefCell::new(Vec::new()),
            bookmarks_monitor: RefCell::new(None),
            tags: RefCell::new(crate::search_ui::all_tags()),
            sections: RefCell::default(),
            building: RefCell::default(),
            current: RefCell::new(None),
        }));
        let weak = Rc::downgrade(&sb.0);
        folders_button.connect_toggled(move |b| {
            let Some(i) = weak.upgrade() else { return };
            i.stack.set_visible_child_name(if b.is_active() { "folders" } else { "places" });
            if b.is_active() {
                let cur = i.current.borrow().clone();
                if let Some(f) = cur {
                    i.folders.reveal(&f);
                }
            }
        });
        let weak = Rc::downgrade(&sb.0);
        sb.0.folders.connect_open(move |f| {
            if let Some(i) = weak.upgrade() {
                Sidebar(i).open(f);
            }
        });
        sb.wire();
        sb.rebuild();
        sb
    }

    pub fn widget(&self) -> gtk::Widget {
        self.0.root.clone().upcast()
    }

    pub fn connect_open(&self, f: impl Fn(gio::File) + 'static) {
        *self.0.on_open.borrow_mut() = Some(Rc::new(f));
    }

    pub fn connect_drop(&self, f: impl Fn(Vec<gio::File>, gio::File, gtk::gdk::DragAction) + 'static) {
        *self.0.on_drop.borrow_mut() = Some(Rc::new(f));
    }

    pub fn connect_server(&self, f: impl Fn() + 'static) {
        *self.0.on_connect.borrow_mut() = Some(Rc::new(f));
    }

    /// Add a folder to the user's places (GTK bookmarks).
    pub fn add_bookmark(&self, file: &gio::File) {
        let mut list = tuck_core::bookmarks::load();
        if tuck_core::bookmarks::add(&mut list, &file.uri()) {
            match tuck_core::bookmarks::save(&list) {
                Ok(()) => self.message(format!("Added “{}” to Places", crate::pane::folder_title(file))),
                Err(e) => self.message(format!("Couldn't save places: {e}")),
            }
            self.rebuild();
        } else {
            self.message("That folder is already in Places".into());
        }
    }

    fn edit_bookmarks(&self, f: impl FnOnce(&mut Vec<tuck_core::bookmarks::Bookmark>)) {
        let mut list = tuck_core::bookmarks::load();
        f(&mut list);
        if let Err(e) = tuck_core::bookmarks::save(&list) {
            self.message(format!("Couldn't save places: {e}"));
        }
        self.rebuild();
    }

    /// Right-click on a bookmark: Rename / Remove.
    fn bookmark_menu(&self, row: &gtk::ListBoxRow, uri: String) {
        let pop = gtk::Popover::builder().has_arrow(true).build();
        let b = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).build();
        let rename = gtk::Button::builder().label("Rename…").css_classes(["flat"]).build();
        let remove = gtk::Button::builder().label("Remove from Places").css_classes(["flat"]).build();
        for w in [&rename, &remove] {
            w.child().and_downcast::<gtk::Label>().inspect(|l| l.set_xalign(0.0));
            b.append(w);
        }
        pop.set_child(Some(&b));
        pop.set_parent(row);
        let weak = Rc::downgrade(&self.0);
        let (p2, u2) = (pop.clone(), uri.clone());
        remove.connect_clicked(move |_| {
            p2.popdown();
            if let Some(i) = weak.upgrade() {
                Sidebar(i).edit_bookmarks(|l| tuck_core::bookmarks::remove(l, &u2));
            }
        });
        let weak = Rc::downgrade(&self.0);
        let p2 = pop.clone();
        let row2 = row.clone();
        rename.connect_clicked(move |_| {
            p2.popdown();
            let Some(i) = weak.upgrade() else { return };
            let sb = Sidebar(i);
            let current = tuck_core::bookmarks::load().into_iter().find(|b| b.uri == uri).and_then(|b| b.label)
                .unwrap_or_else(|| crate::pane::folder_title(&gio::File::for_uri(&uri)));
            let entry = gtk::Entry::builder().text(&current).activates_default(true).build();
            let dialog = adw::AlertDialog::builder().heading("Rename Place").extra_child(&entry).default_response("rename").close_response("cancel").build();
            dialog.add_responses(&[("cancel", "Cancel"), ("rename", "Rename")]);
            dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
            let uri = uri.clone();
            dialog.connect_response(None, move |_, r| {
                if r == "rename" {
                    let t = entry.text().to_string();
                    sb.edit_bookmarks(|l| tuck_core::bookmarks::rename(l, &uri, &t));
                }
            });
            dialog.present(Some(&row2));
        });
        pop.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        pop.popup();
    }

    pub fn connect_tag(&self, f: impl Fn(String) + 'static) {
        *self.0.on_tag.borrow_mut() = Some(Rc::new(f));
    }

    pub fn connect_message(&self, f: impl Fn(String) + 'static) {
        *self.0.on_message.borrow_mut() = Some(Rc::new(f));
    }

    fn message(&self, text: String) {
        let cb = self.0.on_message.borrow().clone();
        if let Some(cb) = cb {
            cb(text);
        }
    }

    fn open(&self, file: gio::File) {
        let cb = self.0.on_open.borrow().clone();
        if let Some(cb) = cb {
            cb(file);
        }
    }

    fn wire(&self) {
        let weak = Rc::downgrade(&self.0);
        self.0.list.connect_row_activated(move |_, row| {
            let Some(inner) = weak.upgrade() else { return };
            let sb = Sidebar(inner);
            let target = sb.0.rows.borrow().iter().find(|(r, _)| r == row).and_then(|(_, t)| t.clone());
            match target {
                Some(Target::File(f)) => sb.open(f),
                Some(Target::Volume(v)) => sb.mount(v),
                Some(Target::Section(id)) => sb.toggle_section(&id),
                Some(Target::Connect) => {
                    let cb = sb.0.on_connect.borrow().clone();
                    if let Some(cb) = cb {
                        cb();
                    }
                }
                Some(Target::Tag(t)) => {
                    let cb = sb.0.on_tag.borrow().clone();
                    if let Some(cb) = cb {
                        cb(t);
                    }
                }
                None => {}
            }
        });
        let weak = Rc::downgrade(&self.0);
        let refresh = move || {
            if let Some(inner) = weak.upgrade() {
                Sidebar(inner).rebuild();
            }
        };
        let r = refresh.clone();
        self.0.monitor.connect_mount_added(move |_, _| r());
        let r = refresh.clone();
        self.0.monitor.connect_mount_removed(move |_, _| r());
        let r = refresh.clone();
        self.0.monitor.connect_volume_added(move |_, _| r());
        let r = refresh;
        self.0.monitor.connect_volume_removed(move |_, _| r());
        // Right-click a bookmark to rename or remove it.
        let click = gtk::GestureClick::builder().button(3).build();
        let weak = Rc::downgrade(&self.0);
        click.connect_pressed(move |_, _, _, y| {
            let Some(i) = weak.upgrade() else { return };
            let Some(row) = i.list.row_at_y(y as i32) else { return };
            let uri = i.bookmark_rows.borrow().iter().find(|(r, _)| *r == row).map(|(_, u)| u.clone());
            if let Some(u) = uri {
                Sidebar(i).bookmark_menu(&row, u);
            }
        });
        self.0.list.add_controller(click);
        // Drop files on a place to copy/move them there; drop folders
        // elsewhere in the list to add them to Places.
        let drop = gtk::DropTarget::new(gtk::gdk::FileList::static_type(), gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE);
        let weak = Rc::downgrade(&self.0);
        drop.connect_drop(move |t, value, _, y| {
            let Some(i) = weak.upgrade() else { return false };
            let Ok(list) = value.get::<gtk::gdk::FileList>() else { return false };
            let files = list.files();
            let target = i.list.row_at_y(y as i32).and_then(|row| i.rows.borrow().iter().find(|(r, _)| *r == row).and_then(|(_, t)| t.clone()));
            let sb = Sidebar(i);
            match target {
                Some(Target::File(dest)) if dest.uri_scheme().as_deref() != Some("recent") => {
                    let action = t.current_drop().map(|d| d.actions()).unwrap_or(gtk::gdk::DragAction::COPY);
                    let action = if action.contains(gtk::gdk::DragAction::MOVE) && !action.contains(gtk::gdk::DragAction::COPY) { gtk::gdk::DragAction::MOVE } else { gtk::gdk::DragAction::COPY };
                    let cb = sb.0.on_drop.borrow().clone();
                    if let Some(cb) = cb {
                        cb(files, dest, action);
                    }
                    true
                }
                _ => {
                    let dirs: Vec<_> = files.into_iter().filter(|f| f.query_file_type(gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE) == gio::FileType::Directory).collect();
                    for d in &dirs {
                        sb.add_bookmark(d);
                    }
                    !dirs.is_empty()
                }
            }
        });
        self.0.list.add_controller(drop);
        // Other apps edit the same bookmarks file.
        if let Ok(m) = gio::File::for_path(tuck_core::bookmarks::file()).monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE) {
            let weak = Rc::downgrade(&self.0);
            m.connect_changed(move |_, _, _, ev| {
                if matches!(ev, gio::FileMonitorEvent::ChangesDoneHint | gio::FileMonitorEvent::Deleted | gio::FileMonitorEvent::Created | gio::FileMonitorEvent::Renamed | gio::FileMonitorEvent::MovedIn)
                    && let Some(i) = weak.upgrade() {
                        Sidebar(i).rebuild();
                    }
            });
            *self.0.bookmarks_monitor.borrow_mut() = Some(m);
        }
        // Tags come from the search index, which refreshes in the background.
        let weak = Rc::downgrade(&self.0);
        glib::timeout_add_seconds_local(4, move || {
            let Some(inner) = weak.upgrade() else { return glib::ControlFlow::Break };
            let now = crate::search_ui::all_tags();
            if *inner.tags.borrow() != now {
                *inner.tags.borrow_mut() = now;
                Sidebar(inner).rebuild();
            }
            glib::ControlFlow::Continue
        });
    }

    pub fn rebuild_public(&self) {
        self.rebuild();
    }

    fn rebuild(&self) {
        let list = &self.0.list;
        while let Some(c) = list.first_child() {
            list.remove(&c);
        }
        self.0.sections.borrow_mut().clear();
        let mut rows = Vec::new();

        let dirs = UserDirs::load();
        self.heading(&mut rows, "favourites", "Favourites");
        for d in UserDir::SIDEBAR {
            if let Some(p) = dirs.get(d) {
                let file = gio::File::for_path(p);
                self.place(&mut rows, d.label(), user_dir_icon(d), Target::File(file), None);
            }
        }
        self.place(&mut rows, "Recent", "tuck-recent-symbolic", Target::File(gio::File::for_uri("recent:///")), None);
        self.place(&mut rows, "Trash", "tuck-trash-symbolic", Target::File(gio::File::for_uri("trash:///")), None);

        // The user's own places (GTK bookmarks, shared with other apps).
        // Skip bookmarks already shown under Favourites (GNOME adds
        // Documents, Music… to the shared bookmarks file on new accounts).
        let in_places: Vec<gio::File> = UserDir::SIDEBAR.iter().filter_map(|d| dirs.get(*d)).map(gio::File::for_path).collect();
        let marks: Vec<_> = tuck_core::bookmarks::load()
            .into_iter()
            .filter(|b| {
                let f = gio::File::for_uri(&b.uri);
                !in_places.iter().any(|p| p.equal(&f))
            })
            .collect();
        let mut bookmark_rows = Vec::new();
        if !marks.is_empty() {
            self.heading(&mut rows, "bookmarks", "Bookmarks");
            for b in marks {
                let file = gio::File::for_uri(&b.uri);
                let label = b.label.clone().unwrap_or_else(|| crate::pane::folder_title(&file));
                let icon = if file.path().is_some() { "tuck-folder-symbolic" } else { "tuck-drive-symbolic" };
                self.place(&mut rows, &label, icon, Target::File(file), None);
                if let Some((row, _)) = rows.last() {
                    bookmark_rows.push((row.clone(), b.uri.clone()));
                }
            }
        }
        *self.0.bookmark_rows.borrow_mut() = bookmark_rows;

        // Drives: the system disk, the user's other partitions (a separate
        // /home, a data disk: GIO hides those), then GIO's local mounts and
        // drives that aren't mounted yet. Network shares go under Network.
        self.heading(&mut rows, "drives", "Drives");
        let root = gio::File::for_path("/");
        self.place_drive(&mut rows, "Computer", "tuck-drive-symbolic", Target::File(root.clone()), None, Some(root), None);
        let home = tuck_core::paths::home();
        let mut shown: Vec<std::path::PathBuf> = vec!["/".into()];
        for p in tuck_core::drives::list() {
            let name = if home.starts_with(&p.mount) {
                "Home Disk".to_string()
            } else {
                p.mount.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.mount.display().to_string())
            };
            let f = gio::File::for_path(&p.mount);
            self.place_drive(&mut rows, &name, "tuck-drive-symbolic", Target::File(f.clone()), None, Some(f), None);
            shown.push(p.mount);
        }
        let mut network: Vec<(String, String, gio::File, gio::Mount)> = Vec::new();
        for m in self.0.monitor.mounts() {
            if m.is_shadowed() {
                continue;
            }
            let root = m.root();
            let name = m.name().to_string();
            let local = root.path().filter(|p| !p.to_string_lossy().contains("/gvfs/"));
            match local {
                Some(p) if shown.contains(&p) => continue,
                Some(p) => {
                    let icon = icon_name(&m.symbolic_icon()).unwrap_or_else(|| "tuck-removable-symbolic".into());
                    self.place_drive(&mut rows, &name, &icon, Target::File(root.clone()), Some(m), Some(root), None);
                    shown.push(p);
                }
                None => {
                    let icon = icon_name(&m.symbolic_icon()).unwrap_or_else(|| "tuck-server-symbolic".into());
                    network.push((name, icon, root, m));
                }
            }
        }
        for v in self.0.monitor.volumes() {
            if v.get_mount().is_some() || !v.can_mount() {
                continue;
            }
            let icon = icon_name(&v.symbolic_icon()).unwrap_or_else(|| "tuck-drive-symbolic".into());
            self.place_drive(&mut rows, &v.name(), &icon, Target::Volume(v.clone()), None, None, Some("Not mounted"));
        }

        self.heading(&mut rows, "network", "Network");
        self.place(&mut rows, "Network", "tuck-server-symbolic", Target::File(gio::File::for_uri("network:///")), None);
        for (name, icon, root, m) in network {
            self.place(&mut rows, &name, &icon, Target::File(root), Some(m));
        }
        self.place(&mut rows, "Connect to Server…", "tuck-link-symbolic", Target::Connect, None);

        let tags = self.0.tags.borrow().clone();
        if !tags.is_empty() {
            self.heading(&mut rows, "tags", "Tags");
            for t in tags {
                self.place(&mut rows, &t, "tuck-tag-symbolic", Target::Tag(t.clone()), None);
            }
        }

        *self.0.rows.borrow_mut() = rows;
        self.apply_folding();
        let cur = self.0.current.borrow().clone();
        if let Some(f) = cur {
            self.set_location(&f);
        }
    }

    /// Hide the rows of folded sections, and point each heading's chevron.
    fn apply_folding(&self) {
        let folded = crate::theme_ui::settings().sidebar_collapsed;
        for (sec, row) in self.0.sections.borrow().iter() {
            row.set_visible(!folded.contains(sec));
        }
        for (row, t) in self.0.rows.borrow().iter() {
            if let Some(Target::Section(id)) = t {
                let open = !folded.contains(id);
                if open { row.remove_css_class("folded") } else { row.add_css_class("folded") }
                row.update_state(&[gtk::accessible::State::Expanded(Some(open))]);
            }
        }
    }

    fn toggle_section(&self, id: &str) {
        let id = id.to_string();
        crate::theme_ui::update_quiet(move |s| {
            if let Some(i) = s.sidebar_collapsed.iter().position(|x| *x == id) {
                s.sidebar_collapsed.remove(i);
            } else {
                s.sidebar_collapsed.push(id);
            }
        });
        self.apply_folding();
    }

    /// A section heading; click it (or Enter) to fold the section.
    fn heading(&self, rows: &mut Vec<(gtk::ListBoxRow, Option<Target>)>, id: &str, text: &str) {
        *self.0.building.borrow_mut() = id.to_string();
        let b = gtk::Box::builder().spacing(4).build();
        b.append(&gtk::Label::builder().label(text).xalign(0.0).hexpand(true).css_classes(["sidebar-heading"]).build());
        b.append(&gtk::Image::builder().icon_name("tuck-chevron-symbolic").pixel_size(12).css_classes(["section-chevron"]).build());
        let row = gtk::ListBoxRow::builder()
            .child(&b)
            .selectable(false)
            .activatable(true)
            .css_classes(["section-heading-row"])
            .tooltip_text(format!("Show or hide {text}"))
            .build();
        row.set_accessible_role(gtk::AccessibleRole::Button);
        row.update_property(&[gtk::accessible::Property::Label(text)]);
        self.0.list.append(&row);
        rows.push((row, Some(Target::Section(id.to_string()))));
    }

    fn place(
        &self,
        rows: &mut Vec<(gtk::ListBoxRow, Option<Target>)>,
        label: &str,
        icon: &str,
        target: Target,
        mount: Option<gio::Mount>,
    ) {
        self.place_drive(rows, label, icon, target, mount, None, None);
    }

    /// A row; with `usage`, a drive row that also shows how full it is.
    #[allow(clippy::too_many_arguments)]
    fn place_drive(
        &self,
        rows: &mut Vec<(gtk::ListBoxRow, Option<Target>)>,
        label: &str,
        icon: &str,
        target: Target,
        mount: Option<gio::Mount>,
        usage: Option<gio::File>,
        note: Option<&str>,
    ) {
        let b = gtk::Box::builder().spacing(10).build();
        b.append(&gtk::Image::from_icon_name(icon));
        let name = gtk::Label::builder().label(label).xalign(0.0).hexpand(true).ellipsize(gtk::pango::EllipsizeMode::End).build();
        match usage {
            Some(file) => {
                let col = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(3).hexpand(true).valign(gtk::Align::Center).css_classes(["drive-row"]).build();
                col.append(&name);
                let bar = gtk::LevelBar::builder().min_value(0.0).max_value(1.0).css_classes(["storage-bar", "drive-bar"]).build();
                for o in [gtk::LEVEL_BAR_OFFSET_LOW, gtk::LEVEL_BAR_OFFSET_HIGH, gtk::LEVEL_BAR_OFFSET_FULL] {
                    bar.remove_offset_value(Some(o));
                }
                let detail = gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).css_classes(["storage-detail"]).build();
                col.append(&bar);
                col.append(&detail);
                b.append(&col);
                fill_usage(file, bar, detail);
            }
            None => match note {
                // A small second line, e.g. "Not mounted".
                Some(n) => {
                    let col = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).hexpand(true).valign(gtk::Align::Center).css_classes(["drive-row"]).build();
                    col.append(&name);
                    col.append(&gtk::Label::builder().label(n).xalign(0.0).css_classes(["storage-detail"]).build());
                    b.append(&col);
                }
                None => b.append(&name),
            },
        }
        if let Some(m) = mount.filter(|m| m.can_unmount() || m.can_eject()) {
            let eject = gtk::Button::builder()
                .icon_name("tuck-eject-symbolic")
                .tooltip_text(format!("Unmount {label}"))
                .css_classes(["flat", "circular", "eject"])
                .valign(gtk::Align::Center)
                .build();
            eject.update_property(&[gtk::accessible::Property::Label(&format!("Unmount {label}"))]);
            let weak = Rc::downgrade(&self.0);
            eject.connect_clicked(move |_| {
                if let Some(inner) = weak.upgrade() {
                    Sidebar(inner).unmount(m.clone());
                }
            });
            b.append(&eject);
        }
        let row = gtk::ListBoxRow::builder().child(&b).tooltip_text(label).build();
        row.update_property(&[gtk::accessible::Property::Label(label)]);
        self.0.list.append(&row);
        let section = self.0.building.borrow().clone();
        self.0.sections.borrow_mut().push((section, row.clone()));
        rows.push((row, Some(target)));
    }

    /// Switch between Places and the Folders tree.
    pub fn toggle_folders(&self) {
        let b = &self.0.folders_button;
        if b.is_active() {
            self.0.places_button.set_active(true);
        } else {
            b.set_active(true);
        }
    }

    /// Highlight the row for `file`, if it has one.
    pub fn set_location(&self, file: &gio::File) {
        *self.0.current.borrow_mut() = Some(file.clone());
        if self.0.folders_button.is_active() {
            self.0.folders.reveal(file);
        }
        let rows = self.0.rows.borrow();
        let hit = rows.iter().find(|(_, t)| matches!(t, Some(Target::File(f)) if f.equal(file)));
        match hit {
            Some((row, _)) => self.0.list.select_row(Some(row)),
            None => self.0.list.unselect_all(),
        }
    }

    fn mount(&self, volume: gio::Volume) {
        let op = gtk::MountOperation::new(self.0.root.root().and_downcast_ref::<gtk::Window>());
        let weak = Rc::downgrade(&self.0);
        let name = volume.name().to_string();
        let v = volume.clone();
        v.mount(gio::MountMountFlags::NONE, Some(&op), gio::Cancellable::NONE, move |res| {
            let Some(inner) = weak.upgrade() else { return };
            let sb = Sidebar(inner);
            match res {
                Ok(()) => {
                    if let Some(root) = volume.get_mount().map(|m| m.root()) {
                        sb.open(root);
                    }
                }
                Err(e) if e.matches(gio::IOErrorEnum::FailedHandled) => {}
                Err(e) => sb.message(format!("Couldn't mount “{name}”: {}", e.message())),
            }
        });
    }

    fn unmount(&self, mount: gio::Mount) {
        let op = gtk::MountOperation::new(self.0.root.root().and_downcast_ref::<gtk::Window>());
        let weak = Rc::downgrade(&self.0);
        let name = mount.name().to_string();
        let done = move |res: Result<(), glib::Error>| {
            let Some(inner) = weak.upgrade() else { return };
            let sb = Sidebar(inner);
            match res {
                Ok(()) => sb.message(format!("“{name}” can be safely removed.")),
                Err(e) if e.matches(gio::IOErrorEnum::FailedHandled) => {}
                Err(e) if e.matches(gio::IOErrorEnum::Busy) => sb.message(format!(
                    "“{name}” is in use. Close any files or apps using it, then try again."
                )),
                Err(e) => sb.message(format!("Couldn't unmount “{name}”: {}", e.message())),
            }
        };
        if mount.can_eject() {
            mount.eject_with_operation(gio::MountUnmountFlags::NONE, Some(&op), gio::Cancellable::NONE, done);
        } else {
            mount.unmount_with_operation(gio::MountUnmountFlags::NONE, Some(&op), gio::Cancellable::NONE, done);
        }
    }
}

/// Fill a drive row's bar and "X free of Y" once the numbers are in.
fn fill_usage(file: gio::File, bar: gtk::LevelBar, detail: gtk::Label) {
    glib::MainContext::default().spawn_local(async move {
        let Ok(info) = file.query_filesystem_info_future("filesystem::size,filesystem::free", glib::Priority::LOW).await else {
            bar.set_visible(false);
            detail.set_visible(false);
            return;
        };
        let size = info.attribute_uint64("filesystem::size");
        let free = info.attribute_uint64("filesystem::free");
        if size == 0 {
            bar.set_visible(false);
            detail.set_visible(false);
            return;
        }
        let used = size.saturating_sub(free);
        let frac = used as f64 / size as f64;
        bar.set_value(frac);
        if frac > 0.9 {
            bar.add_css_class("nearly-full");
        }
        detail.set_label(&format!("{} free of {}", format::size(free), format::size(size)));
        bar.update_property(&[gtk::accessible::Property::Label(&format!("{:.0}% used", frac * 100.0))]);
    });
}

fn user_dir_icon(d: UserDir) -> &'static str {
    match d {
        UserDir::Home => "tuck-home-symbolic",
        UserDir::Desktop => "tuck-desktop-symbolic",
        UserDir::Documents => "tuck-documents-symbolic",
        UserDir::Downloads => "tuck-downloads-symbolic",
        UserDir::Music => "tuck-music-symbolic",
        UserDir::Pictures => "tuck-pictures-symbolic",
        UserDir::Videos => "tuck-videos-symbolic",
        UserDir::Templates => "folder-templates-symbolic",
        UserDir::Public => "folder-publicshare-symbolic",
    }
}

fn icon_name(icon: &gio::Icon) -> Option<String> {
    icon.downcast_ref::<gio::ThemedIcon>()
        .and_then(|t| t.names().first().map(|s| s.to_string()))
}
