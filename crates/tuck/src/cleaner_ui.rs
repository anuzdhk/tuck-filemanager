// SPDX-License-Identifier: GPL-3.0-or-later
//! The Cleaner window: measure caches in the background, let the user pick
//! (safe caches are ticked; other apps' caches and the Trash aren't), confirm,
//! clean, and say how much space came back.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use tuck_core::cleaner::{self, Found, Kind};
use tuck_core::format;
use tuck_core::ops::Cancel;
use tuck_core::trash::Trash;

type Chosen = Rc<RefCell<Vec<(Found, gtk::CheckButton)>>>;

pub fn show(parent: &impl IsA<gtk::Widget>) {
    let dialog = adw::Dialog::builder().title("Cleaner").content_width(600).content_height(640).build();
    let stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).vexpand(true).build();

    // 1. Measuring.
    let busy = adw::StatusPage::builder()
        .title("Looking for Caches…")
        .description("Measuring what can be cleaned. Nothing is deleted yet.")
        .child(&adw::Spinner::builder().width_request(32).height_request(32).build())
        .build();
    stack.add_named(&busy, Some("busy"));

    // 3. Done.
    let done = adw::StatusPage::builder().icon_name("tuck-cleaner-symbolic").build();
    let close = gtk::Button::builder().label("Done").halign(gtk::Align::Center).css_classes(["pill", "suggested-action"]).build();
    let d2 = dialog.clone();
    close.connect_clicked(move |_| {
        d2.close();
    });
    done.set_child(Some(&close));
    stack.add_named(&done, Some("done"));

    let header = adw::HeaderBar::new();
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    tv.set_content(Some(&stack));
    // Bottom: selection summary and the Clean button.
    let summary = gtk::Label::builder().xalign(0.0).hexpand(true).css_classes(["heading"]).build();
    let clean = gtk::Button::builder().label("Clean").css_classes(["destructive-action", "pill"]).sensitive(false).build();
    let bar = gtk::Box::builder().spacing(12).margin_start(18).margin_end(18).margin_top(10).margin_bottom(10).build();
    bar.append(&summary);
    bar.append(&clean);
    let bottom = gtk::Revealer::builder().child(&bar).reveal_child(false).build();
    tv.add_bottom_bar(&bottom);
    dialog.set_child(Some(&tv));
    dialog.present(Some(parent));

    let cancel = Cancel::new();
    let c2 = cancel.clone();
    dialog.connect_closed(move |_| c2.cancel());

    let chosen: Chosen = Rc::default();
    let (stack2, summary2, clean2, bottom2, chosen2) = (stack.clone(), summary.clone(), clean.clone(), bottom.clone(), chosen.clone());
    glib::MainContext::default().spawn_local(async move {
        let c = cancel.clone();
        let found = gio::spawn_blocking(move || {
            let home = tuck_core::paths::home();
            let cache = tuck_core::paths::cache_home();
            let trash = Trash::for_user();
            cleaner::scan(cleaner::catalogue(&home, &cache, Some(&trash)), std::path::Path::new("/proc"), &c)
        })
        .await
        .unwrap_or_default();
        if cancel.is_cancelled() {
            return;
        }
        if found.is_empty() {
            done.set_title("Already Clean");
            done.set_description(Some("There are no caches to clean right now."));
            stack2.set_visible_child_name("done");
            return;
        }
        let page = list_page(&found, &chosen2, &summary2, &clean2);
        stack2.add_named(&page, Some("list"));
        stack2.set_visible_child_name("list");
        bottom2.set_reveal_child(true);
        update_summary(&chosen2, &summary2, &clean2);

        clean2.connect_clicked(move |b| {
            let picked: Vec<Found> = chosen2.borrow().iter().filter(|(_, c)| c.is_active()).map(|(f, _)| f.clone()).collect();
            if picked.is_empty() {
                return;
            }
            let bytes: u64 = picked.iter().map(|f| f.bytes).sum();
            let with_trash = picked.iter().any(|f| f.item.kind == Kind::Trash);
            let body = if with_trash {
                format!("{} will be freed. Files in the Trash are deleted for good; this can't be undone.", format::size(bytes))
            } else {
                format!("{} will be freed. Apps make their caches again when they need them.", format::size(bytes))
            };
            let confirm = adw::AlertDialog::builder()
                .heading(format!("Clean {}?", format::size(bytes)))
                .body(body)
                .default_response("cancel")
                .close_response("cancel")
                .build();
            confirm.add_responses(&[("cancel", "Cancel"), ("clean", "Clean")]);
            confirm.set_response_appearance("clean", adw::ResponseAppearance::Destructive);
            let (stack, done, bottom, busy_page) = (stack2.clone(), done.clone(), bottom2.clone(), busy.clone());
            confirm.connect_response(None, move |_, r| {
                if r != "clean" {
                    return;
                }
                bottom.set_reveal_child(false);
                busy_page.set_title("Cleaning…");
                busy_page.set_description(Some("Deleting cache files."));
                stack.set_visible_child_name("busy");
                let (stack, done, picked) = (stack.clone(), done.clone(), picked.clone());
                glib::MainContext::default().spawn_local(async move {
                    let report = gio::spawn_blocking(move || {
                        let trash = Trash::for_user();
                        cleaner::clean(&picked, Some(&trash), &Cancel::new())
                    })
                    .await
                    .unwrap_or_default();
                    done.set_title(&format!("Freed {}", format::size(report.freed)));
                    let mut desc = format!("{} files removed.", report.files);
                    if !report.errors.is_empty() {
                        desc.push_str(&format!(" {} couldn't be removed (they may be in use).", report.errors.len()));
                    }
                    done.set_description(Some(&desc));
                    stack.set_visible_child_name("done");
                });
            });
            confirm.present(Some(b));
        });
    });
}

fn update_summary(chosen: &Chosen, summary: &gtk::Label, clean: &gtk::Button) {
    let (n, bytes) = chosen.borrow().iter().filter(|(_, c)| c.is_active()).fold((0, 0u64), |(n, b), (f, _)| (n + 1, b + f.bytes));
    summary.set_label(&if n == 0 { "Nothing selected".to_string() } else { format!("{} selected", format::size(bytes)) });
    clean.set_sensitive(n > 0);
}

fn list_page(found: &[Found], chosen: &Chosen, summary: &gtk::Label, clean: &gtk::Button) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    let safe = adw::PreferencesGroup::builder()
        .title("Safe to Clean")
        .description("Caches your apps make again by themselves. Nothing you made or saved is in here.")
        .build();
    let others = adw::PreferencesGroup::builder()
        .title("Other Apps")
        .description("Usually safe too. An app may be slower once, or ask you to sign in again.")
        .build();
    let trash = adw::PreferencesGroup::builder().title("Trash").build();
    let total: u64 = found.iter().map(|f| f.bytes).sum();
    let headline = gtk::Label::builder()
        .label(format!("{} can be cleaned", format::size(total)))
        .css_classes(["title-1"])
        .margin_bottom(4)
        .build();
    let intro = adw::PreferencesGroup::new();
    intro.add(&headline);
    page.add(&intro);
    for f in found {
        let check = gtk::CheckButton::builder().active(f.item.default_on && f.blocked_by.is_none()).valign(gtk::Align::Center).build();
        let subtitle = match &f.blocked_by {
            Some(why) => format!("{why} · {}", f.item.detail),
            None => f.item.detail.clone(),
        };
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&f.item.name))
            .subtitle(glib::markup_escape_text(&subtitle))
            .subtitle_lines(2)
            .activatable_widget(&check)
            .build();
        row.add_prefix(&check);
        row.add_suffix(&gtk::Label::builder().label(format::size(f.bytes)).css_classes(["numeric", "dim-label"]).build());
        if f.blocked_by.is_some() {
            row.set_sensitive(false);
        }
        let (chosen2, summary, clean) = (chosen.clone(), summary.clone(), clean.clone());
        check.connect_toggled(move |_| update_summary(&chosen2, &summary, &clean));
        chosen.borrow_mut().push((f.clone(), check));
        match f.item.kind {
            Kind::Trash => trash.add(&row),
            Kind::Cache if f.item.id.starts_with("app:") => others.add(&row),
            Kind::Cache => safe.add(&row),
        }
    }
    // Only groups that have rows.
    page.add(&safe);
    if found.iter().any(|f| f.item.id.starts_with("app:")) {
        page.add(&others);
    }
    if found.iter().any(|f| f.item.kind == Kind::Trash) {
        page.add(&trash);
    }
    if !found.iter().any(|f| f.item.kind == Kind::Cache && !f.item.id.starts_with("app:")) {
        safe.set_visible(false);
    }
    page
}
