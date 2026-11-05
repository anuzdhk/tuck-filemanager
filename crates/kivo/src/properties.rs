// SPDX-License-Identifier: GPL-3.0-or-later
//! Properties: what an item is, how big it is (folders are measured in the
//! background), where it lives, how much space is left on its drive, and who
//! may read or change it.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gio, glib};
use kivo_core::ops::{self, Cancel};
use kivo_core::perms::{self, Access, Who};
use kivo_core::{format, paths};

use crate::item::FileItem;

pub fn show(parent: &gtk::Widget, items: &[FileItem]) {
    if items.is_empty() {
        return;
    }
    let dialog = adw::PreferencesDialog::builder()
        .title(if items.len() == 1 { "Properties".to_string() } else { format!("Properties of {} Items", items.len()) })
        .search_enabled(false)
        .content_width(440)
        .build();
    let page = adw::PreferencesPage::new();
    dialog.add(&page);

    // Header: icon, name, kind.
    let head = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).margin_bottom(6).build();
    let icon = gtk::Image::builder().gicon(&items[0].icon()).pixel_size(96).build();
    let title = if items.len() == 1 { items[0].name() } else { format!("{} items", items.len()) };
    let name = gtk::Label::builder().label(&title).wrap(true).justify(gtk::Justification::Center).css_classes(["title-2"]).build();
    let kind_text = if items.len() == 1 {
        let k = items[0].meta().type_name.clone();
        if k.is_empty() { "Unknown kind".into() } else { k }
    } else {
        let dirs = items.iter().filter(|i| i.is_dir()).count();
        format!("{} folders, {} files", dirs, items.len() - dirs)
    };
    let kind = gtk::Label::builder().label(&kind_text).css_classes(["dim-label"]).build();
    head.append(&icon);
    head.append(&name);
    head.append(&kind);
    let head_group = adw::PreferencesGroup::new();
    head_group.add(&head);
    page.add(&head_group);

    let general = adw::PreferencesGroup::builder().title("General").build();
    page.add(&general);

    let paths: Vec<PathBuf> = items.iter().filter_map(|i| i.path()).collect();
    let size_row = row(&general, "Size", "Calculating…");
    measure_into(&size_row, paths.clone(), items.iter().any(|i| i.is_dir()));

    if let [item] = items {
        if let Some(parent_dir) = item.file().parent() {
            let loc = parent_dir.path().map(|p| paths::display(&p, &paths::home())).unwrap_or_else(|| parent_dir.uri().to_string());
            let r = row(&general, "Location", &loc);
            let open = gtk::Button::builder()
                .icon_name("folder-open-symbolic")
                .tooltip_text("Open Containing Folder")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            open.update_property(&[gtk::accessible::Property::Label("Open Containing Folder")]);
            let f = parent_dir.clone();
            let d = dialog.clone();
            open.connect_clicked(move |b| {
                let launcher = gtk::FileLauncher::new(Some(&f));
                launcher.launch(b.root().and_downcast_ref::<gtk::Window>(), gio::Cancellable::NONE, |_| {});
                d.close();
            });
            r.add_suffix(&open);
        }
        if let Some(t) = item.link_target() {
            row(&general, "Link Target", &t);
        }
        if let Some((orig, deleted)) = item.trash_info() {
            row(&general, "Original Location", &paths::display(&orig, &paths::home()));
            if let Some(d) = deleted {
                row(&general, "Deleted", &d.replace('T', " "));
            }
        }
        if let Some(p) = item.path()
            && let Ok(m) = std::fs::symlink_metadata(&p) {
                row(&general, "Modified", &format::date_full(m.mtime()));
                row(&general, "Accessed", &format::date_full(m.atime()));
                if let Some(c) = item.created() {
                    row(&general, "Created", &format::date_full(c));
                }
            }
    }

    // Drive.
    if let Some(first) = paths.first() {
        let probe = if first.is_dir() { first.clone() } else { first.parent().map(PathBuf::from).unwrap_or_else(|| first.clone()) };
        let drive = adw::PreferencesGroup::builder().title("Drive").build();
        let r = row(&drive, "Free Space", "…");
        page.add(&drive);
        let f = gio::File::for_path(&probe);
        glib::MainContext::default().spawn_local(async move {
            if let Ok(info) = f.query_filesystem_info_future("filesystem::size,filesystem::free,filesystem::type", glib::Priority::LOW).await {
                let size = info.attribute_uint64("filesystem::size");
                let free = info.attribute_uint64("filesystem::free");
                let fs = info.attribute_string("filesystem::type").map(|s| s.to_string()).unwrap_or_default();
                r.set_subtitle(&format!("{} free of {}{}", format::size(free), format::size(size), if fs.is_empty() { String::new() } else { format!(" · {fs}") }));
                let bar = gtk::LevelBar::builder().value(if size > 0 { (size - free) as f64 / size as f64 } else { 0.0 }).width_request(90).valign(gtk::Align::Center).css_classes(["storage-bar"]).build();
                bar.remove_offset_value(Some(gtk::LEVEL_BAR_OFFSET_LOW));
                bar.remove_offset_value(Some(gtk::LEVEL_BAR_OFFSET_HIGH));
                bar.remove_offset_value(Some(gtk::LEVEL_BAR_OFFSET_FULL));
                r.add_suffix(&bar);
            }
        });
    }

    // Permissions: single local item only.
    if let ([item], [path]) = (items, paths.as_slice())
        && let Ok(m) = std::fs::symlink_metadata(path)
            && !m.file_type().is_symlink() {
                page.add(&permissions_group(&dialog, path.clone(), &m, item.is_dir()));
            }

    dialog.present(Some(parent));
}

fn row(group: &adw::PreferencesGroup, title: &str, value: &str) -> adw::ActionRow {
    let r = adw::ActionRow::builder().title(title).subtitle(value).subtitle_selectable(true).css_classes(["property"]).build();
    group.add(&r);
    r
}

/// Fill `row` with the total size, measuring folders on a worker thread.
fn measure_into(row: &adw::ActionRow, paths: Vec<PathBuf>, has_dirs: bool) {
    let (tx, rx) = async_channel::bounded::<(u64, u64)>(1);
    let cancel = Cancel::new();
    let c = cancel.clone();
    std::thread::spawn(move || {
        let mut bytes = 0;
        let mut items = 0;
        for p in &paths {
            let (b, n) = ops::measure(p, &c);
            bytes += b;
            items += n;
        }
        let _ = tx.send_blocking((bytes, items));
    });
    let row = row.clone();
    // Stop measuring if the dialog goes away.
    row.connect_destroy(move |_| cancel.cancel());
    glib::MainContext::default().spawn_local(async move {
        if let Ok((bytes, items)) = rx.recv().await {
            let mut text = format!("{} ({} bytes)", format::size(bytes), group_digits(bytes));
            if has_dirs {
                text.push_str(&format!(" · {}", format::item_count(items)));
            }
            row.set_subtitle(&text);
        }
    });
}

fn permissions_group(dialog: &adw::PreferencesDialog, path: PathBuf, m: &std::fs::Metadata, is_dir: bool) -> adw::PreferencesGroup {
    let mode = m.permissions().mode();
    let owner = perms::user_name(m.uid()).unwrap_or_else(|| m.uid().to_string());
    let group_name = perms::group_name(m.gid()).unwrap_or_else(|| m.gid().to_string());
    let me = perms::current_uid();
    let can_change = me == 0 || me == m.uid();

    let g = adw::PreferencesGroup::builder()
        .title("Permissions")
        .description(if can_change {
            format!("{} ({:o})", perms::mode_string(mode), mode & 0o7777)
        } else {
            format!("{} ({:o}) — only {owner} can change these.", perms::mode_string(mode), mode & 0o7777)
        })
        .build();
    let state = std::rc::Rc::new(std::cell::Cell::new(mode));

    let apply = {
        let path = path.clone();
        let dialog = dialog.clone();
        let state = state.clone();
        let g = g.clone();
        move |new_mode: u32| -> bool {
            match std::fs::set_permissions(&path, std::fs::Permissions::from_mode(new_mode)) {
                Ok(()) => {
                    state.set(new_mode);
                    g.set_description(Some(&format!("{} ({:o})", perms::mode_string(new_mode), new_mode & 0o7777)));
                    true
                }
                Err(e) => {
                    let err = ops::OpError::io(&e, &path, "change the permissions of");
                    dialog.add_toast(adw::Toast::new(&format!("{} {}", err.message, err.hint)));
                    false
                }
            }
        }
    };
    let apply = std::rc::Rc::new(apply);

    for (who, title) in [(Who::Owner, format!("Owner · {owner}")), (Who::Group, format!("Group · {group_name}")), (Who::Others, "Everyone Else".to_string())] {
        let current = perms::access(mode, who);
        let labels: Vec<&str> = Access::CHOICES.iter().map(|a| a.label(is_dir)).collect();
        let list = gtk::StringList::new(&labels);
        let combo = adw::ComboRow::builder().title(&title).model(&list).sensitive(can_change).build();
        match Access::CHOICES.iter().position(|a| *a == current) {
            Some(i) => combo.set_selected(i as u32),
            None => {
                // Unusual bits (write-only): show them, don't silently change.
                list.append(current.label(is_dir));
                combo.set_selected(3);
            }
        }
        let apply = apply.clone();
        let state = state.clone();
        combo.connect_selected_notify(move |c| {
            let Some(a) = Access::CHOICES.get(c.selected() as usize) else { return };
            let new = perms::with_access(state.get(), who, *a, is_dir);
            if new != state.get() && !apply(new) {
                // Put the selector back to what is really on disk.
                let back = perms::access(state.get(), who);
                if let Some(i) = Access::CHOICES.iter().position(|x| *x == back) {
                    c.set_selected(i as u32);
                }
            }
        });
        g.add(&combo);
    }

    if !is_dir {
        let exec = adw::SwitchRow::builder()
            .title("Allow Running as a Program")
            .subtitle("Needed for scripts and AppImages")
            .active(perms::is_executable(mode))
            .sensitive(can_change)
            .build();
        let apply = apply.clone();
        let state = state.clone();
        exec.connect_active_notify(move |s| {
            let new = perms::with_executable(state.get(), s.is_active());
            if new != state.get() && !apply(new) {
                s.set_active(perms::is_executable(state.get()));
            }
        });
        g.add(&exec);
    }
    g
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
