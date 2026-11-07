// SPDX-License-Identifier: GPL-3.0-or-later
//! Preferences › Storage: every drive with how much space is used and free,
//! its file system, where it's mounted, and buttons to open or eject it.

use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gio, glib};
use tuck_core::format;

struct Drive {
    name: String,
    icon: String,
    root: gio::File,
    mount: Option<gio::Mount>,
}

/// The system drive, the home drive if it's separate, and every mount.
fn drives() -> Vec<Drive> {
    let mut out = vec![Drive { name: "Computer".into(), icon: "tuck-drive-symbolic".into(), root: gio::File::for_path("/"), mount: None }];
    let dev = |p: &std::path::Path| std::fs::metadata(p).map(|m| m.dev()).ok();
    let home = tuck_core::paths::home();
    let mut seen: Vec<u64> = dev(std::path::Path::new("/")).into_iter().collect();
    // The user's other partitions (a separate /home, a data disk).
    for p in tuck_core::drives::list() {
        if let Some(d) = dev(&p.mount) {
            seen.push(d);
        }
        let name = if home.starts_with(&p.mount) {
            "Home Disk".to_string()
        } else {
            p.mount.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.mount.display().to_string())
        };
        out.push(Drive { name, icon: "tuck-drive-symbolic".into(), root: gio::File::for_path(&p.mount), mount: None });
    }
    for m in gio::VolumeMonitor::get().mounts() {
        if m.is_shadowed() {
            continue;
        }
        let root = m.root();
        if let Some(p) = root.path() {
            match dev(&p) {
                Some(d) if seen.contains(&d) => continue,
                Some(d) => seen.push(d),
                None => {}
            }
        }
        let icon = m.symbolic_icon().downcast::<gio::ThemedIcon>().ok().and_then(|t| t.names().first().map(|n| n.to_string())).unwrap_or_else(|| "tuck-removable-symbolic".into());
        out.push(Drive { name: m.name().to_string(), icon, root, mount: Some(m) });
    }
    out
}

fn open_in_tuck(file: &gio::File, from: &gtk::Widget) {
    if let Some(w) = crate::window::Window::any()
        && let Some(p) = w.active_pane()
    {
        p.navigate(file.clone());
        if let Some(d) = from.ancestor(adw::Dialog::static_type()).and_downcast::<adw::Dialog>() {
            d.close();
        }
    }
}

fn drive_group(d: Drive) -> adw::PreferencesGroup {
    let where_ = d.root.path().map(|p: PathBuf| p.display().to_string()).unwrap_or_else(|| d.root.uri().to_string());
    let group = adw::PreferencesGroup::builder().title(glib::markup_escape_text(&d.name)).description(glib::markup_escape_text(&where_)).build();
    let open = gtk::Button::builder().icon_name("tuck-open-folder-symbolic").tooltip_text("Open").css_classes(["flat"]).valign(gtk::Align::Center).build();
    open.update_property(&[gtk::accessible::Property::Label(&format!("Open {}", d.name))]);
    let root = d.root.clone();
    open.connect_clicked(move |b| open_in_tuck(&root, b.upcast_ref()));
    let suffix = gtk::Box::builder().spacing(4).build();
    suffix.append(&open);
    if let Some(m) = d.mount.clone().filter(|m| m.can_unmount() || m.can_eject()) {
        let eject = gtk::Button::builder().icon_name("tuck-eject-symbolic").tooltip_text("Eject").css_classes(["flat"]).valign(gtk::Align::Center).build();
        eject.update_property(&[gtk::accessible::Property::Label(&format!("Eject {}", d.name))]);
        let g = group.clone();
        eject.connect_clicked(move |b| {
            b.set_sensitive(false);
            let op = gtk::MountOperation::new(b.root().and_downcast_ref::<gtk::Window>());
            let g = g.clone();
            let done = move |r: Result<(), glib::Error>| match r {
                Ok(()) => g.set_visible(false),
                Err(e) => g.set_description(Some(&glib::markup_escape_text(&format!("Couldn't eject: {}", e.message())))),
            };
            if m.can_eject() {
                m.eject_with_operation(gio::MountUnmountFlags::NONE, Some(&op), gio::Cancellable::NONE, done);
            } else {
                m.unmount_with_operation(gio::MountUnmountFlags::NONE, Some(&op), gio::Cancellable::NONE, done);
            }
        });
        suffix.append(&eject);
    }
    group.set_header_suffix(Some(&suffix));

    // One card: a big bar, then used / free / total and the file system.
    let bar = gtk::LevelBar::builder().min_value(0.0).max_value(1.0).css_classes(["storage-bar", "storage-big"]).build();
    for o in [gtk::LEVEL_BAR_OFFSET_LOW, gtk::LEVEL_BAR_OFFSET_HIGH, gtk::LEVEL_BAR_OFFSET_FULL] {
        bar.remove_offset_value(Some(o));
    }
    let percent = gtk::Label::builder().xalign(1.0).css_classes(["title-3", "numeric"]).build();
    let summary = gtk::Label::builder().xalign(0.0).hexpand(true).css_classes(["heading"]).label("Measuring…").build();
    let top = gtk::Box::builder().spacing(10).build();
    top.append(&gtk::Image::builder().icon_name(&d.icon).pixel_size(24).build());
    top.append(&summary);
    top.append(&percent);
    let facts = gtk::Grid::builder().column_spacing(24).row_spacing(4).build();
    let card = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).css_classes(["card", "storage-card"]).build();
    card.append(&top);
    card.append(&bar);
    card.append(&facts);
    group.add(&card);

    let root = d.root.clone();
    glib::MainContext::default().spawn_local(async move {
        let attrs = "filesystem::size,filesystem::free,filesystem::used,filesystem::type,filesystem::readonly";
        let Ok(info) = root.query_filesystem_info_future(attrs, glib::Priority::LOW).await else {
            summary.set_label("Space information isn't available for this drive");
            bar.set_visible(false);
            return;
        };
        let size = info.attribute_uint64("filesystem::size");
        let free = info.attribute_uint64("filesystem::free");
        let used = if info.has_attribute("filesystem::used") { info.attribute_uint64("filesystem::used") } else { size.saturating_sub(free) };
        let frac = if size > 0 { used as f64 / size as f64 } else { 0.0 };
        summary.set_label(&format!("{} free", format::size(free)));
        percent.set_label(&format!("{:.0}% used", frac * 100.0));
        bar.set_value(frac);
        if frac > 0.9 {
            bar.add_css_class("nearly-full");
            summary.set_label(&format!("{} free · almost full", format::size(free)));
        }
        bar.update_property(&[gtk::accessible::Property::Label(&format!("{:.0}% used", frac * 100.0))]);
        let mut row = 0;
        let mut fact = |k: &str, v: String| {
            facts.attach(&gtk::Label::builder().label(k).xalign(0.0).css_classes(["dim-label"]).build(), 0, row, 1, 1);
            facts.attach(&gtk::Label::builder().label(v).xalign(0.0).selectable(true).css_classes(["numeric"]).build(), 1, row, 1, 1);
            row += 1;
        };
        fact("Used", format::size(used));
        fact("Free", format::size(free));
        fact("Total", format::size(size));
        if let Some(t) = info.attribute_string("filesystem::type") {
            let ro = if info.boolean("filesystem::readonly") { " (read-only)" } else { "" };
            fact("File system", format!("{t}{ro}"));
        }
    });
    group
}

/// The Storage page of Preferences.
pub fn page() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder().title("Storage").name("storage").icon_name("tuck-drive-symbolic").build();
    for d in drives() {
        page.add(&drive_group(d));
    }
    page
}
