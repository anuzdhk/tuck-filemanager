// SPDX-License-Identifier: GPL-3.0-or-later
//! Context menus. Common actions sit at the top level, never buried.

use gtk::{gio, glib};
use tuck_core::paths::{UserDir, UserDirs};

pub struct Selection {
    pub count: usize,
    pub has_archive: bool,
    pub has_installable: bool,
    pub has_dir: bool,
    pub has_file: bool,
    pub in_trash: bool,
    /// Dolphin service-menu actions that apply.
    pub services: Vec<crate::services_ui::Entry>,
}

/// Menu for right-clicking one or more items.
pub fn item_menu(sel: &Selection) -> gio::Menu {
    let menu = gio::Menu::new();
    if sel.in_trash {
        let s = gio::Menu::new();
        s.append(Some("_Restore"), Some("win.restore"));
        s.append(Some("_Delete Permanently…"), Some("win.delete"));
        menu.append_section(None, &s);
        let s = gio::Menu::new();
        s.append(Some("P_roperties"), Some("win.properties"));
        menu.append_section(None, &s);
        return menu;
    }

    let open = gio::Menu::new();
    if sel.has_installable && sel.count == 1 {
        open.append(Some("_Install…"), Some("win.install"));
    }
    open.append(Some("_Open"), Some("win.open"));
    if sel.has_dir {
        open.append(Some("Open in New _Tab"), Some("win.open-new-tab"));
    }
    if sel.has_file {
        open.append(Some("Open _With…"), Some("win.open-with"));
    }
    menu.append_section(None, &open);

    let edit = gio::Menu::new();
    edit.append(Some("Cu_t"), Some("win.cut"));
    edit.append(Some("_Copy"), Some("win.copy"));
    if sel.count == 1 && sel.has_dir {
        edit.append(Some("_Paste Into Folder"), Some("win.paste-into"));
    }
    menu.append_section(None, &edit);

    let manage = gio::Menu::new();
    manage.append(Some("Re_name…"), Some("win.rename"));
    if sel.has_archive {
        manage.append(Some("E_xtract Here"), Some("win.extract"));
    }
    manage.append(Some("Co_mpress…"), Some("win.compress"));
    manage.append(Some("_Duplicate"), Some("win.duplicate"));
    manage.append(Some("Create _Link"), Some("win.link"));
    manage.append(Some("_Move To…"), Some("win.move-to"));
    manage.append(Some("Cop_y To…"), Some("win.copy-to"));
    manage.append(Some("Move to T_rash"), Some("win.trash"));
    manage.append(Some("_Delete Permanently…"), Some("win.delete"));
    menu.append_section(None, &manage);

    let more = gio::Menu::new();
    more.append(Some("Copy _Path"), Some("win.copy-path"));
    if sel.count == 1 && sel.has_dir {
        more.append(Some("Open in T_erminal"), Some("win.open-terminal-here"));
    }
    menu.append_section(None, &more);

    if !sel.services.is_empty() {
        // Service menus: grouped into their submenus, like Dolphin.
        let section = gio::Menu::new();
        let mut subs: Vec<(String, gio::Menu)> = Vec::new();
        for (sub, label, idx) in &sel.services {
            let item = gio::MenuItem::new(Some(label), None);
            item.set_action_and_target_value(Some("win.service-action"), Some(&glib::Variant::from(*idx)));
            match sub {
                Some(name) => match subs.iter().find(|(n, _)| n == name) {
                    Some((_, m)) => m.append_item(&item),
                    None => {
                        let m = gio::Menu::new();
                        m.append_item(&item);
                        subs.push((name.clone(), m));
                    }
                },
                None => section.append_item(&item),
            }
        }
        for (name, m) in subs {
            section.append_submenu(Some(&name), &m);
        }
        menu.append_section(None, &section);
    }

    let props = gio::Menu::new();
    props.append(Some("P_roperties"), Some("win.properties"));
    menu.append_section(None, &props);
    menu
}

/// Menu for right-clicking empty space in a folder.
pub fn background_menu(in_trash: bool, writable: bool) -> gio::Menu {
    let menu = gio::Menu::new();
    if in_trash {
        let s = gio::Menu::new();
        s.append(Some("_Empty Trash…"), Some("win.empty-trash"));
        s.append(Some("Select _All"), Some("win.select-all"));
        menu.append_section(None, &s);
        return menu;
    }
    if writable {
        let new = gio::Menu::new();
        new.append(Some("New _Folder…"), Some("win.new-folder"));
        new.append_submenu(Some("New _Document"), &templates_menu());
        menu.append_section(None, &new);
        let edit = gio::Menu::new();
        edit.append(Some("_Paste"), Some("win.paste"));
        edit.append(Some("Select _All"), Some("win.select-all"));
        menu.append_section(None, &edit);
    } else {
        let edit = gio::Menu::new();
        edit.append(Some("Select _All"), Some("win.select-all"));
        menu.append_section(None, &edit);
    }
    let view = gio::Menu::new();
    view.append(Some("Open in T_erminal"), Some("win.open-terminal"));
    view.append(Some("Show _Hidden Files"), Some("win.show-hidden"));
    menu.append_section(None, &view);
    let props = gio::Menu::new();
    props.append(Some("P_roperties"), Some("win.folder-properties"));
    menu.append_section(None, &props);
    menu
}

/// "New Document": an empty file, then everything in ~/Templates.
pub fn templates_menu() -> gio::Menu {
    let m = gio::Menu::new();
    m.append(Some("_Empty File…"), Some("win.new-file::"));
    if let Some(dir) = UserDirs::load().get(UserDir::Templates) {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| !n.starts_with('.'))
                    .collect()
            })
            .unwrap_or_default();
        names.sort_by(|a, b| tuck_core::sort::natural_cmp(a, b));
        if !names.is_empty() {
            let s = gio::Menu::new();
            for n in names {
                let label = tuck_core::names::split_ext(&n).0.to_string();
                let item = gio::MenuItem::new(Some(&label.replace('_', "__")), None);
                item.set_action_and_target_value(Some("win.new-file"), Some(&glib_variant(&n)));
                s.append_item(&item);
            }
            m.append_section(None, &s);
        }
    }
    m
}

fn glib_variant(s: &str) -> gtk::glib::Variant {
    use gtk::prelude::ToVariant;
    s.to_variant()
}
