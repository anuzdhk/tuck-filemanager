// SPDX-License-Identifier: GPL-3.0-or-later
//! Preferences › Appearance: a gallery of theme cards (each a small painted
//! preview of the window in that theme), accent colours, icon styles with
//! sample icons, and readability. The gallery is built from the theme list,
//! so new built-in themes and the user's own theme files appear by
//! themselves.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{cairo, gdk, gio};
use tuck_core::settings::{Density, IconStyle};
use tuck_core::theme::{self, ACCENTS, Palette, Rgb};

use crate::theme_ui;

type Refresh = Rc<RefCell<Vec<Box<dyn Fn()>>>>;

// ------------------------------------------------------------- drawing

fn set(cr: &cairo::Context, c: Rgb, a: f64) {
    cr.set_source_rgba(c.r, c.g, c.b, a);
}

fn rounded(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -std::f64::consts::FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, std::f64::consts::FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
    cr.arc(x + r, y + r, r, std::f64::consts::PI, 1.5 * std::f64::consts::PI);
    cr.close_path();
}

/// A tiny Tuck window in `p`'s colours: header, sidebar with a selected
/// place, and a grid of folders and files.
fn paint_window(cr: &cairo::Context, w: f64, h: f64, p: &Palette) {
    let r = 9.0;
    if p.window_alpha < 0.999 {
        // Translucent: show something behind it.
        let g = cairo::LinearGradient::new(0.0, 0.0, w, h);
        g.add_color_stop_rgb(0.0, 0.93, 0.45, 0.55);
        g.add_color_stop_rgb(0.5, 0.45, 0.42, 0.95);
        g.add_color_stop_rgb(1.0, 0.25, 0.75, 0.85);
        rounded(cr, 0.0, 0.0, w, h, r);
        let _ = cr.set_source(&g);
        let _ = cr.fill();
    }
    cr.save().ok();
    rounded(cr, 0.0, 0.0, w, h, r);
    cr.clip();
    set(cr, p.window_bg, p.window_alpha);
    cr.paint().ok();
    let side = (w * 0.3).round();
    let head = (h * 0.17).round();
    // Sidebar.
    set(cr, p.sidebar_bg, p.layer_alpha(p.sidebar_alpha));
    cr.rectangle(0.0, 0.0, side, h);
    cr.fill().ok();
    for i in 0..5 {
        let y = head + 6.0 + i as f64 * 11.0;
        if i == 1 {
            set(cr, p.accent_bg, 1.0);
            rounded(cr, 4.0, y - 3.0, side - 8.0, 10.0, 4.0);
            cr.fill().ok();
            set(cr, p.accent_fg, 0.9);
        } else {
            set(cr, p.sidebar_fg, 0.45);
        }
        rounded(cr, 9.0, y, side * 0.55, 4.0, 2.0);
        cr.fill().ok();
    }
    // Header bar over the content.
    set(cr, p.headerbar_bg, p.layer_alpha(p.window_alpha));
    cr.rectangle(side, 0.0, w - side, head);
    cr.fill().ok();
    set(cr, p.headerbar_fg, 0.5);
    rounded(cr, side + 8.0, head / 2.0 - 2.0, 22.0, 4.0, 2.0);
    cr.fill().ok();
    // Content.
    set(cr, p.view_bg, p.layer_alpha(p.view_alpha));
    cr.rectangle(side, head, w - side, h - head);
    cr.fill().ok();
    let cols = 3;
    let cw = (w - side - 10.0) / cols as f64;
    for row in 0..2 {
        for col in 0..cols {
            let x = side + 5.0 + col as f64 * cw + cw / 2.0 - 10.0;
            let y = head + 9.0 + row as f64 * 30.0;
            if row == 0 {
                // Folder: tab and body in the accent.
                set(cr, p.accent_bg, 0.55);
                rounded(cr, x, y, 9.0, 5.0, 2.0);
                cr.fill().ok();
                set(cr, p.accent_bg, 0.9);
                rounded(cr, x, y + 3.0, 20.0, 13.0, 2.5);
                cr.fill().ok();
            } else {
                // Document.
                set(cr, p.view_fg, 0.14);
                rounded(cr, x + 3.0, y, 14.0, 17.0, 2.0);
                cr.fill().ok();
            }
            set(cr, p.view_fg, 0.5);
            rounded(cr, x - 1.0, y + 19.0, 22.0, 3.0, 1.5);
            cr.fill().ok();
        }
    }
    cr.restore().ok();
    // Outline.
    rounded(cr, 0.5, 0.5, w - 1.0, h - 1.0, r);
    set(cr, p.border, 0.9);
    cr.set_line_width(1.0);
    cr.stroke().ok();
}

fn palette_of(id: &str) -> Option<Palette> {
    let accent = theme_ui::settings().accent;
    let t = theme_ui::theme_for(id)?;
    Some(theme::resolve(&t, &accent, true))
}

/// The preview for a theme card. "Follow System" shows light and dark
/// side by side.
fn preview(id: &str) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder().content_width(112).content_height(70).halign(gtk::Align::Center).css_classes(["theme-preview"]).build();
    let id = id.to_string();
    area.set_draw_func(move |_, cr, w, h| {
        // Drawn at the 150×94 design size, scaled to fit.
        cr.scale(w as f64 / 150.0, h as f64 / 94.0);
        let (w, h) = (150.0, 94.0);
        if id == "system" {
            let (Some(light), Some(dark)) = (palette_of("light"), palette_of("dark")) else { return };
            cr.save().ok();
            cr.move_to(0.0, 0.0);
            cr.line_to(w * 0.62, 0.0);
            cr.line_to(w * 0.38, h);
            cr.line_to(0.0, h);
            cr.close_path();
            cr.clip();
            paint_window(cr, w, h, &light);
            cr.restore().ok();
            cr.save().ok();
            cr.move_to(w * 0.62, 0.0);
            cr.line_to(w, 0.0);
            cr.line_to(w, h);
            cr.line_to(w * 0.38, h);
            cr.close_path();
            cr.clip();
            paint_window(cr, w, h, &dark);
            cr.restore().ok();
        } else if let Some(p) = palette_of(&id) {
            paint_window(cr, w, h, &p);
        }
    });
    area
}

fn card(child: &impl IsA<gtk::Widget>, title: &str, subtitle: &str) -> gtk::ToggleButton {
    let b = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
    b.append(child);
    // Capped widths, so long names don't force fewer cards per row.
    b.append(&gtk::Label::builder().label(title).css_classes(["card-title"]).ellipsize(gtk::pango::EllipsizeMode::End).max_width_chars(14).build());
    b.append(
        &gtk::Label::builder()
            .label(subtitle)
            .css_classes(["caption", "dim-label"])
            .wrap(true)
            .lines(2)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .justify(gtk::Justification::Center)
            .max_width_chars(16)
            .build(),
    );
    let t = gtk::ToggleButton::builder().child(&b).css_classes(["pick-card"]).build();
    t.update_property(&[gtk::accessible::Property::Label(&format!("{title}, {subtitle}"))]);
    t
}

fn grid() -> gtk::FlowBox {
    gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(true)
        .min_children_per_line(2)
        .max_children_per_line(4)
        .column_spacing(10)
        .row_spacing(10)
        .build()
}

// --------------------------------------------------------------- page

/// Fill `flow` with a card per theme.
fn fill_themes(flow: &gtk::FlowBox) {
    flow.remove_all();
    let current = theme_ui::settings().theme;
    let mut first: Option<gtk::ToggleButton> = None;
    for (id, name) in theme_ui::theme_choices() {
        let subtitle = if id == "system" {
            "Follows your desktop".to_string()
        } else if theme_ui::is_user_theme(&id) {
            "Your theme".to_string()
        } else {
            match theme_ui::theme_for(&id) {
                Some(t) if t.translucent => "See-through".into(),
                Some(t) if t.high_contrast => "High contrast".into(),
                Some(t) if t.dark => "Dark".into(),
                _ => "Light".into(),
            }
        };
        let subtitle = if subtitle == name { format!("Standard {}", subtitle.to_lowercase()) } else { subtitle };
        let c = card(&preview(&id), &name, &subtitle);
        c.set_tooltip_text(Some(&name));
        match &first {
            Some(f) => c.set_group(Some(f)),
            None => first = Some(c.clone()),
        }
        c.set_active(id == current);
        let idc = id.clone();
        c.connect_toggled(move |b| {
            if b.is_active() && theme_ui::settings().theme != idc {
                let id = idc.clone();
                theme_ui::update(|s| s.theme = id);
            }
        });
        flow.append(&c);
    }
}

/// Focus the first setting (Text Size), so the page opens at the top
/// rather than wherever GTK puts focus.
pub fn focus_current(page: &adw::PreferencesPage) {
    fn find(w: &gtk::Widget) -> Option<adw::SpinRow> {
        if let Some(t) = w.downcast_ref::<adw::SpinRow>() {
            return Some(t.clone());
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            if let Some(t) = find(&ch) {
                return Some(t);
            }
            c = ch.next_sibling();
        }
        None
    }
    if let Some(t) = find(page.upcast_ref()) {
        t.grab_focus();
    }
    page.scroll_to_top();
}

pub fn page(dialog: &adw::PreferencesDialog) -> adw::PreferencesPage {
    let s = theme_ui::settings();
    let page = adw::PreferencesPage::builder().title("Appearance").name("appearance").icon_name("tuck-preferences-symbolic").build();
    let refresh: Refresh = Rc::default();

    // ---------------------------------------------------- theme gallery
    let themes = adw::PreferencesGroup::builder()
        .title("Theme")
        .description("Colours are adjusted automatically so text always stays readable.")
        .build();
    let theme_flow = grid();
    theme_flow.set_max_children_per_line(5);
    fill_themes(&theme_flow);
    themes.add(&theme_flow);
    {
        let f = theme_flow.clone();
        refresh.borrow_mut().push(Box::new(move || {
            let mut c = f.first_child();
            while let Some(ch) = c {
                // FlowBoxChild › ToggleButton › Box › DrawingArea
                if let Some(a) = ch.first_child().and_then(|b| b.first_child()).and_then(|b| b.first_child()) {
                    a.queue_draw();
                }
                c = ch.next_sibling();
            }
        }));
    }

    // ----------------------------------------------------------- accent
    let accent = adw::PreferencesGroup::builder().title("Accent Colour").build();
    let swatches = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .min_children_per_line(5)
        .max_children_per_line(12)
        .column_spacing(4)
        .row_spacing(4)
        .build();
    let mut group: Option<gtk::ToggleButton> = None;
    let theme_accent = theme_ui::palette().accent_bg;
    for (id, label, hex) in ACCENTS {
        let colour = if hex.is_empty() { theme_accent.hex() } else { hex.to_string() };
        let b = gtk::ToggleButton::builder()
            .tooltip_text(*label)
            .css_classes(["accent-swatch", "circular"])
            .width_request(32)
            .height_request(32)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        b.update_property(&[gtk::accessible::Property::Label(&format!("{label} accent"))]);
        let css = gtk::CssProvider::new();
        css.load_from_string(&format!(
            "button.accent-swatch {{ background: {colour}; min-width: 32px; min-height: 32px; padding: 0; }} button.accent-swatch:checked {{ box-shadow: 0 0 0 3px var(--window-bg-color), 0 0 0 5px {colour}; }}"
        ));
        #[allow(deprecated)]
        b.style_context().add_provider(&css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 2);
        if hex.is_empty() {
            b.set_tooltip_text(Some("Theme default"));
            b.update_property(&[gtk::accessible::Property::Label("Theme default accent")]);
            b.set_child(Some(&gtk::Label::builder().label("A").css_classes(["accent-default-mark"]).build()));
        }
        if *id == s.accent {
            b.set_active(true);
        }
        match &group {
            Some(g) => b.set_group(Some(g)),
            None => group = Some(b.clone()),
        }
        let (id, r) = (id.to_string(), refresh.clone());
        b.connect_toggled(move |b| {
            if b.is_active() {
                let id = id.clone();
                theme_ui::update(|s| s.accent = id);
                for f in r.borrow().iter() {
                    f();
                }
            }
        });
        swatches.append(&b);
    }
    let custom = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::builder().with_alpha(false).title("Custom Accent").build()));
    custom.set_valign(gtk::Align::Center);
    custom.set_tooltip_text(Some("Any colour"));
    custom.update_property(&[gtk::accessible::Property::Label("Custom accent colour")]);
    if let Some(rgb) = Rgb::parse(&s.accent).filter(|_| s.accent.starts_with('#')) {
        custom.set_rgba(&gdk::RGBA::new(rgb.r as f32, rgb.g as f32, rgb.b as f32, 1.0));
    }
    let r = refresh.clone();
    custom.connect_rgba_notify(move |b| {
        let c = b.rgba();
        let hex = Rgb { r: c.red() as f64, g: c.green() as f64, b: c.blue() as f64 }.hex();
        theme_ui::update(|s| s.accent = hex);
        for f in r.borrow().iter() {
            f();
        }
    });
    swatches.append(&custom);
    accent.add(&swatches);

    // ------------------------------------------------------------ icons
    let icons = adw::PreferencesGroup::builder()
        .title("Icons")
        .description("Used for files, folders and the toolbar.")
        .build();
    let icon_flow = grid();
    icon_flow.set_max_children_per_line(3);
    let styles = [
        (IconStyle::Normal, "Tuck", "Tuck's folders, Tabler icons".to_string()),
        (IconStyle::Material, "Material", "Material Symbols in the accent".to_string()),
        (IconStyle::System, "System", format!("Your desktop's {}", theme_ui::system_icon_theme_name())),
    ];
    let mut first: Option<gtk::ToggleButton> = None;
    let mut samples: Vec<(IconStyle, gtk::Box)> = Vec::new();
    for (style, title, subtitle) in styles {
        let row = gtk::Box::builder().spacing(8).halign(gtk::Align::Center).height_request(56).css_classes(["icon-sample"]).build();
        for i in theme_ui::preview_icons(style) {
            row.append(&gtk::Image::builder().gicon(&i).pixel_size(32).build());
        }
        samples.push((style, row.clone()));
        let c = card(&row, title, &subtitle);
        match &first {
            Some(f) => c.set_group(Some(f)),
            None => first = Some(c.clone()),
        }
        c.set_active(s.icon_style == style);
        c.connect_toggled(move |b| {
            if b.is_active() && theme_ui::settings().icon_style != style {
                theme_ui::update(|s| s.icon_style = style);
            }
        });
        icon_flow.append(&c);
    }
    // The Tuck and Material samples follow the accent.
    refresh.borrow_mut().push(Box::new(move || {
        for (style, row) in &samples {
            if *style == IconStyle::System {
                continue;
            }
            while let Some(c) = row.first_child() {
                row.remove(&c);
            }
            for i in theme_ui::preview_icons(*style) {
                row.append(&gtk::Image::builder().gicon(&i).pixel_size(32).build());
            }
        }
    }));
    icons.add(&icon_flow);

    // ------------------------------------------------------ readability
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
        .subtitle("Turn off animations, including animated GIF previews. Also off when your desktop asks for reduced motion")
        .active(s.reduce_motion)
        .build();
    motion.connect_active_notify(|r| {
        let on = r.is_active();
        theme_ui::update(|s| s.reduce_motion = on);
    });
    access.add(&motion);

    // ------------------------------------------------------ user themes
    let custom_group = adw::PreferencesGroup::builder()
        .title("Your Themes")
        .description("Make your own: put a .toml file in the themes folder with a name, dark = true or false, and window_bg / window_fg colours. Everything else is optional and is adjusted for readability. It appears in the gallery above.")
        .build();
    let open = adw::ActionRow::builder().title("Open Themes Folder").subtitle("Includes an example to copy").activatable(true).build();
    open.add_suffix(&gtk::Image::from_icon_name("tuck-open-folder-symbolic"));
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
    reload.add_suffix(&gtk::Image::from_icon_name("tuck-recent-symbolic"));
    let errors_box = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
    let show_errors = {
        let b = errors_box.clone();
        move || {
            while let Some(c) = b.first_child() {
                b.remove(&c);
            }
            for e in theme_ui::user_theme_errors() {
                let l = gtk::Label::builder().label(format!("A theme couldn't be loaded: {e}")).wrap(true).xalign(0.0).css_classes(["error", "caption"]).build();
                b.append(&l);
            }
        }
    };
    show_errors();
    let (d2, f2) = (dialog.clone(), theme_flow.clone());
    reload.connect_activated(move |_| {
        theme_ui::reload_user_themes();
        fill_themes(&f2);
        show_errors();
        let n = theme_ui::user_theme_errors().len();
        let msg = if n == 0 { "Themes reloaded".to_string() } else { format!("{n} theme file(s) have problems; see below") };
        d2.add_toast(adw::Toast::builder().title(msg).timeout(4).build());
    });
    custom_group.add(&reload);
    custom_group.add(&errors_box);
    page.add(&themes);
    page.add(&accent);
    page.add(&icons);
    page.add(&custom_group);
    page
}
