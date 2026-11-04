// SPDX-License-Identifier: GPL-3.0-or-later
//! `FileItem`: the GObject every list model holds.
//!
//! It is built either from the core's fast local lister (`RawEntry`) or from
//! a `gio::FileInfo` (remote and virtual locations, live updates). Everything
//! sorting needs is cached in `EntryMeta`, so sorting 100k items never touches
//! GIO or allocates. The `gio::File` and icon are created lazily.

use std::cell::{Cell, OnceCell, Ref, RefCell};
use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use strata_core::group::{Group, group_now};
use strata_core::listing::RawEntry;
use strata_core::paths::{UserDir, UserDirs};
use strata_core::sort::SortKey;
use strata_core::{EntryKind, EntryMeta};

/// Attributes requested from GIO (non-local locations and live updates).
/// Only `fast-content-type`, guessed from the name, so files are never opened.
pub const ATTRS: &str = "standard::name,standard::display-name,standard::type,standard::size,\
standard::icon,standard::fast-content-type,standard::is-hidden,standard::is-backup,\
standard::is-symlink,standard::symlink-target,standard::target-uri,time::modified,time::created,\
trash::orig-path,trash::deletion-date";

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct FileItem {
        pub parent: OnceCell<gio::File>,
        pub raw_name: RefCell<OsString>,
        pub file: OnceCell<gio::File>,
        pub meta: RefCell<EntryMeta>,
        pub hidden: Cell<bool>,
        pub content_type: RefCell<String>,
        pub icon: RefCell<Option<gio::Icon>>,
        pub is_symlink: Cell<bool>,
        pub link_target: RefCell<Option<String>>,
        pub created: Cell<Option<i64>>,
        pub target_uri: RefCell<Option<String>>,
        pub group: RefCell<Option<(SortKey, Group)>>,
        /// For items listed in trash:///.
        pub trash_orig: RefCell<Option<std::path::PathBuf>>,
        pub trash_deleted: RefCell<Option<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FileItem {
        const NAME: &'static str = "StrataFileItem";
        type Type = super::FileItem;
    }

    impl ObjectImpl for FileItem {}
}

glib::wrapper! {
    pub struct FileItem(ObjectSubclass<imp::FileItem>);
}

impl FileItem {
    /// From the fast local lister. `parent` is the listed folder.
    pub fn from_raw(parent: &gio::File, e: RawEntry) -> Self {
        let obj: Self = glib::Object::new();
        let imp = obj.imp();
        imp.parent.set(parent.clone()).expect("set once");
        let mut meta = e.meta;
        let ct = guess_content_type(&meta.name, meta.kind == EntryKind::Directory);
        meta.type_name = describe(&ct);
        imp.hidden.set(meta.is_hidden());
        imp.is_symlink.set(e.is_symlink);
        *imp.link_target.borrow_mut() = e.link_target.map(|p| p.to_string_lossy().into_owned());
        imp.created.set(e.created);
        *imp.content_type.borrow_mut() = ct;
        *imp.meta.borrow_mut() = meta;
        *imp.raw_name.borrow_mut() = e.name;
        obj
    }

    /// From GIO, for `file` itself.
    pub fn from_info(file: gio::File, info: gio::FileInfo) -> Self {
        let obj: Self = glib::Object::new();
        if let Some(p) = file.parent() {
            let _ = obj.imp().parent.set(p);
        }
        obj.imp().file.set(file).expect("set once");
        obj.set_info(&info);
        obj
    }

    /// From GIO, for a child of `parent`.
    pub fn from_child(parent: &gio::File, info: gio::FileInfo) -> Self {
        Self::from_info(parent.child(info.name()), info)
    }

    fn set_info(&self, info: &gio::FileInfo) {
        let imp = self.imp();
        let kind = match info.file_type() {
            gio::FileType::Directory | gio::FileType::Mountable => EntryKind::Directory,
            gio::FileType::Regular | gio::FileType::SymbolicLink => EntryKind::File,
            _ => EntryKind::Other,
        };
        let ct = attr(info, "standard::fast-content-type")
            .or_else(|| attr(info, "standard::content-type"))
            .unwrap_or_else(|| guess_content_type(&info.display_name(), kind == EntryKind::Directory));
        let meta = EntryMeta::new(
            info.display_name().to_string(),
            kind,
            info.size().max(0) as u64,
            info.modification_date_time().map(|d| d.to_unix()).unwrap_or(0),
            describe(&ct),
        );
        imp.hidden.set(info.is_hidden() || info.is_backup() || meta.is_hidden());
        imp.is_symlink.set(info.is_symlink());
        *imp.link_target.borrow_mut() = if info.is_symlink() {
            attr(info, "standard::symlink-target")
        } else {
            None
        };
        if info.has_attribute("time::created") {
            imp.created.set(Some(info.attribute_uint64("time::created") as i64));
        }
        *imp.target_uri.borrow_mut() = attr(info, "standard::target-uri").filter(|s| !s.is_empty());
        *imp.icon.borrow_mut() = info.icon();
        *imp.trash_orig.borrow_mut() = attr_bytes_path(info, "trash::orig-path");
        *imp.trash_deleted.borrow_mut() = attr(info, "trash::deletion-date");
        *imp.raw_name.borrow_mut() = info.name().into_os_string();
        *imp.content_type.borrow_mut() = ct;
        *imp.meta.borrow_mut() = meta;
        *imp.group.borrow_mut() = None;
    }

    /// Run `f` with this item's group for `key`, computing it once.
    pub fn with_group<R>(&self, key: SortKey, f: impl FnOnce(&Group) -> R) -> R {
        let imp = self.imp();
        {
            let cached = imp.group.borrow();
            if let Some((k, g)) = cached.as_ref()
                && *k == key {
                    return f(g);
                }
        }
        let g = group_now(&imp.meta.borrow(), key);
        let r = f(&g);
        *imp.group.borrow_mut() = Some((key, g));
        r
    }

    pub fn file(&self) -> gio::File {
        let imp = self.imp();
        imp.file
            .get_or_init(|| {
                let parent = imp.parent.get().expect("item has a parent or a file");
                parent.child(&*imp.raw_name.borrow())
            })
            .clone()
    }

    pub fn meta(&self) -> Ref<'_, EntryMeta> {
        self.imp().meta.borrow()
    }

    pub fn name(&self) -> String {
        self.imp().meta.borrow().name.clone()
    }

    /// The on-disk name, used to match monitor events.
    pub fn raw_name(&self) -> OsString {
        self.imp().raw_name.borrow().clone()
    }

    pub fn raw_name_eq(&self, name: &std::ffi::OsStr) -> bool {
        self.imp().raw_name.borrow().as_bytes() == name.as_bytes()
    }

    pub fn is_dir(&self) -> bool {
        self.imp().meta.borrow().is_dir()
    }

    pub fn is_hidden(&self) -> bool {
        self.imp().hidden.get()
    }

    pub fn is_symlink(&self) -> bool {
        self.imp().is_symlink.get()
    }

    pub fn link_target(&self) -> Option<String> {
        self.imp().link_target.borrow().clone()
    }

    pub fn created(&self) -> Option<i64> {
        self.imp().created.get()
    }

    pub fn content_type(&self) -> String {
        self.imp().content_type.borrow().clone()
    }

    pub fn icon(&self) -> gio::Icon {
        let imp = self.imp();
        if let Some(i) = imp.icon.borrow().as_ref() {
            return i.clone();
        }
        let icon = if self.is_dir() {
            special_folder_icon(&self.file()).unwrap_or_else(|| icon_for(&imp.content_type.borrow()))
        } else {
            icon_for(&imp.content_type.borrow())
        };
        *imp.icon.borrow_mut() = Some(icon.clone());
        icon
    }

    /// Original location and deletion date, for items in the trash.
    pub fn trash_info(&self) -> Option<(std::path::PathBuf, Option<String>)> {
        let imp = self.imp();
        let orig = imp.trash_orig.borrow().clone()?;
        Some((orig, imp.trash_deleted.borrow().clone()))
    }

    /// Local path of the item (following trash and mountable targets).
    pub fn path(&self) -> Option<std::path::PathBuf> {
        self.target().path()
    }

    /// For mountables (trash, network) the item points elsewhere.
    pub fn target(&self) -> gio::File {
        match self.imp().target_uri.borrow().as_deref() {
            Some(uri) => gio::File::for_uri(uri),
            None => self.file(),
        }
    }
}

fn attr_bytes_path(info: &gio::FileInfo, name: &str) -> Option<std::path::PathBuf> {
    if !info.has_attribute(name) {
        return None;
    }
    info.attribute_byte_string(name).map(|s| std::path::PathBuf::from(s.as_str()))
}

fn attr(info: &gio::FileInfo, name: &str) -> Option<String> {
    if info.has_attribute(name) {
        info.attribute_string(name).map(|s| s.to_string())
    } else {
        None
    }
}

thread_local! {
    static TYPE_BY_SUFFIX: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    static DESCRIPTIONS: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    static ICONS: RefCell<HashMap<String, gio::Icon>> = RefCell::new(HashMap::new());
    static USER_DIRS: UserDirs = UserDirs::load();
}

/// Content type from the name, cached by suffix: 100,000 files with a
/// handful of extensions cost a handful of lookups.
fn guess_content_type(name: &str, is_dir: bool) -> String {
    if is_dir {
        return "inode/directory".into();
    }
    let lower = name.to_lowercase();
    let suffix = match lower.rfind('.') {
        Some(i) if i > 0 => {
            // Keep compound suffixes like .tar.gz distinct.
            let before = &lower[..i];
            match before.rfind('.') {
                Some(j) if &before[j..] == ".tar" => Some(lower[j..].to_string()),
                _ => Some(lower[i..].to_string()),
            }
        }
        _ => None,
    };
    let guess = || gio::content_type_guess(Some(name), None::<&[u8]>).0.to_string();
    match suffix {
        Some(s) => TYPE_BY_SUFFIX.with(|m| m.borrow_mut().entry(s).or_insert_with(guess).clone()),
        None => guess(),
    }
}

fn describe(content_type: &str) -> String {
    DESCRIPTIONS.with(|d| {
        d.borrow_mut()
            .entry(content_type.to_string())
            .or_insert_with(|| gio::content_type_get_description(content_type).to_string())
            .clone()
    })
}

fn icon_for(content_type: &str) -> gio::Icon {
    ICONS.with(|m| {
        m.borrow_mut()
            .entry(content_type.to_string())
            .or_insert_with(|| gio::content_type_get_icon(content_type))
            .clone()
    })
}

fn special_folder_icon(file: &gio::File) -> Option<gio::Icon> {
    let path = file.path()?;
    let dir = USER_DIRS.with(|d| d.identify(&path))?;
    let name = match dir {
        UserDir::Home => "user-home",
        UserDir::Desktop => "user-desktop",
        UserDir::Documents => "folder-documents",
        UserDir::Downloads => "folder-download",
        UserDir::Music => "folder-music",
        UserDir::Pictures => "folder-pictures",
        UserDir::Videos => "folder-videos",
        UserDir::Templates => "folder-templates",
        UserDir::Public => "folder-publicshare",
    };
    Some(gio::ThemedIcon::from_names(&[name, "folder"]).upcast())
}
