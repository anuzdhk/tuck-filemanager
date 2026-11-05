// SPDX-License-Identifier: GPL-3.0-or-later
//! Preferences: appearance and accessibility. Every change applies at once.

use adw::prelude::*;
use gtk::{gdk, gio};
use kivo_core::settings::{Density, IconStyle};
use kivo_core::theme::{ACCENTS, Rgb};

use crate::theme_ui;

pub fn show(parent: Option<&gtk::Window>) {
    let s = theme_ui::settings();
    let dialog = adw::PreferencesDialog::builder().title("Preferences").search_enabled(false).build();
    let page = adw::PreferencesPage::builder().title("Appearance").icon_name("kivo-preferences-symbolic").build();
    dialog.add(&page);

    // --------------------------------------------------------- theme
    let look = adw::PreferencesGroup::builder().title("Look").build();
    page.add(&look);
    let choices = theme_ui::theme_choices();
    let names: Vec<&str> = choices.iter().map(|(_, n)| n.as_str()).collect();
    let theme_row = adw::ComboRow::builder()
        .title("Theme")
        .subtitle("Translucent needs a compositor; without one it falls back to solid colours")
        .model(&gtk::StringList::new(&names))
        .build();
    if let Some(i) = choices.iter().position(|(id, _)| *id == s.theme) {
        theme_row.set_selected(i as u32);
    }
    theme_row.connect_selected_notify(move |r| {
        if let Some((id, _)) = choices.get(r.selected() as usize) {
            let id = id.clone();
            theme_ui::update(|s| s.theme = id);
        }
    });
    look.add(&theme_row);

    // -------------------------------------------------------- accent
    let accent_row = adw::ActionRow::builder()
        .title("Accent Colour")
        .subtitle("Text and icons are adjusted automatically so they stay readable")
        .build();
    let swatches = gtk::Box::builder().spacing(6).valign(gtk::Align::Center).build();
    let mut group: Option<gtk::ToggleButton> = None;
    let theme_accent = theme_ui::palette().accent_bg;
    for (id, label, hex) in ACCENTS {
        let colour = if hex.is_empty() { theme_accent.hex() } else { hex.to_string() };
        let b = gtk::ToggleButton::builder()
            .tooltip_text(*label)
            .css_classes(["accent-swatch", "circular"])
            .width_request(26)
            .height_request(26)
            .valign(gtk::Align::Center)
            .build();
        b.update_property(&[gtk::accessible::Property::Label(&format!("{label} accent"))]);
        let css = gtk::CssProvider::new();
        css.load_from_string(&format!("button.accent-swatch {{ background: {colour}; min-width: 26px; min-height: 26px; padding: 0; }} button.accent-swatch:checked {{ box-shadow: 0 0 0 2px var(--window-bg-color), 0 0 0 4px {colour}; }}"));
        #[allow(deprecated)]
        b.style_context().add_provider(&css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 2);
        if hex.is_empty() {
            b.set_icon_name("kivo-close-symbolic");
            b.set_tooltip_text(Some("Theme default"));
            b.update_property(&[gtk::accessible::Property::Label("Theme default accent")]);
            b.set_child(None::<&gtk::Widget>);
        }
        if *id == s.accent {
            b.set_active(true);
        }
        if let Some(g) = &group {
            b.set_group(Some(g));
        } else {
            group = Some(b.clone());
        }
        let id = id.to_string();
        b.connect_toggled(move |b| {
            if b.is_active() {
                let id = id.clone();
                theme_ui::update(|s| s.accent = id);
            }
        });
        swatches.append(&b);
    }
    // Any colour at all.
    let custom = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::builder().with_alpha(false).title("Custom Accent").build()));
    custom.set_valign(gtk::Align::Center);
    custom.set_tooltip_text(Some("Custom colour"));
    custom.update_property(&[gtk::accessible::Property::Label("Custom accent colour")]);
    if let Some(rgb) = Rgb::parse(&s.accent).filter(|_| s.accent.starts_with('#')) {
        custom.set_rgba(&gdk::RGBA::new(rgb.r as f32, rgb.g as f32, rgb.b as f32, 1.0));
    }
    custom.connect_rgba_notify(|b| {
        let c = b.rgba();
        let hex = Rgb { r: c.red() as f64, g: c.green() as f64, b: c.blue() as f64 }.hex();
        theme_ui::update(|s| s.accent = hex);
    });
    swatches.append(&custom);
    accent_row.add_suffix(&swatches);
    look.add(&accent_row);

    // --------------------------------------------------------- icons
    let icons = adw::ComboRow::builder()
        .title("Icon Theme")
        .subtitle("Normal: Tabler icons with Kivo's folders. Material: Material Symbols in the accent colour")
        .model(&gtk::StringList::new(&["Normal", "Material"]))
        .selected(if s.icon_style == IconStyle::Material { 1 } else { 0 })
        .build();
    icons.connect_selected_notify(|r| {
        let style = if r.selected() == 1 { IconStyle::Material } else { IconStyle::Normal };
        theme_ui::update(|s| s.icon_style = style);
    });
    look.add(&icons);

    // ------------------------------------------------- accessibility
    let access = adw::PreferencesGroup::builder().title("Readability").build();
    page.add(&access);
    let text = adw::SpinRow::builder()
        .title("Text Size")
        .subtitle("Percent of the normal size")
        .adjustment(&gtk::Adjustment::new((s.text_scale * 100.0).round(), 80.0, 160.0, 10.0, 10.0, 0.0))
        .build();
    text.connect_value_notify(|r| {
        let v = r.value() / 100.0;
        theme_ui::update(|s| s.text_scale = v);
    });
    access.add(&text);
    let density = adw::ComboRow::builder()
        .title("Spacing")
        .model(&gtk::StringList::new(&["Compact", "Comfortable", "Spacious"]))
        .selected(match s.density {
            Density::Compact => 0,
            Density::Comfortable => 1,
            Density::Spacious => 2,
        })
        .build();
    density.connect_selected_notify(|r| {
        let d = match r.selected() {
            0 => Density::Compact,
            2 => Density::Spacious,
            _ => Density::Comfortable,
        };
        theme_ui::update(|s| s.density = d);
    });
    access.add(&density);
    let motion = adw::SwitchRow::builder()
        .title("Reduce Motion")
        .subtitle("Turn off animations, including animated GIF previews")
        .active(s.reduce_motion)
        .build();
    motion.connect_active_notify(|r| {
        let on = r.is_active();
        theme_ui::update(|s| s.reduce_motion = on);
    });
    access.add(&motion);

    // -------------------------------------------------- custom themes
    let custom_group = adw::PreferencesGroup::builder()
        .title("Custom Themes")
        .description("Add a theme by putting a .toml file in the themes folder. A theme needs a name, dark = true or false, and window_bg / window_fg colours; everything else is optional and is adjusted for readability.")
        .build();
    let open = adw::ActionRow::builder().title("Open Themes Folder").activatable(true).build();
    open.add_suffix(&gtk::Image::from_icon_name("kivo-open-folder-symbolic"));
    open.connect_activated(|row| {
        let dir = theme_ui::themes_dir();
        let _ = std::fs::create_dir_all(&dir);
        let example = dir.join("example.toml.txt");
        if !example.exists() {
            let _ = std::fs::write(
                &example,
                "# Copy this to a .toml file and edit it.\nname = \"Solarized Dark\"\ndark = true\n# translucent = false\n\n[colors]\nwindow_bg = \"#002b36\"\nwindow_fg = \"#93a1a1\"\nview_bg = \"#00252f\"\nsidebar_bg = \"#073642\"\ndim_fg = \"#839496\"\naccent = \"#268bd2\"\ndestructive = \"#dc322f\"\nsuccess = \"#859900\"\nwarning = \"#b58900\"\n",
            );
        }
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&dir)));
        launcher.launch(row.root().and_downcast_ref::<gtk::Window>(), gio::Cancellable::NONE, |_| {});
    });
    custom_group.add(&open);
    let reload = adw::ActionRow::builder().title("Reload Themes").subtitle("After adding or editing a theme file").activatable(true).build();
    let d2 = dialog.clone();
    reload.connect_activated(move |_| {
        theme_ui::reload_user_themes();
        let errors = theme_ui::user_theme_errors();
        let msg = if errors.is_empty() { "Themes reloaded. Reopen Preferences to see new ones.".to_string() } else { errors.join("\n") };
        d2.add_toast(adw::Toast::builder().title(msg).timeout(6).build());
    });
    custom_group.add(&reload);
    for e in theme_ui::user_theme_errors() {
        custom_group.add(&adw::ActionRow::builder().title("A theme couldn't be loaded").subtitle(gtk::glib::markup_escape_text(&e)).build());
    }
    page.add(&custom_group);

    dialog.present(parent);
}
