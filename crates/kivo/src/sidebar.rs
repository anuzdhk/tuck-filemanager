// SPDX-License-Identifier: GPL-3.0-or-later
//! Places sidebar: user folders, locations, devices (mount / unmount), and a
//! storage indicator for the home file system.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};
use kivo_core::format;
use kivo_core::paths::{self, UserDir, UserDirs};

#[derive(Clone)]
enum Target {
    File(gio::File),
    /// An unmounted volume; activating it mounts it first.
    Volume(gio::Volume),
}

type OnOpen = Rc<dyn Fn(gio::File)>;
type OnMessage = Rc<dyn Fn(String)>;

pub struct Inner {
    root: gtk::Box,
    list: gtk::ListBox,
    rows: RefCell<Vec<(gtk::ListBoxRow, Option<Target>)>>,
    monitor: gio::VolumeMonitor,
    storage_label: gtk::Label,
    storage_value: gtk::Label,
    storage_bar: gtk::LevelBar,
    storage_detail: gtk::Label,
    on_open: RefCell<Option<OnOpen>>,
    on_message: RefCell<Option<OnMessage>>,
    current: RefCell<Option<gio::File>>,
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

        let storage_label = gtk::Label::builder().xalign(0.0).hexpand(true).css_classes(["storage-title"]).build();
        let storage_value = gtk::Label::builder().xalign(1.0).css_classes(["storage-title", "numeric"]).build();
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        top.append(&storage_label);
        top.append(&storage_value);
        let storage_bar = gtk::LevelBar::builder().min_value(0.0).max_value(1.0).css_classes(["storage-bar"]).build();
        storage_bar.remove_offset_value(Some(gtk::LEVEL_BAR_OFFSET_LOW));
        storage_bar.remove_offset_value(Some(gtk::LEVEL_BAR_OFFSET_HIGH));
        storage_bar.remove_offset_value(Some(gtk::LEVEL_BAR_OFFSET_FULL));
        let storage_detail = gtk::Label::builder().xalign(0.0).css_classes(["storage-detail"]).build();
        let storage = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .css_classes(["storage"])
            .build();
        storage.append(&top);
        storage.append(&storage_bar);
        storage.append(&storage_detail);

        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["kivo-sidebar"])
            .build();
        root.append(&scroller);
        root.append(&storage);

        let sb = Sidebar(Rc::new(Inner {
            root,
            list,
            rows: RefCell::new(Vec::new()),
            monitor: gio::VolumeMonitor::get(),
            storage_label,
            storage_value,
            storage_bar,
            storage_detail,
            on_open: RefCell::new(None),
            on_message: RefCell::new(None),
            current: RefCell::new(None),
        }));
        sb.wire();
        sb.rebuild();
        sb.refresh_storage();
        sb
    }

    pub fn widget(&self) -> gtk::Widget {
        self.0.root.clone().upcast()
    }

    pub fn connect_open(&self, f: impl Fn(gio::File) + 'static) {
        *self.0.on_open.borrow_mut() = Some(Rc::new(f));
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
    }

    fn rebuild(&self) {
        let list = &self.0.list;
        while let Some(c) = list.first_child() {
            list.remove(&c);
        }
        let mut rows = Vec::new();

        let dirs = UserDirs::load();
        self.heading(&mut rows, "Places");
        for d in UserDir::SIDEBAR {
            if let Some(p) = dirs.get(d) {
                let file = gio::File::for_path(p);
                self.place(&mut rows, d.label(), user_dir_icon(d), Target::File(file), None);
            }
        }
        self.place(&mut rows, "Recent", "document-open-recent-symbolic", Target::File(gio::File::for_uri("recent:///")), None);
        self.place(&mut rows, "Trash", "user-trash-symbolic", Target::File(gio::File::for_uri("trash:///")), None);

        self.heading(&mut rows, "Locations");
        self.place(&mut rows, "Computer", "drive-harddisk-symbolic", Target::File(gio::File::for_path("/")), None);

        // Devices: mounted volumes, then unmounted ones that can be mounted.
        let mut devices: Vec<(String, String, Target, Option<gio::Mount>)> = Vec::new();
        for m in self.0.monitor.mounts() {
            if m.is_shadowed() {
                continue;
            }
            let root = m.root();
            if root.path().is_some_and(|p| p.as_os_str() == "/") {
                continue;
            }
            let icon = icon_name(&m.symbolic_icon()).unwrap_or_else(|| "drive-removable-media-symbolic".into());
            devices.push((m.name().to_string(), icon, Target::File(root), Some(m)));
        }
        for v in self.0.monitor.volumes() {
            if v.get_mount().is_some() || !v.can_mount() {
                continue;
            }
            let icon = icon_name(&v.symbolic_icon()).unwrap_or_else(|| "drive-harddisk-symbolic".into());
            devices.push((v.name().to_string(), icon, Target::Volume(v), None));
        }
        if !devices.is_empty() {
            self.heading(&mut rows, "Devices");
            for (name, icon, target, mount) in devices {
                self.place(&mut rows, &name, &icon, target, mount);
            }
        }

        *self.0.rows.borrow_mut() = rows;
        let cur = self.0.current.borrow().clone();
        if let Some(f) = cur {
            self.set_location(&f);
        }
    }

    fn heading(&self, rows: &mut Vec<(gtk::ListBoxRow, Option<Target>)>, text: &str) {
        let label = gtk::Label::builder().label(text).xalign(0.0).css_classes(["sidebar-heading"]).build();
        let row = gtk::ListBoxRow::builder()
            .child(&label)
            .selectable(false)
            .activatable(false)
            .focusable(false)
            .build();
        row.set_accessible_role(gtk::AccessibleRole::Presentation);
        self.0.list.append(&row);
        rows.push((row, None));
    }

    fn place(
        &self,
        rows: &mut Vec<(gtk::ListBoxRow, Option<Target>)>,
        label: &str,
        icon: &str,
        target: Target,
        mount: Option<gio::Mount>,
    ) {
        let b = gtk::Box::builder().spacing(10).build();
        b.append(&gtk::Image::from_icon_name(icon));
        b.append(&gtk::Label::builder().label(label).xalign(0.0).hexpand(true).ellipsize(gtk::pango::EllipsizeMode::End).build());
        if let Some(m) = mount.filter(|m| m.can_unmount() || m.can_eject()) {
            let eject = gtk::Button::builder()
                .icon_name("media-eject-symbolic")
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
        rows.push((row, Some(target)));
    }

    /// Highlight the row for `file`, if it has one.
    pub fn set_location(&self, file: &gio::File) {
        *self.0.current.borrow_mut() = Some(file.clone());
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

    pub fn refresh_storage(&self) {
        let home = gio::File::for_path(paths::home());
        let weak = Rc::downgrade(&self.0);
        glib::MainContext::default().spawn_local(async move {
            let info = home
                .query_filesystem_info_future("filesystem::size,filesystem::free", glib::Priority::LOW)
                .await;
            let Some(inner) = weak.upgrade() else { return };
            let Ok(info) = info else {
                inner.storage_label.set_label("Storage");
                inner.storage_detail.set_label("Unavailable");
                return;
            };
            let size = info.attribute_uint64("filesystem::size");
            let free = info.attribute_uint64("filesystem::free");
            let used = size.saturating_sub(free);
            let frac = if size > 0 { used as f64 / size as f64 } else { 0.0 };
            inner.storage_label.set_label("Storage");
            inner.storage_value.set_label(&format!("{:.0}%", frac * 100.0));
            inner.storage_bar.set_value(frac);
            if frac > 0.9 {
                inner.storage_bar.add_css_class("nearly-full");
            } else {
                inner.storage_bar.remove_css_class("nearly-full");
            }
            inner.storage_detail.set_label(&format!(
                "{} of {} used · {} free",
                format::size(used),
                format::size(size),
                format::size(free)
            ));
            inner.storage_bar.update_property(&[gtk::accessible::Property::Label(&format!(
                "Storage {:.0}% used",
                frac * 100.0
            ))]);
        });
    }
}

fn user_dir_icon(d: UserDir) -> &'static str {
    match d {
        UserDir::Home => "user-home-symbolic",
        UserDir::Desktop => "user-desktop-symbolic",
        UserDir::Documents => "folder-documents-symbolic",
        UserDir::Downloads => "folder-download-symbolic",
        UserDir::Music => "folder-music-symbolic",
        UserDir::Pictures => "folder-pictures-symbolic",
        UserDir::Videos => "folder-videos-symbolic",
        UserDir::Templates => "folder-templates-symbolic",
        UserDir::Public => "folder-publicshare-symbolic",
    }
}

fn icon_name(icon: &gio::Icon) -> Option<String> {
    icon.downcast_ref::<gio::ThemedIcon>()
        .and_then(|t| t.names().first().map(|s| s.to_string()))
}
