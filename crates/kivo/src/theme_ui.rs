// SPDX-License-Identifier: GPL-3.0-or-later
//! Applies appearance settings live: theme colours (as libadwaita CSS
//! variables), light/dark scheme, translucency (only with a compositor),
//! text size, density, reduced motion, and the icon set.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use adw::prelude::*;
use gtk::{gdk, gio};
use kivo_core::icons::{self, Doc, Folder};
use kivo_core::paths::{self, UserDir, UserDirs};
use kivo_core::settings::{AppSettings, Density, IconStyle, load_json, save_json};
use kivo_core::theme::{self, Palette, Theme};

const APP_ICON: &str = include_str!("../../../data/icons/app/io.github.anuzdhk.Kivo.svg");

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/icons.rs"));
}

/// UI icons: (Kivo name, Tabler file, Material file).
pub const UI_ICONS: &[(&str, &str, &str)] = &[
    ("back", "arrow-left", "arrow_back"),
    ("forward", "arrow-right", "arrow_forward"),
    ("up", "arrow-up", "arrow_upward"),
    ("chevron", "chevron-right", "chevron_right"),
    ("sidebar", "layout-sidebar", "side_navigation"),
    ("grid", "layout-grid", "grid_view"),
    ("compact", "list", "view_list"),
    ("details", "list-details", "table_rows"),
    ("split", "layout-columns", "vertical_split"),
    ("info", "info-circle", "info"),
    ("menu", "menu-2", "menu"),
    ("new-tab", "plus", "add"),
    ("search", "search", "search"),
    ("sort", "arrows-sort", "sort"),
    ("close", "x", "close"),
    ("open-folder", "folder-open", "folder_open"),
    ("eject", "player-eject", "eject"),
    ("stop", "square-x", "cancel"),
    ("more", "dots", "more_horiz"),
    ("link", "link", "link"),
    ("home", "home", "home"),
    ("desktop", "device-desktop", "desktop_windows"),
    ("documents", "file-text", "description"),
    ("downloads", "download", "download"),
    ("pictures", "photo", "image"),
    ("music", "music", "music_note"),
    ("videos", "movie", "movie"),
    ("recent", "clock", "history"),
    ("trash", "trash", "delete"),
    ("drive", "server", "hard_drive"),
    ("removable", "usb", "usb"),
    ("warning", "alert-triangle", "warning"),
    ("folder", "folder", "folder"),
    ("file", "file", "draft"),
    ("preferences", "adjustments", "tune"),
    ("star", "star", "star"),
    ("star-filled", "star-filled", "star-fill"),
    ("tag", "tag", "sell"),
];

/// Material file-type icons (shown at file size, tinted with the accent).
const MATERIAL_FILES: &[(&str, &str)] = &[
    ("folder", "folder-fill"),
    ("generic", "draft-fill"),
    ("pdf", "description-fill"),
    ("document", "description-fill"),
    ("text", "description"),
    ("image", "image-fill"),
    ("video", "movie-fill"),
    ("audio", "music_note-fill"),
    ("archive", "folder_zip"),
    ("code", "code"),
    ("spreadsheet", "table_chart"),
    ("presentation", "slideshow"),
    ("app", "apps"),
];

struct State {
    settings: AppSettings,
    provider: gtk::CssProvider,
    base_dpi: i32,
    base_animations: bool,
    default_search: Vec<PathBuf>,
    icon_root: PathBuf,
    generation: u32,
    palette: Palette,
    user_themes: Vec<Theme>,
    user_errors: Vec<String>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static FILE_ICONS: RefCell<std::collections::HashMap<String, gio::Icon>> = RefCell::new(Default::default());
    static ICON_GEN: std::cell::Cell<u32> = const { std::cell::Cell::new(1) };
    static USER_DIRS: UserDirs = UserDirs::load();
}

fn settings_file() -> PathBuf {
    paths::config_dir().join("settings.json")
}

pub fn themes_dir() -> PathBuf {
    paths::config_dir().join("themes")
}

/// Call once at startup, after the display exists.
pub fn init() {
    let display = gdk::Display::default().expect("display");
    let provider = gtk::CssProvider::new();
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1);
    let gs = gtk::Settings::default().expect("settings");
    let base_dpi = match gs.gtk_xft_dpi() {
        d if d > 0 => d,
        _ => 96 * 1024,
    };
    let theme = gtk::IconTheme::for_display(&display);
    let icon_root = paths::cache_home().join("kivo").join("icons");
    let _ = std::fs::remove_dir_all(&icon_root);
    let (user_themes, user_errors) = theme::user_themes(&themes_dir());
    let loaded = load_json::<AppSettings>(&settings_file()).sanitized();
    let state = State {
        settings: loaded,
        provider,
        base_dpi,
        base_animations: gs.is_gtk_enable_animations(),
        default_search: theme.search_path(),
        icon_root,
        generation: 0,
        palette: theme::resolve(&theme::builtin("light").expect("light"), "theme", true),
        user_themes,
        user_errors,
    };
    STATE.with(|s| *s.borrow_mut() = Some(state));
    apply();
    // Follow the desktop's light/dark and high-contrast settings live.
    let sm = adw::StyleManager::default();
    sm.connect_dark_notify(|_| {
        if settings().theme == "system" {
            apply_colors();
        }
    });
    sm.connect_high_contrast_notify(|_| {
        if settings().theme == "system" {
            apply_colors();
        }
    });
}

pub fn settings() -> AppSettings {
    STATE.with(|s| s.borrow().as_ref().map(|s| s.settings.clone()).unwrap_or_default())
}

/// Change settings, save them, and apply immediately.
pub fn update(f: impl FnOnce(&mut AppSettings)) {
    let changed_icons = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(st) = s.as_mut() else { return false };
        let before = st.settings.clone();
        f(&mut st.settings);
        st.settings = st.settings.clone().sanitized();
        let _ = save_json(&settings_file(), &st.settings);
        before.icon_style != st.settings.icon_style || before.accent != st.settings.accent || before.theme != st.settings.theme
    });
    apply();
    if changed_icons {
        crate::window::Window::refresh_all_icons();
    }
}

/// Theme choices: built-ins, then user themes.
pub fn theme_choices() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = theme::CHOICES.iter().map(|(id, n)| (id.to_string(), n.to_string())).collect();
    STATE.with(|s| {
        if let Some(st) = s.borrow().as_ref() {
            v.extend(st.user_themes.iter().map(|t| (t.id.clone(), t.name.clone())));
        }
    });
    v
}

pub fn user_theme_errors() -> Vec<String> {
    STATE.with(|s| s.borrow().as_ref().map(|s| s.user_errors.clone()).unwrap_or_default())
}

pub fn reload_user_themes() {
    let (t, e) = theme::user_themes(&themes_dir());
    STATE.with(|s| {
        if let Some(st) = s.borrow_mut().as_mut() {
            st.user_themes = t;
            st.user_errors = e;
        }
    });
    apply();
}

pub fn palette() -> Palette {
    STATE.with(|s| s.borrow().as_ref().map(|s| s.palette.clone()).expect("theme initialised"))
}

fn current_theme(st: &State) -> Theme {
    let sm = adw::StyleManager::default();
    let id = st.settings.theme.as_str();
    if id == "system" {
        // Read the desktop preference: stop forcing a scheme first.
        sm.set_color_scheme(adw::ColorScheme::Default);
        if sm.is_high_contrast() {
            return theme::builtin("high-contrast").expect("hc");
        }
        return theme::builtin(if sm.is_dark() { "dark" } else { "light" }).expect("builtin");
    }
    if let Some(t) = theme::builtin(id) {
        return t;
    }
    st.user_themes.iter().find(|t| t.id == id).cloned().unwrap_or_else(|| theme::builtin("light").expect("light"))
}

fn apply_colors() {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(st) = s.as_mut() else { return };
        let theme = current_theme(st);
        let composited = gdk::Display::default().is_some_and(|d| d.is_composited());
        st.palette = theme::resolve(&theme, &st.settings.accent, composited);
        let sm = adw::StyleManager::default();
        if st.settings.theme != "system" {
            sm.set_color_scheme(if theme.dark { adw::ColorScheme::ForceDark } else { adw::ColorScheme::ForceLight });
        }
        st.provider.load_from_string(&st.palette.css());
    });
    crate::window::Window::restyle_all();
}

fn apply() {
    apply_colors();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(st) = s.as_mut() else { return };
        let gs = gtk::Settings::default().expect("settings");
        gs.set_gtk_xft_dpi((st.base_dpi as f64 * st.settings.text_scale).round() as i32);
        gs.set_gtk_enable_animations(st.base_animations && !st.settings.reduce_motion);
        write_ui_icons(st);
    });
    FILE_ICONS.with(|c| c.borrow_mut().clear());
    ICON_GEN.with(|g| g.set(g.get() + 1));
}

/// Bumped whenever icons change; items re-fetch their icon when it moves.
pub fn icon_generation() -> u32 {
    ICON_GEN.with(|g| g.get())
}

/// Window CSS classes for the current settings.
pub fn window_classes() -> Vec<&'static str> {
    let st = settings();
    let p = palette();
    let mut v = vec![match st.density {
        Density::Compact => "density-compact",
        Density::Comfortable => "density-comfortable",
        Density::Spacious => "density-spacious",
    }];
    if st.icon_style == IconStyle::Material {
        v.push("icons-material");
    }
    if p.window_alpha < 1.0 {
        v.push("kivo-main");
    }
    if p.high_contrast {
        v.push("kivo-high-contrast");
    }
    v
}

pub const ALL_WINDOW_CLASSES: &[&str] = &["density-compact", "density-comfortable", "density-spacious", "icons-material", "kivo-main", "kivo-high-contrast"];

/// Write the active UI icon set into a fresh folder and point GTK at it.
fn write_ui_icons(st: &mut State) {
    let old = st.icon_root.join(format!("gen-{}", st.generation));
    st.generation += 1;
    let dir = st.icon_root.join(format!("gen-{}", st.generation));
    let actions = dir.join("hicolor").join("scalable").join("actions");
    if std::fs::create_dir_all(&actions).is_err() {
        return;
    }
    let material = st.settings.icon_style == IconStyle::Material;
    fn find(set: &'static [(&'static str, &'static str)], name: &str) -> Option<&'static str> {
        set.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
    }
    for (kivo, tabler, mat) in UI_ICONS {
        let svg = if material { find(embedded::MATERIAL, mat) } else { find(embedded::TABLER, tabler) };
        if let Some(svg) = svg {
            let _ = std::fs::write(actions.join(format!("kivo-{kivo}-symbolic.svg")), svg);
        }
    }
    for (kind, mat) in MATERIAL_FILES {
        if let Some(svg) = find(embedded::MATERIAL, mat) {
            let _ = std::fs::write(actions.join(format!("kivo-m-file-{kind}-symbolic.svg")), svg);
        }
    }
    // The app icon, so windows and About show it even when not installed.
    let apps = dir.join("hicolor").join("scalable").join("apps");
    if std::fs::create_dir_all(&apps).is_ok() {
        let _ = std::fs::write(apps.join(format!("{}.svg", crate::APP_ID)), APP_ICON);
    }
    let mut search = vec![dir];
    search.extend(st.default_search.iter().cloned());
    if let Some(d) = gdk::Display::default() {
        gtk::IconTheme::for_display(&d).set_search_path(&search.iter().map(PathBuf::as_path).collect::<Vec<&Path>>());
    }
    let _ = std::fs::remove_dir_all(old);
}

fn special_folder(path: Option<&Path>) -> Folder {
    let Some(p) = path else { return Folder::Plain };
    match USER_DIRS.with(|d| d.identify(p)) {
        Some(UserDir::Home) => Folder::Home,
        Some(UserDir::Desktop) => Folder::Desktop,
        Some(UserDir::Documents) => Folder::Documents,
        Some(UserDir::Downloads) => Folder::Downloads,
        Some(UserDir::Pictures) => Folder::Pictures,
        Some(UserDir::Music) => Folder::Music,
        Some(UserDir::Videos) => Folder::Videos,
        Some(UserDir::Templates) => Folder::Templates,
        Some(UserDir::Public) => Folder::Public,
        None => Folder::Plain,
    }
}

/// The icon for a file or folder in the current icon style.
pub fn file_icon(name: &str, is_dir: bool, path: Option<&Path>) -> gio::Icon {
    let st = settings();
    let kind = kivo_core::search::Kind::of(name, is_dir);
    if st.icon_style == IconStyle::Material {
        let k = if is_dir { "folder" } else { Doc::for_kind(kind).id() };
        return gio::ThemedIcon::new(&format!("kivo-m-file-{k}-symbolic")).upcast();
    }
    let p = palette();
    let key = if is_dir {
        format!("folder-{}-{}", special_folder(path).id(), p.accent_bg.hex())
    } else {
        format!("doc-{}-{}", Doc::for_kind(kind).id(), icons::badge(name))
    };
    if let Some(i) = FILE_ICONS.with(|c| c.borrow().get(&key).cloned()) {
        return i;
    }
    let dir = paths::cache_home().join("kivo").join("icons").join("files");
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join(format!("{}.svg", key.trim_start_matches('#').replace('#', "")));
    if !file.exists() {
        let svg = if is_dir { icons::folder_svg(special_folder(path), p.accent_bg) } else { icons::doc_svg(Doc::for_kind(kind), &icons::badge(name)) };
        let _ = std::fs::write(&file, svg);
    }
    let icon: gio::Icon = gio::FileIcon::new(&gio::File::for_path(&file)).upcast();
    FILE_ICONS.with(|c| c.borrow_mut().insert(key, icon.clone()));
    icon
}
