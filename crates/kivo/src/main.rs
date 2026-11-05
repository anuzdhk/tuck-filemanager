// SPDX-License-Identifier: GPL-3.0-or-later
//! Kivo — a fast, friendly file manager for Linux.

mod breadcrumb;
mod debug;
mod dialogs;
mod jobs;
mod menus;
mod ops_ui;
mod pdf;
mod properties;
mod quicklook;
mod thumbs_ui;
mod info;
mod item;
mod pane;
mod sidebar;
mod window;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use kivo_core::paths;
use kivo_core::settings::{FolderViews, Session, load_json};

pub const APP_ID: &str = "io.github.anuzdhk.Kivo";

pub fn views_file() -> PathBuf {
    paths::config_dir().join("views.json")
}

pub fn session_file() -> PathBuf {
    paths::state_dir().join("session.json")
}

/// (action, accelerators, description) — one table drives both the key
/// bindings and the Keyboard Shortcuts window.
pub const SHORTCUTS: &[(&str, &[&str], &str, &str)] = &[
    ("win.back", &["<Alt>Left"], "Navigation", "Back"),
    ("win.forward", &["<Alt>Right"], "Navigation", "Forward"),
    ("win.up", &["<Alt>Up"], "Navigation", "Enclosing folder"),
    ("win.home", &["<Alt>Home"], "Navigation", "Home folder"),
    ("win.location", &["<Ctrl>l", "F6"], "Navigation", "Edit location"),
    ("win.reload", &["F5", "<Ctrl>r"], "Navigation", "Reload"),
    ("win.new-tab", &["<Ctrl>t"], "Tabs and Panes", "New tab"),
    ("win.close-tab", &["<Ctrl>w"], "Tabs and Panes", "Close tab"),
    ("win.reopen-tab", &["<Ctrl><Shift>t"], "Tabs and Panes", "Reopen closed tab"),
    ("win.split", &["F3"], "Tabs and Panes", "Split view"),
    ("win.switch-pane", &["<Ctrl>F3"], "Tabs and Panes", "Switch to other pane"),
    ("app.new-window", &["<Ctrl>n"], "Tabs and Panes", "New window"),
    ("win.view-mode::icons", &["<Ctrl>1"], "View", "Icons"),
    ("win.view-mode::compact", &["<Ctrl>2"], "View", "Compact"),
    ("win.view-mode::details", &["<Ctrl>3"], "View", "Details"),
    ("win.zoom-in", &["<Ctrl>plus", "<Ctrl>equal", "<Ctrl>KP_Add"], "View", "Zoom in"),
    ("win.zoom-out", &["<Ctrl>minus", "<Ctrl>KP_Subtract"], "View", "Zoom out"),
    ("win.zoom-reset", &["<Ctrl>0", "<Ctrl>KP_0"], "View", "Reset zoom"),
    ("win.show-hidden", &["<Ctrl>h", "<Alt>period"], "View", "Show hidden files"),
    ("win.toggle-sidebar", &["F9"], "View", "Show sidebar"),
    ("win.toggle-info", &["F11"], "View", "Show information panel"),
    ("win.filter", &["<Ctrl>i"], "Selection", "Filter items"),
    ("app.shortcuts", &["<Ctrl>question", "<Ctrl>F1"], "General", "Keyboard shortcuts"),
    ("app.quit", &["<Ctrl>q"], "General", "Quit"),
];

/// Shortcuts that only apply while a file view has focus, so they never
/// steal keys from text fields (Delete, Ctrl+C, Ctrl+A …).
pub const VIEW_SHORTCUTS: &[(&str, &[&str], &str, &str)] = &[
    ("win.select-all", &["<Ctrl>a"], "Selection", "Select all"),
    ("win.invert-selection", &["<Ctrl><Shift>a"], "Selection", "Invert selection"),
    ("win.copy", &["<Ctrl>c"], "Files", "Copy"),
    ("win.cut", &["<Ctrl>x"], "Files", "Cut"),
    ("win.paste", &["<Ctrl>v"], "Files", "Paste"),
    ("win.rename", &["F2"], "Files", "Rename"),
    ("win.duplicate", &["<Ctrl>d"], "Files", "Duplicate"),
    ("win.trash", &["Delete", "KP_Delete"], "Files", "Move to Trash"),
    ("win.delete", &["<Shift>Delete", "<Shift>KP_Delete"], "Files", "Delete permanently"),
    ("win.new-folder", &["<Ctrl><Shift>n", "F10"], "Files", "New folder"),
    ("win.properties", &["<Alt>Return", "<Alt>KP_Enter"], "Files", "Properties"),
    ("win.copy-path", &["<Ctrl><Alt>c"], "Files", "Copy path"),
    ("win.open-terminal", &["<Shift>F4"], "Files", "Open terminal here"),
    ("win.undo", &["<Ctrl>z"], "Files", "Undo"),
    ("win.quick-look", &["<Ctrl>y"], "Files", "Quick Look (also Space)"),
];

fn main() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    let folder_views: Rc<RefCell<FolderViews>> = Rc::new(RefCell::new({
        let mut fv: FolderViews = load_json(&views_file());
        if !views_file().exists() {
            fv = FolderViews::new();
        }
        fv
    }));

    app.connect_startup(|app| {
        load_css();
        // Clean up after a crash or kill in the middle of a copy.
        let removed = kivo_core::ops::journal::recover(&kivo_core::ops::journal::file());
        if removed > 0 {
            eprintln!("Removed {removed} unfinished partial file(s) from an earlier session.");
        }
        for (action, accels, _, _) in SHORTCUTS {
            app.set_accels_for_action(action, accels);
        }
        let quit = gio::SimpleAction::new("quit", None);
        let a = app.clone();
        quit.connect_activate(move |_, _| {
            for w in a.windows() {
                w.close();
            }
        });
        app.add_action(&quit);
        let shortcuts = gio::SimpleAction::new("shortcuts", None);
        let a = app.clone();
        shortcuts.connect_activate(move |_, _| show_shortcuts(a.active_window().as_ref()));
        app.add_action(&shortcuts);
    });

    let fv = folder_views.clone();
    let new_window = gio::SimpleAction::new("new-window", None);
    let a = app.clone();
    new_window.connect_activate(move |_, _| {
        let w = window::Window::new(&a, fv.clone());
        w.open_tab(gio::File::for_path(paths::home()), true);
        w.present();
    });
    app.add_action(&new_window);

    let fv = folder_views.clone();
    app.connect_activate(move |app| {
        let w = window::Window::new(app, fv.clone());
        let session: Session = load_json(&session_file());
        if !w.restore_session(&session) {
            w.open_tab(gio::File::for_path(paths::home()), true);
        }
        w.present();
        debug::run_script(app);
    });

    let fv = folder_views;
    app.connect_open(move |app, files, _| {
        let w = window::Window::new(app, fv.clone());
        for (i, f) in files.iter().enumerate() {
            // Opening a file shows its folder.
            let dir = match f.query_file_type(gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE) {
                gio::FileType::Directory | gio::FileType::Mountable => f.clone(),
                _ => f.parent().unwrap_or_else(|| f.clone()),
            };
            w.open_tab(dir, i == 0);
        }
        w.present();
        debug::run_script(app);
    });

    app.run()
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("style.css"));
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().expect("display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

fn show_shortcuts(parent: Option<&gtk::Window>) {
    let page = adw::PreferencesPage::new();
    let mut groups: Vec<(&str, adw::PreferencesGroup)> = Vec::new();
    for (_, accels, section, label) in SHORTCUTS.iter().chain(VIEW_SHORTCUTS) {
        let group = match groups.iter().find(|(s, _)| s == section) {
            Some((_, g)) => g.clone(),
            None => {
                let g = adw::PreferencesGroup::builder().title(*section).build();
                page.add(&g);
                groups.push((section, g.clone()));
                g
            }
        };
        let row = adw::ActionRow::builder().title(*label).build();
        let keys = gtk::ShortcutLabel::new(&accels.join(" "));
        keys.set_valign(gtk::Align::Center);
        row.add_suffix(&keys);
        group.add(&row);
    }
    // Keys handled inside the file views rather than as actions.
    let g = adw::PreferencesGroup::builder().title("In the File View").build();
    for (keys, label) in [
        ("Return", "Open"),
        ("<Ctrl>Return", "Open folder in new tab"),
        ("BackSpace", "Back"),
        ("<Ctrl>Tab", "Next tab"),
    ] {
        let row = adw::ActionRow::builder().title(label).build();
        let k = gtk::ShortcutLabel::new(keys);
        k.set_valign(gtk::Align::Center);
        row.add_suffix(&k);
        g.add(&row);
    }
    let row = adw::ActionRow::builder().title("Type a name to jump to it").subtitle("Type-ahead selection").build();
    g.add(&row);
    page.add(&g);

    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    tv.set_content(Some(&page));
    let dialog = adw::Dialog::builder()
        .title("Keyboard Shortcuts")
        .content_width(520)
        .content_height(640)
        .child(&tv)
        .build();
    dialog.present(parent);
}
