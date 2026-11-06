// SPDX-License-Identifier: GPL-3.0-or-later
//! Tuck — a fast, friendly file manager for Linux.

mod appearance_ui;
mod breadcrumb;
mod cleaner_ui;
mod debug;
mod dialogs;
mod jobs;
mod menus;
mod ops_ui;
mod pdf;
mod properties;
mod quicklook;
mod search_ui;
mod searchbar;
mod services_ui;
mod storage_ui;
mod terminal_ui;
mod theme_ui;
mod update_ui;
mod thumbs_ui;
mod prefs;
mod folders_ui;
mod info;
mod install_ui;
mod item;
mod pane;
mod sidebar;
mod window;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use tuck_core::paths;
use tuck_core::settings::{FolderViews, Session, load_json};

pub const APP_ID: &str = "io.github.anuzdhk.Tuck";

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
    ("win.search", &["<Ctrl>f"], "Navigation", "Search"),
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
    ("win.toggle-terminal", &["F4"], "View", "Show terminal panel"),
    ("win.toggle-folders", &["F7"], "View", "Show folders panel"),
    ("win.add-to-places", &["<Ctrl>b"], "Navigation", "Add folder to Places"),
    ("win.filter", &["<Ctrl>i"], "Selection", "Filter items"),
    ("win.selection-mode", &["<Ctrl><Shift>s"], "Selection", "Selection mode"),
    ("app.shortcuts", &["<Ctrl>question", "<Ctrl>F1"], "General", "Keyboard shortcuts"),
    ("app.preferences", &["<Ctrl>comma"], "General", "Preferences"),
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
    ("win.redo", &["<Ctrl><Shift>z"], "Files", "Redo"),
    ("win.quick-look", &["<Ctrl>y"], "Files", "Quick Look (also Space)"),
];

fn main() -> glib::ExitCode {
    // Settings from when the app was called Kivo.
    tuck_core::paths::migrate_old_name();
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(if std::env::var_os("TUCK_ACTIONS").is_some() || std::env::var_os("TUCK_SCREENSHOT").is_some() {
            // Scripted test runs get their own instance instead of handing
            // their work to a Tuck the user already has open.
            gio::ApplicationFlags::HANDLES_OPEN | gio::ApplicationFlags::NON_UNIQUE
        } else {
            gio::ApplicationFlags::HANDLES_OPEN
        })
        .build();

    let folder_views: Rc<RefCell<FolderViews>> = Rc::new(RefCell::new({
        let mut fv: FolderViews = load_json(&views_file());
        if !views_file().exists() {
            fv = FolderViews::new();
        }
        fv
    }));

    // Command line: `tuck [--new-window] [FOLDER|URI…]`, `tuck --version`.
    app.add_main_option("new-window", glib::Char::from(b'w'), glib::OptionFlags::NONE, glib::OptionArg::None, "Open a new window", None);
    app.add_main_option("version", glib::Char::from(b'v'), glib::OptionFlags::NONE, glib::OptionArg::None, "Show the version and exit", None);
    app.connect_handle_local_options(|_, opts| {
        if opts.contains("version") {
            println!("Tuck {}", env!("CARGO_PKG_VERSION"));
            return std::ops::ControlFlow::Break(glib::ExitCode::SUCCESS);
        }
        // --new-window needs nothing else: each launch opens a window.
        std::ops::ControlFlow::Continue(())
    });
    app.connect_startup(|app| {
        load_css();
        theme_ui::init();
        gtk::Window::set_default_icon_name(APP_ID);
        search_ui::start();
        // Clean up after a crash or kill in the middle of a copy.
        let removed = tuck_core::ops::journal::recover(&tuck_core::ops::journal::file());
        if removed > 0 {
            eprintln!("Removed {removed} unfinished partial file(s) from an earlier session.");
        }
        apply_shortcuts(app);
        let quit = gio::SimpleAction::new("quit", None);
        let a = app.clone();
        quit.connect_activate(move |_, _| {
            for w in a.windows() {
                w.close();
            }
        });
        app.add_action(&quit);
        let prefs = gio::SimpleAction::new("preferences", None);
        let a = app.clone();
        prefs.connect_activate(move |_, _| prefs::show(a.active_window().as_ref()));
        app.add_action(&prefs);
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
        update_ui::startup_check(&w);
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
        update_ui::startup_check(&w);
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

/// The keys for `action`: the user's choice, or the default.
pub fn accels_for(action: &str) -> Vec<String> {
    if let Some(custom) = theme_ui::settings().shortcuts.get(action) {
        return custom.clone();
    }
    SHORTCUTS.iter().chain(VIEW_SHORTCUTS).find(|(a, ..)| *a == action).map(|(_, k, ..)| k.iter().map(|s| s.to_string()).collect()).unwrap_or_default()
}

/// Bind the application-wide shortcuts (view-only ones are matched by the
/// file view itself).
pub fn apply_shortcuts(app: &impl IsA<gtk::Application>) {
    for (action, ..) in SHORTCUTS {
        let keys = accels_for(action);
        let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
        app.set_accels_for_action(action, &refs);
    }
}

fn show_shortcuts(parent: Option<&gtk::Window>) {
    let page = adw::PreferencesPage::new();
    let mut groups: Vec<(&str, adw::PreferencesGroup)> = Vec::new();
    for (action, _, section, label) in SHORTCUTS.iter().chain(VIEW_SHORTCUTS) {
        let accels = accels_for(action);
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
