// SPDX-License-Identifier: GPL-3.0-or-later
//! Preferences: Appearance (see `appearance_ui`), Keyboard and Updates.
//! Every change applies at once.

use adw::prelude::*;
use gtk::{gdk, gio};

use crate::theme_ui;

pub fn show(parent: Option<&gtk::Window>) {
    show_page(parent, None);
}

/// Open Preferences on a given page ("appearance" or "keyboard").
pub fn show_page(parent: Option<&gtk::Window>, page_name: Option<&str>) {
    let dialog = adw::PreferencesDialog::builder().title("Preferences").search_enabled(false).content_width(820).content_height(720).build();
    let page = crate::appearance_ui::page(&dialog);
    dialog.add(&page);

    dialog.add(&keyboard_page());
    dialog.add(&crate::storage_ui::page());
    dialog.add(&crate::update_ui::page());
    if let Some(n) = page_name {
        dialog.set_visible_page_name(n);
    }
    dialog.present(parent);
    // Start at the top, on the current theme.
    if page_name.is_none_or(|n| n == "appearance") {
        crate::appearance_ui::focus_current(&page);
    }
}

/// The key combination as GTK writes it, so equal keys compare equal.
fn normal(accel: &str) -> Option<String> {
    gtk::accelerator_parse(accel).map(|(k, m)| gtk::accelerator_name(k.to_lower(), m).to_string())
}

fn all_actions() -> impl Iterator<Item = &'static (&'static str, &'static [&'static str], &'static str, &'static str)> {
    crate::SHORTCUTS.iter().chain(crate::VIEW_SHORTCUTS)
}

/// Which other action already uses `accel`?
fn conflict(accel: &str, except: &str) -> Option<(&'static str, &'static str)> {
    all_actions()
        .filter(|(a, ..)| *a != except)
        .find(|(a, ..)| crate::accels_for(a).iter().filter_map(|k| normal(k)).any(|k| k == accel))
        .map(|(a, _, _, label)| (*a, *label))
}

fn set_keys(action: &str, keys: Vec<String>) {
    let action = action.to_string();
    theme_ui::update_quiet(move |s| {
        s.shortcuts.insert(action, keys);
    });
    if let Some(app) = gio::Application::default().and_downcast::<gtk::Application>() {
        crate::apply_shortcuts(&app);
    }
}

/// Keyboard: every shortcut, editable. Click a row and press new keys.
fn keyboard_page() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder().title("Keyboard").name("keyboard").icon_name("tuck-compact-symbolic").build();
    let reset_all = adw::PreferencesGroup::builder()
        .description("Click a shortcut, then press the new keys. Backspace removes a shortcut; Escape cancels.")
        .build();
    let reset_btn = gtk::Button::builder().label("Reset All").valign(gtk::Align::Center).css_classes(["flat"]).build();
    reset_all.set_header_suffix(Some(&reset_btn));
    page.add(&reset_all);
    let mut groups: Vec<(&str, adw::PreferencesGroup)> = Vec::new();
    let mut refreshers: Vec<std::rc::Rc<dyn Fn()>> = Vec::new();
    for (action, _, section, label) in all_actions() {
        let group = match groups.iter().find(|(s, _)| s == section) {
            Some((_, g)) => g.clone(),
            None => {
                let g = adw::PreferencesGroup::builder().title(*section).build();
                page.add(&g);
                groups.push((section, g.clone()));
                g
            }
        };
        let row = adw::ActionRow::builder().title(*label).activatable(true).build();
        let keys = gtk::ShortcutLabel::builder().disabled_text("None").valign(gtk::Align::Center).build();
        let reset = gtk::Button::builder().icon_name("tuck-back-symbolic").tooltip_text("Reset to default").css_classes(["flat", "circular"]).valign(gtk::Align::Center).build();
        reset.update_property(&[gtk::accessible::Property::Label(&format!("Reset {label} shortcut"))]);
        row.add_suffix(&keys);
        row.add_suffix(&reset);
        let refresh: std::rc::Rc<dyn Fn()> = {
            let (keys, reset, action) = (keys.clone(), reset.clone(), *action);
            std::rc::Rc::new(move || {
                keys.set_accelerator(&crate::accels_for(action).join(" "));
                reset.set_visible(theme_ui::settings().shortcuts.contains_key(action));
            })
        };
        refresh();
        refreshers.push(refresh.clone());
        let (a, r) = (*action, refresh.clone());
        reset.connect_clicked(move |_| {
            theme_ui::update_quiet(|s| {
                s.shortcuts.remove(a);
            });
            if let Some(app) = gio::Application::default().and_downcast::<gtk::Application>() {
                crate::apply_shortcuts(&app);
            }
            r();
        });
        let (a, l) = (*action, *label);
        row.connect_activated(move |row| capture(row, a, l));
        group.add(&row);
    }
    // A reassignment changes another row too, so every change refreshes all.
    let shared: Refreshers = std::rc::Rc::new(std::cell::RefCell::new(refreshers));
    REFRESH.with(|r| *r.borrow_mut() = Some(shared.clone()));
    reset_btn.connect_clicked(move |_| {
        theme_ui::update_quiet(|s| s.shortcuts.clear());
        if let Some(app) = gio::Application::default().and_downcast::<gtk::Application>() {
            crate::apply_shortcuts(&app);
        }
        for f in shared.borrow().iter() {
            f();
        }
    });
    page
}

type Refreshers = std::rc::Rc<std::cell::RefCell<Vec<std::rc::Rc<dyn Fn()>>>>;

thread_local! {
    static REFRESH: std::cell::RefCell<Option<Refreshers>> = const { std::cell::RefCell::new(None) };
}

fn refresh_all() {
    if let Some(list) = REFRESH.with(|r| r.borrow().clone()) {
        for f in list.borrow().iter() {
            f();
        }
    }
}

/// Wait for a key combination for `action`.
fn capture(row: &adw::ActionRow, action: &'static str, label: &'static str) {
    let shown = gtk::ShortcutLabel::builder().accelerator(crate::accels_for(action).join(" ")).disabled_text("None").halign(gtk::Align::Center).build();
    let warning = gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).css_classes(["warning"]).visible(false).build();
    let b = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).build();
    b.append(&shown);
    b.append(&warning);
    let dialog = adw::AlertDialog::builder()
        .heading(format!("Shortcut for “{label}”"))
        .body("Press the new keys. Backspace removes the shortcut.")
        .extra_child(&b)
        .close_response("cancel")
        .build();
    dialog.add_responses(&[("cancel", "Cancel")]);
    let pending: std::rc::Rc<std::cell::RefCell<Option<String>>> = std::rc::Rc::default();
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let d2 = dialog.clone();
    keys.connect_key_pressed(move |_, key, _, state| {
        let mods = state & gtk::accelerator_get_default_mod_mask();
        if matches!(key, gdk::Key::Shift_L | gdk::Key::Shift_R | gdk::Key::Control_L | gdk::Key::Control_R | gdk::Key::Alt_L | gdk::Key::Alt_R | gdk::Key::Super_L | gdk::Key::Super_R | gdk::Key::Meta_L | gdk::Key::Meta_R | gdk::Key::ISO_Level3_Shift) {
            return gtk::glib::Propagation::Stop;
        }
        if mods.is_empty() && key == gdk::Key::Escape {
            d2.close();
            return gtk::glib::Propagation::Stop;
        }
        if mods.is_empty() && key == gdk::Key::BackSpace {
            set_keys(action, Vec::new());
            refresh_all();
            d2.close();
            return gtk::glib::Propagation::Stop;
        }
        let accel = gtk::accelerator_name(key.to_lower(), mods).to_string();
        shown.set_accelerator(&accel);
        if let Some((_, other_label)) = conflict(&accel, action)
            && pending.borrow().as_deref() != Some(accel.as_str())
        {
            warning.set_label(&format!("Already used by “{other_label}”. Press it again to move it here."));
            warning.set_visible(true);
            *pending.borrow_mut() = Some(accel);
            return gtk::glib::Propagation::Stop;
        }
        if let Some((other, _)) = conflict(&accel, action) {
            let left: Vec<String> = crate::accels_for(other).into_iter().filter(|k| normal(k).as_deref() != Some(accel.as_str())).collect();
            set_keys(other, left);
        }
        set_keys(action, vec![accel]);
        refresh_all();
        d2.close();
        gtk::glib::Propagation::Stop
    });
    dialog.add_controller(keys);
    dialog.present(Some(row));
}
