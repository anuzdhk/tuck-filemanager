// SPDX-License-Identifier: GPL-3.0-or-later
//! Applies appearance settings live: theme colours (as libadwaita CSS
//! variables), light/dark scheme, translucency (only with a compositor),
//! text size, density, reduced motion, and the icon set.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use adw::prelude::*;
use gtk::{gdk, gio};
use tuck_core::icons::{self, Doc, Folder};
use tuck_core::paths::{self, UserDir, UserDirs};
use tuck_core::settings::{AppSettings, Density, IconStyle, load_json, save_json};
use tuck_core::theme::{self, Palette, Theme};

const APP_ICON: &str = include_str!("../../../data/icons/app/io.github.anuzdhk.Tuck.svg");

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/icons.rs"));
}

/// UI icons: (Tuck name, Tabler file, Material file).
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
    ("server", "server", "hard_drive"),
    ("cleaner", "mop", "mop"),
    ("settings", "settings", "settings"),
    ("columns", "columns-3", "view_column"),
    ("addons", "puzzle", "extension"),
];

/// System icon style: the standard freedesktop names for each UI icon, best
/// first. Copied from the desktop's icon theme; Tabler fills any gaps.
const SYSTEM_UI: &[(&str, &[&str])] = &[
    ("back", &["go-previous-symbolic"]),
    ("forward", &["go-next-symbolic"]),
    ("up", &["go-up-symbolic"]),
    ("chevron", &["go-next-symbolic"]),
    ("sidebar", &["sidebar-show-symbolic", "view-sidebar-start-symbolic", "view-dual-symbolic"]),
    ("grid", &["view-grid-symbolic", "view-app-grid-symbolic"]),
    ("compact", &["view-list-compact-symbolic", "view-list-bullet-symbolic", "view-list-text-symbolic", "view-list-symbolic"]),
    ("details", &["view-list-details-symbolic", "view-list-symbolic", "view-paged-symbolic"]),
    ("split", &["view-dual-symbolic", "view-split-left-right-symbolic"]),
    ("info", &["info-outline-symbolic", "dialog-information-symbolic", "help-about-symbolic"]),
    ("menu", &["open-menu-symbolic"]),
    ("new-tab", &["tab-new-symbolic", "list-add-symbolic"]),
    ("search", &["system-search-symbolic", "edit-find-symbolic"]),
    ("sort", &["view-sort-ascending-symbolic", "view-sort-descending-symbolic"]),
    ("close", &["window-close-symbolic"]),
    ("open-folder", &["folder-open-symbolic", "document-open-symbolic"]),
    ("eject", &["media-eject-symbolic"]),
    ("stop", &["process-stop-symbolic"]),
    ("more", &["view-more-horizontal-symbolic", "view-more-symbolic"]),
    ("link", &["insert-link-symbolic"]),
    ("home", &["user-home-symbolic", "go-home-symbolic"]),
    ("desktop", &["user-desktop-symbolic"]),
    ("documents", &["folder-documents-symbolic", "x-office-document-symbolic"]),
    ("downloads", &["folder-download-symbolic"]),
    ("pictures", &["folder-pictures-symbolic", "image-x-generic-symbolic"]),
    ("music", &["folder-music-symbolic", "audio-x-generic-symbolic"]),
    ("videos", &["folder-videos-symbolic", "video-x-generic-symbolic"]),
    ("recent", &["document-open-recent-symbolic"]),
    ("trash", &["user-trash-symbolic"]),
    ("drive", &["drive-harddisk-symbolic"]),
    ("removable", &["drive-removable-media-symbolic", "media-removable-symbolic"]),
    ("warning", &["dialog-warning-symbolic"]),
    ("folder", &["folder-symbolic"]),
    ("file", &["text-x-generic-symbolic"]),
    ("preferences", &["preferences-system-symbolic", "emblem-system-symbolic"]),
    ("star", &["non-starred-symbolic", "starred-symbolic"]),
    ("star-filled", &["starred-symbolic"]),
    ("tag", &["tag-symbolic", "bookmark-new-symbolic"]),
    ("server", &["network-server-symbolic"]),
    ("cleaner", &["edit-clear-all-symbolic", "user-trash-full-symbolic"]),
    ("columns", &["view-column-symbolic", "view-dual-symbolic"]),
    ("addons", &["application-x-addon-symbolic", "puzzle-piece-symbolic"]),
    ("settings", &["emblem-system-symbolic", "preferences-system-symbolic"]),
];

/// The file holding `names[0..]` in the desktop's icon theme, if any:
/// (contents, extension).
fn system_icon_file(names: &[&str]) -> Option<(Vec<u8>, String)> {
    let display = gdk::Display::default()?;
    let theme = gtk::IconTheme::for_display(&display);
    for n in names {
        if !theme.has_icon(n) {
            continue;
        }
        let paintable = theme.lookup_icon(n, &[], 16, 1, gtk::TextDirection::Ltr, gtk::IconLookupFlags::empty());
        let Some(file) = paintable.file() else { continue };
        let ext = file.path().and_then(|p| p.extension().map(|e| e.to_string_lossy().into_owned()))
            .or_else(|| file.uri().rsplit('.').next().map(str::to_string))
            .unwrap_or_else(|| "svg".into());
        if let Ok((bytes, _)) = file.load_contents(gio::Cancellable::NONE) {
            return Some((bytes.to_vec(), ext));
        }
    }
    None
}

/// The name of the desktop's icon theme (for Preferences).
pub fn system_icon_theme_name() -> String {
    gtk::Settings::default().and_then(|s| s.gtk_icon_theme_name()).map(|s| s.to_string()).unwrap_or_else(|| "Adwaita".into())
}

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
    let icon_root = paths::cache_home().join("tuck").join("icons");
    let _ = std::fs::remove_dir_all(&icon_root);
    let (user_themes, user_errors) = theme::user_themes(&themes_dir());
    let loaded = load_json::<AppSettings>(&settings_file()).sanitized();
    let state = State {
        settings: loaded,
        provider,
        base_dpi,
        default_search: theme.search_path(),
        icon_root,
        generation: 0,
        palette: theme::resolve(&theme::builtin("light").expect("light"), "theme", true),
        user_themes,
        user_errors,
    };
    STATE.with(|s| *s.borrow_mut() = Some(state));
    apply();
    // GTK 4.20+ reports the desktop's reduced-motion preference.
    if gs.find_property(REDUCED_MOTION).is_some() {
        gs.connect_notify_local(Some(REDUCED_MOTION), |_, _| apply());
    }
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

const REDUCED_MOTION: &str = "gtk-interface-reduced-motion";

/// Has the desktop asked apps to reduce motion (GNOME Accessibility ›
/// Reduce Animation, or the portal elsewhere)?
fn system_reduced_motion(gs: &gtk::Settings) -> bool {
    if gs.find_property(REDUCED_MOTION).is_none() {
        return false;
    }
    let v = gs.property_value(REDUCED_MOTION);
    gtk::glib::EnumValue::from_value(&v).is_some_and(|(_, e)| e.nick() == "reduce")
}

pub fn settings() -> AppSettings {
    STATE.with(|s| s.borrow().as_ref().map(|s| s.settings.clone()).unwrap_or_default())
}

/// Change settings that need no restyling (e.g. shortcuts), and save.
pub fn update_quiet(f: impl FnOnce(&mut AppSettings)) {
    STATE.with(|s| {
        if let Some(st) = s.borrow_mut().as_mut() {
            f(&mut st.settings);
            let _ = save_json(&settings_file(), &st.settings);
        }
    });
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

/// A theme by id: built-in or user ("system" has no single theme).
pub fn theme_for(id: &str) -> Option<Theme> {
    theme::builtin(id).or_else(|| STATE.with(|s| s.borrow().as_ref().and_then(|st| st.user_themes.iter().find(|t| t.id == id).cloned())))
}

/// Is `id` one of the user's own theme files?
pub fn is_user_theme(id: &str) -> bool {
    STATE.with(|s| s.borrow().as_ref().is_some_and(|st| st.user_themes.iter().any(|t| t.id == id)))
}

/// Sample icons (folder, PDF, photo, song) in an icon style, for Preferences.
pub fn preview_icons(style: IconStyle) -> Vec<gio::Icon> {
    let samples = ["Report.pdf", "Photo.jpg", "Song.mp3"];
    match style {
        IconStyle::System => std::iter::once(system_file_icon("Folder", true, None))
            .chain(samples.iter().map(|n| system_file_icon(n, false, None)))
            .collect(),
        IconStyle::Material => {
            let accent = palette().accent_color.hex();
            let dir = paths::cache_home().join("tuck").join("icons").join("preview");
            let _ = std::fs::create_dir_all(&dir);
            ["folder-fill", "description-fill", "image-fill", "music_note-fill"]
                .iter()
                .filter_map(|m| {
                    let svg = embedded::MATERIAL.iter().find(|(n, _)| n == m)?.1;
                    // Tint it here: preview files aren't recoloured by GTK.
                    let svg = svg.replacen("<svg ", &format!("<svg fill=\"{accent}\" "), 1);
                    let f = dir.join(format!("m-{m}-{}.svg", accent.trim_start_matches('#')));
                    let _ = std::fs::write(&f, svg);
                    Some(gio::FileIcon::new(&gio::File::for_path(&f)).upcast())
                })
                .collect()
        }
        IconStyle::Normal => {
            // The real file icons, which honour the accent.
            let p = palette();
            let dir = paths::cache_home().join("tuck").join("icons").join("preview");
            let _ = std::fs::create_dir_all(&dir);
            let mut v: Vec<gio::Icon> = Vec::new();
            let f = dir.join(format!("folder-{}.svg", p.accent_bg.hex().trim_start_matches('#')));
            let _ = std::fs::write(&f, icons::folder_svg(Folder::Plain, p.accent_bg));
            v.push(gio::FileIcon::new(&gio::File::for_path(&f)).upcast());
            for n in samples {
                let kind = tuck_core::search::Kind::of(n, false);
                let doc = Doc::for_kind(kind);
                let f = dir.join(format!("doc-{}-{}.svg", doc.id(), icons::badge(n)));
                let _ = std::fs::write(&f, icons::doc_svg(doc, &icons::badge(n)));
                v.push(gio::FileIcon::new(&gio::File::for_path(&f)).upcast());
            }
            v
        }
    }
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

/// The theme to show. For "system", the caller must already have stopped
/// forcing a colour scheme, so the desktop's preference can be read.
fn current_theme(st: &State) -> Theme {
    let sm = adw::StyleManager::default();
    let id = st.settings.theme.as_str();
    if id == "system" {
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
    thread_local! {
        static APPLYING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    // Changing the colour scheme fires the dark/high-contrast signals, which
    // call back in here: skip those nested calls.
    if APPLYING.with(|a| a.replace(true)) {
        return;
    }
    let sm = adw::StyleManager::default();
    // Never touch the style manager while STATE is borrowed: its signal
    // handlers read the settings.
    if settings().theme == "system" {
        // Stop forcing a scheme so the desktop's preference can be read.
        sm.set_color_scheme(adw::ColorScheme::Default);
    }
    let scheme = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let st = s.as_mut()?;
        let theme = current_theme(st);
        let composited = gdk::Display::default().is_some_and(|d| d.is_composited());
        st.palette = theme::resolve(&theme, &st.settings.accent, composited);
        st.provider.load_from_string(&st.palette.css());
        (st.settings.theme != "system").then_some(if theme.dark { adw::ColorScheme::ForceDark } else { adw::ColorScheme::ForceLight })
    });
    if let Some(scheme) = scheme {
        sm.set_color_scheme(scheme);
    }
    APPLYING.with(|a| a.set(false));
    crate::window::Window::restyle_all();
}

fn apply() {
    apply_colors();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(st) = s.as_mut() else { return };
        let gs = gtk::Settings::default().expect("settings");
        gs.set_gtk_xft_dpi((st.base_dpi as f64 * st.settings.text_scale).round() as i32);
        // Tuck's own switch or the desktop's "reduced motion" turns animations
        // off; otherwise GTK follows the desktop's animation setting live.
        if st.settings.reduce_motion || system_reduced_motion(&gs) {
            gs.set_gtk_enable_animations(false);
        } else {
            gs.reset_property("gtk-enable-animations");
        }
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
        v.push("tuck-main");
    }
    if p.high_contrast {
        v.push("tuck-high-contrast");
    }
    v
}

pub const ALL_WINDOW_CLASSES: &[&str] = &["density-compact", "density-comfortable", "density-spacious", "icons-material", "tuck-main", "tuck-high-contrast"];

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
    let system = st.settings.icon_style == IconStyle::System;
    fn find(set: &'static [(&'static str, &'static str)], name: &str) -> Option<&'static str> {
        set.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
    }
    // The view buttons must look different: some themes draw the list
    // icons the same (or link one to the other), so a repeat uses Tabler's.
    let mut view_icons: Vec<Vec<u8>> = Vec::new();
    for (tuck, tabler, mat) in UI_ICONS {
        if system
            && let Some(names) = SYSTEM_UI.iter().find(|(n, _)| n == tuck).map(|(_, v)| *v)
            && let Some((bytes, ext)) = system_icon_file(names)
        {
            let is_view = matches!(*tuck, "grid" | "compact" | "details" | "columns");
            if !(is_view && view_icons.contains(&bytes)) {
                if is_view {
                    view_icons.push(bytes.clone());
                }
                let _ = std::fs::write(actions.join(format!("tuck-{tuck}-symbolic.{ext}")), bytes);
                continue;
            }
        }
        let svg = if material { find(embedded::MATERIAL, mat) } else { find(embedded::TABLER, tabler) };
        if let Some(svg) = svg {
            let _ = std::fs::write(actions.join(format!("tuck-{tuck}-symbolic.svg")), svg);
        }
    }
    for (kind, mat) in MATERIAL_FILES {
        if let Some(svg) = find(embedded::MATERIAL, mat) {
            let _ = std::fs::write(actions.join(format!("tuck-m-file-{kind}-symbolic.svg")), svg);
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

/// A file's icon from the desktop's icon theme, by type; folders get their
/// special icons (Documents, Music…) where the theme has them.
fn system_file_icon(name: &str, is_dir: bool, path: Option<&Path>) -> gio::Icon {
    if is_dir {
        let special = match special_folder(path) {
            Folder::Home => Some("user-home"),
            Folder::Desktop => Some("user-desktop"),
            Folder::Documents => Some("folder-documents"),
            Folder::Downloads => Some("folder-download"),
            Folder::Pictures => Some("folder-pictures"),
            Folder::Music => Some("folder-music"),
            Folder::Videos => Some("folder-videos"),
            Folder::Templates => Some("folder-templates"),
            Folder::Public => Some("folder-publicshare"),
            Folder::Plain => None,
        };
        let names: Vec<&str> = special.into_iter().chain(["folder"]).collect();
        return gio::ThemedIcon::from_names(&names).upcast();
    }
    let (ctype, _) = gio::content_type_guess(Some(name), None);
    gio::content_type_get_icon(&ctype)
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
    let kind = tuck_core::search::Kind::of(name, is_dir);
    if st.icon_style == IconStyle::System {
        return system_file_icon(name, is_dir, path);
    }
    if st.icon_style == IconStyle::Material {
        let k = if is_dir { "folder" } else { Doc::for_kind(kind).id() };
        return gio::ThemedIcon::new(&format!("tuck-m-file-{k}-symbolic")).upcast();
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
    let dir = paths::cache_home().join("tuck").join("icons").join("files");
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
