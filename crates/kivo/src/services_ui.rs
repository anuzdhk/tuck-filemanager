// SPDX-License-Identifier: GPL-3.0-or-later
//! Dolphin service menus in the context menu. The menus that apply are
//! worked out when the menu opens; picking one runs its command.

use std::cell::RefCell;

use gtk::gio;
use gtk::prelude::*;
use kivo_core::servicemenu::{self, Picked, ServiceAction, ServiceMenu};

use crate::item::FileItem;

thread_local! {
    /// Actions offered by the menu that is open now, by index.
    static OFFERED: RefCell<Vec<ServiceAction>> = const { RefCell::new(Vec::new()) };
}

/// One menu entry: (submenu, label, index for `win.service-action`).
pub type Entry = (Option<String>, String, u32);

fn scheme_of(item: &FileItem) -> String {
    item.file().uri_scheme().map(|s| s.to_string()).unwrap_or_else(|| "file".into())
}

/// The service-menu actions that apply to `items`.
pub fn applicable(items: &[FileItem]) -> Vec<Entry> {
    let menus: Vec<ServiceMenu> = servicemenu::load(&kivo_core::terminal::on_path_or_abs);
    let mimes: Vec<(String, bool, String)> = items
        .iter()
        .map(|i| {
            let m = if i.is_dir() { "inode/directory".to_string() } else { i.content_type() };
            let mime = gio::content_type_get_mime_type(&m).map(|s| s.to_string()).unwrap_or(m);
            (mime, i.is_dir(), scheme_of(i))
        })
        .collect();
    let picked: Vec<Picked> = mimes.iter().map(|(m, d, s)| Picked { mime: m, is_dir: *d, scheme: s }).collect();
    let is_a = |a: &str, b: &str| gio::content_type_is_mime_type(a, b);
    let mut offered = Vec::new();
    let mut entries = Vec::new();
    for m in menus.iter().filter(|m| m.applies(&picked, &is_a)) {
        for a in &m.actions {
            entries.push((m.submenu.clone(), a.name.clone(), offered.len() as u32));
            offered.push(a.clone());
        }
    }
    OFFERED.with(|o| *o.borrow_mut() = offered);
    entries
}

/// Run offered action `index` on `items`, in folder `cwd`.
pub fn run(index: u32, items: &[FileItem], cwd: Option<&std::path::Path>) -> Result<(), String> {
    let Some(action) = OFFERED.with(|o| o.borrow().get(index as usize).cloned()) else {
        return Err("That action is no longer available.".into());
    };
    let files: Vec<std::path::PathBuf> = items.iter().filter_map(|i| i.path()).collect();
    let uris: Vec<String> = items.iter().map(|i| i.file().uri().to_string()).collect();
    for argv in servicemenu::commands(&action.exec, &files, &uris, &action.name, action.icon.as_deref()) {
        let mut cmd = std::process::Command::new(&argv[0]);
        cmd.args(&argv[1..]);
        if let Some(d) = cwd {
            cmd.current_dir(d);
        }
        let mut child = cmd.spawn().map_err(|e| format!("Couldn't run “{}”: {e}", action.name))?;
        // Reap it when it ends, so it doesn't linger as a zombie.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
    Ok(())
}
