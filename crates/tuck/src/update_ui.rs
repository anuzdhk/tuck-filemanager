// SPDX-License-Identifier: GPL-3.0-or-later
//! Preferences › Updates: check GitHub for a newer Tuck, show what's new,
//! download the right package with progress (checksum-verified when the
//! release publishes checksums), then install it: packages through the
//! normal install flow (the package manager upgrades in place), an AppImage
//! by replacing the installed one. Optionally checks once a day at startup.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use tuck_core::ops::Cancel;
use tuck_core::update::{self, Release};

use crate::theme_ui;

/// Quit and start the new AppImage. It starts after a moment, once this
/// instance has gone (a second start would just raise this window).
fn restart_into(b: &gtk::Button, target: &std::path::Path) {
    let spawned = std::process::Command::new("sh")
        .args(["-c", "sleep 1; exec \"$0\""])
        .arg(target)
        .spawn();
    match spawned {
        Ok(_) => {
            if let Some(app) = gtk::gio::Application::default() {
                app.quit();
            }
        }
        Err(e) => b.set_tooltip_text(Some(&format!("Couldn't restart: {e}"))),
    }
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn downloads_dir() -> PathBuf {
    let dirs = tuck_core::paths::UserDirs::load();
    dirs.get(tuck_core::paths::UserDir::Downloads).map(PathBuf::from).unwrap_or_else(|| tuck_core::paths::home().join("Downloads"))
}

/// The package kind for this machine.
fn kind() -> update::PackageKind {
    let fam = tuck_core::install::family(&tuck_core::install::read_os_release());
    update::package_kind(fam, std::env::var_os("APPIMAGE").is_some())
}

/// Check in the background; `done` gets the result on the main thread.
fn check_async(done: impl FnOnce(Result<Release, String>) + 'static) {
    glib::MainContext::default().spawn_local(async move {
        let r = gtk::gio::spawn_blocking(update::latest).await.unwrap_or_else(|_| Err("The update check failed.".into()));
        done(r);
    });
}

/// Once a day at startup, if the user turned it on: say so when there's a
/// newer version.
pub fn startup_check(win: &crate::window::Window) {
    let s = theme_ui::settings();
    if !s.check_updates || now() - s.last_update_check < 24 * 3600 || (std::env::var_os("TUCK_ACTIONS").is_some() && std::env::var_os("TUCK_UPDATE_URL").is_none()) {
        return;
    }
    theme_ui::update_quiet(|s| s.last_update_check = now());
    let weak = win.downgrade();
    check_async(move |r| {
        let (Some(w), Ok(rel)) = (crate::window::Window::from_weak(&weak), r) else { return };
        if update::is_newer(&rel.tag, update::CURRENT) {
            // A desktop notification too: the window may be in the background.
            if let Some(app) = w.gtk_window().application() {
                let n = gtk::gio::Notification::new(&format!("Tuck {} is available", rel.version()));
                n.set_body(Some("Open Preferences › Updates to install it."));
                n.set_default_action("app.updates");
                n.add_button("Update", "app.updates");
                app.send_notification(Some("update"), &n);
            }
            let gw = w.gtk_window();
            w.toast_with_button(&format!("Tuck {} is available", rel.version()), "Details", move || {
                crate::prefs::show_page(Some(gw.upcast_ref()), Some("updates"));
            });
        }
    });
}

const DESKTOP_ID: &str = "io.github.anuzdhk.Tuck.desktop";

/// Is Tuck the app that opens folders?
fn is_default_fm() -> bool {
    gtk::gio::AppInfo::default_for_type("inode/directory", false).and_then(|a| a.id()).is_some_and(|id| id == DESKTOP_ID)
}

/// "Default File Manager": make Tuck open folders everywhere (the same as
/// `xdg-mime default io.github.anuzdhk.Tuck.desktop inode/directory`).
fn default_fm_group() -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Default File Manager").build();
    let row = adw::ActionRow::builder().title("Open Folders With Tuck").build();
    let button = gtk::Button::builder().label("Make Default").valign(gtk::Align::Center).build();
    row.add_suffix(&button);
    group.add(&row);
    let installed = gtk::gio::AppInfo::all().into_iter().find(|a| a.id().is_some_and(|id| id == DESKTOP_ID));
    let sync = {
        let (row, button) = (row.clone(), button.clone());
        let installed = installed.is_some();
        move || {
            if !installed {
                row.set_subtitle("Install Tuck first (a package, or ./install.sh) so other apps can find it");
                button.set_sensitive(false);
            } else if is_default_fm() {
                row.set_subtitle("Tuck opens folders from other apps, the desktop and downloads");
                button.set_label("Default");
                button.set_sensitive(false);
            } else {
                let current = gtk::gio::AppInfo::default_for_type("inode/directory", false).map(|a| a.display_name().to_string());
                row.set_subtitle(&match current {
                    Some(name) => format!("Folders currently open in {}", glib::markup_escape_text(&name)),
                    None => "No app is set to open folders".to_string(),
                });
                button.set_label("Make Default");
                button.set_sensitive(true);
            }
        }
    };
    sync();
    button.connect_clicked(move |b| {
        let Some(app) = &installed else { return };
        let mut err = None;
        for t in ["inode/directory", "application/x-gnome-saved-search"] {
            if let Err(e) = app.set_as_default_for_type(t) {
                err.get_or_insert(e);
            }
        }
        if let Some(e) = err
            && let Some(d) = b.ancestor(adw::PreferencesDialog::static_type()).and_downcast::<adw::PreferencesDialog>()
        {
            d.add_toast(adw::Toast::new(&format!("Couldn't change the default: {}", e.message())));
        }
        sync();
    });
    group.add(&picker_row());
    group
}

/// "Pick Files With Tuck": other apps' Open and Save dialogs become Tuck
/// windows (the portal backend, see `crate::portal`).
fn picker_row() -> adw::SwitchRow {
    use tuck_core::portal;
    let row = adw::SwitchRow::builder().title("Pick Files With Tuck").subtitle_lines(0).build();
    if !portal::installed() {
        row.set_subtitle("Open and Save dialogs in other apps can be Tuck windows. Needs Tuck installed from a package (.deb, .rpm, Arch), not the AppImage.");
        row.set_sensitive(false);
        return row;
    }
    row.set_subtitle("Open and Save dialogs in other apps become Tuck windows. Apps already open pick it up once restarted.");
    row.set_active(portal::is_enabled());
    row.connect_active_notify(|r| {
        let on = r.is_active();
        if on == portal::is_enabled() {
            return;
        }
        match portal::set_enabled(on) {
            // The portal reads its config at start: restart it (the desktop
            // starts it again on the next request).
            Ok(_) => {
                let _ = gtk::gio::Subprocess::newv(&["systemctl".as_ref(), "--user".as_ref(), "try-restart".as_ref(), "xdg-desktop-portal.service".as_ref()], gtk::gio::SubprocessFlags::NONE);
            }
            Err(e) => {
                if let Some(d) = r.ancestor(adw::PreferencesDialog::static_type()).and_downcast::<adw::PreferencesDialog>() {
                    d.add_toast(adw::Toast::new(&format!("Couldn't change the file picker: {e}")));
                }
                r.set_active(!on);
            }
        }
    });
    row
}

/// The Updates page of Preferences.
pub fn page() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder().title("Updates").name("updates").icon_name("tuck-downloads-symbolic").build();

    let group = adw::PreferencesGroup::builder().title("Tuck").description(format!("You're running version {}.", update::CURRENT)).build();
    page.add(&group);
    let check_row = adw::ActionRow::builder().title("Check for Updates").subtitle("Looks for a newer version on GitHub").build();
    let check_btn = gtk::Button::builder().label("Check Now").valign(gtk::Align::Center).build();
    let spinner = adw::Spinner::builder().visible(false).valign(gtk::Align::Center).build();
    check_row.add_suffix(&spinner);
    check_row.add_suffix(&check_btn);
    check_row.set_activatable_widget(Some(&check_btn));
    group.add(&check_row);
    let auto = adw::SwitchRow::builder()
        .title("Check Automatically")
        .subtitle("Once a day when Tuck starts. Only the version number is fetched; nothing is downloaded without asking.")
        .active(theme_ui::settings().check_updates)
        .build();
    auto.connect_active_notify(|r| {
        let on = r.is_active();
        theme_ui::update_quiet(|s| s.check_updates = on);
    });
    group.add(&auto);

    page.add(&default_fm_group());

    // Filled in after a check.
    let result = adw::PreferencesGroup::builder().visible(false).build();
    page.add(&result);
    let rows: Rc<RefCell<Vec<gtk::Widget>>> = Rc::default();

    let (r2, rows2, spinner2) = (result.clone(), rows.clone(), spinner.clone());
    check_btn.connect_clicked(move |b| {
        b.set_sensitive(false);
        spinner2.set_visible(true);
        let (b, result, rows, spinner) = (b.clone(), r2.clone(), rows2.clone(), spinner2.clone());
        check_async(move |r| {
            b.set_sensitive(true);
            spinner.set_visible(false);
            theme_ui::update_quiet(|s| s.last_update_check = now());
            show_result(&result, &rows, r);
        });
    });
    page
}

fn clear(group: &adw::PreferencesGroup, rows: &RefCell<Vec<gtk::Widget>>) {
    for w in rows.borrow_mut().drain(..) {
        group.remove(&w);
    }
}

fn add(group: &adw::PreferencesGroup, rows: &RefCell<Vec<gtk::Widget>>, w: &impl IsA<gtk::Widget>) {
    group.add(w);
    rows.borrow_mut().push(w.clone().upcast());
}

fn show_result(group: &adw::PreferencesGroup, rows: &Rc<RefCell<Vec<gtk::Widget>>>, r: Result<Release, String>) {
    clear(group, rows);
    group.set_visible(true);
    let rel = match r {
        Err(e) => {
            group.set_title("Couldn't Check");
            add(group, rows, &adw::ActionRow::builder().title(glib::markup_escape_text(&e)).build());
            return;
        }
        Ok(rel) => rel,
    };
    if !update::is_newer(&rel.tag, update::CURRENT) {
        group.set_title("You're Up to Date");
        add(group, rows, &adw::ActionRow::builder().title(format!("Tuck {} is the latest version.", update::CURRENT)).build());
        return;
    }
    group.set_title(&format!("Tuck {} Is Available", rel.version()));
    if let Some(notes) = rel.body.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        let label = gtk::Label::builder()
            .label(notes)
            .wrap(true)
            .xalign(0.0)
            .selectable(true)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        let row = adw::PreferencesRow::builder().activatable(false).child(&label).build();
        add(group, rows, &row);
    }
    let Some((asset, kind_found)) = update::pick_asset(&rel, kind(), std::env::consts::ARCH).map(|(a, k)| (a.clone(), k)) else {
        let row = adw::ActionRow::builder()
            .title("No package for this system")
            .subtitle("This release has nothing for your distribution or processor. Open the release page to see what's there.")
            .build();
        add(group, rows, &row);
        add_page_link(group, rows, &rel);
        return;
    };

    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(&asset.name))
        .subtitle(format!("{} · {}", kind_found.label(), tuck_core::format::size(asset.size)))
        .build();
    let download = gtk::Button::builder().label("Download").css_classes(["suggested-action"]).valign(gtk::Align::Center).build();
    row.add_suffix(&download);
    add(group, rows, &row);
    let bar = gtk::ProgressBar::builder().show_text(true).visible(false).margin_top(8).margin_bottom(8).margin_start(12).margin_end(12).build();
    let bar_row = adw::PreferencesRow::builder().activatable(false).child(&bar).visible(false).build();
    add(group, rows, &bar_row);
    add_page_link(group, rows, &rel);

    let cancel = Rc::new(RefCell::new(Cancel::new()));
    let busy = Rc::new(std::cell::Cell::new(false));
    let downloaded: Rc<RefCell<Option<PathBuf>>> = Rc::default();
    let replaced: Rc<RefCell<Option<PathBuf>>> = Rc::default();
    let rel = Rc::new(rel);
    download.connect_clicked(move |b| {
        // AppImage swapped in: the button restarts into it.
        if let Some(target) = replaced.borrow().clone() {
            restart_into(b, &target);
            return;
        }
        // Done: the button now installs the downloaded file.
        let done = downloaded.borrow().clone();
        if let Some(path) = done {
            if kind_found == update::PackageKind::AppImage {
                let target = update::appimage_target(std::env::var_os("APPIMAGE").map(PathBuf::from).as_deref(), &tuck_core::paths::home());
                let menu = tuck_core::install::appimage::Dirs::user().menu;
                match update::replace_appimage(&path, &target, &menu) {
                    Ok(_) => {
                        row.set_subtitle(&glib::markup_escape_text(&format!("Installed to {}", target.display())));
                        b.set_label("Restart Tuck");
                        *replaced.borrow_mut() = Some(target);
                    }
                    Err(e) => row.set_subtitle(&glib::markup_escape_text(&format!("Couldn't install the update: {e}"))),
                }
                return;
            }
            let win = b.root().and_downcast::<gtk::Window>().and_then(|d| d.transient_for().or(Some(d)));
            let tw = win.as_ref().and_then(crate::window::Window::find).or_else(crate::window::Window::any);
            if let Some(w) = tw {
                // Close Preferences first, then show the install dialog once
                // it's gone (two dialogs at once can hide the second).
                match b.ancestor(adw::Dialog::static_type()).and_downcast::<adw::Dialog>() {
                    Some(dialog) => {
                        let w = w.clone();
                        let path = std::cell::Cell::new(Some(path));
                        dialog.connect_closed(move |_| {
                            if let Some(p) = path.take() {
                                w.install_file(p);
                            }
                        });
                        dialog.close();
                    }
                    None => w.install_file(path),
                }
            }
            return;
        }
        if busy.get() {
            // The button reads "Cancel" while downloading.
            cancel.borrow().cancel();
            return;
        }
        busy.set(true);
        *cancel.borrow_mut() = Cancel::new();
        b.set_label("Cancel");
        b.remove_css_class("suggested-action");
        bar_row.set_visible(true);
        bar.set_visible(true);
        bar.set_fraction(0.0);
        bar.set_text(Some("Starting…"));
        let (tx, rx) = async_channel::unbounded::<(u64, u64)>();
        let (rel2, asset2, c) = ((*rel).clone(), asset.clone(), cancel.borrow().clone());
        let job = gtk::gio::spawn_blocking(move || {
            update::download(&rel2, &asset2, &downloads_dir(), &c, &mut |d, t| {
                let _ = tx.send_blocking((d, t));
            })
        });
        let bar2 = bar.clone();
        glib::MainContext::default().spawn_local(async move {
            while let Ok((d, t)) = rx.recv().await {
                if t > 0 {
                    bar2.set_fraction(d as f64 / t as f64);
                    bar2.set_text(Some(&format!("{} of {}", tuck_core::format::size(d), tuck_core::format::size(t))));
                } else {
                    bar2.pulse();
                    bar2.set_text(Some(&tuck_core::format::size(d)));
                }
            }
        });
        let (b, bar, bar_row, row, busy, downloaded) = (b.clone(), bar.clone(), bar_row.clone(), row.clone(), busy.clone(), downloaded.clone());
        glib::MainContext::default().spawn_local(async move {
            let res = job.await.unwrap_or_else(|_| Err("The download failed.".into()));
            busy.set(false);
            match res {
                Ok((path, verified)) => {
                    bar.set_fraction(1.0);
                    bar.set_text(Some(if verified { "Downloaded and verified" } else { "Downloaded (this release publishes no checksum)" }));
                    row.set_subtitle(&glib::markup_escape_text(&format!("Saved to {}", path.display())));
                    b.set_label("Install…");
                    b.add_css_class("suggested-action");
                    *downloaded.borrow_mut() = Some(path);
                }
                Err(e) => {
                    bar.set_visible(false);
                    bar_row.set_visible(false);
                    row.set_subtitle(&glib::markup_escape_text(&e));
                    b.set_label("Try Again");
                    b.add_css_class("suggested-action");
                }
            }
        });
    });
}

fn add_page_link(group: &adw::PreferencesGroup, rows: &RefCell<Vec<gtk::Widget>>, rel: &Release) {
    if rel.page.is_empty() {
        return;
    }
    let row = adw::ActionRow::builder().title("Release Page").subtitle("See every download and the full notes on GitHub").activatable(true).build();
    row.add_suffix(&gtk::Image::from_icon_name("tuck-link-symbolic"));
    let url = rel.page.clone();
    row.connect_activated(move |r| {
        let launcher = gtk::UriLauncher::new(&url);
        launcher.launch(r.root().and_downcast_ref::<gtk::Window>(), gtk::gio::Cancellable::NONE, |_| {});
    });
    add(group, rows, &row);
}
