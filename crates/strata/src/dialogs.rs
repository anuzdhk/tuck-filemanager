// SPDX-License-Identifier: GPL-3.0-or-later
//! Dialogs for file operations: conflicts, rename, batch rename, new folder,
//! confirm delete, and error reports. All text is plain language.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use strata_core::names::{self, BatchRule};
use strata_core::ops::{ConflictInfo, OpError, Resolution};
use strata_core::format;

fn describe_item(path: &Path) -> String {
    match std::fs::symlink_metadata(path) {
        Ok(m) => {
            let when = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| format::date_relative(d.as_secs() as i64, chrono::Local::now()))
                .unwrap_or_default();
            if m.is_dir() {
                format!("Folder · modified {when}")
            } else {
                format!("{} · modified {when}", format::size(m.len()))
            }
        }
        Err(_) => "Unavailable".into(),
    }
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Ask what to do when `info.dest` already exists.
pub async fn ask_conflict(parent: &gtk::Widget, info: &ConflictInfo) -> (Resolution, bool) {
    let both_dirs = info.source_is_dir && info.dest_is_dir;
    let name = name_of(&info.dest);
    let folder = info.dest.parent().map(name_of).unwrap_or_default();
    let heading = if both_dirs { format!("Merge Folder “{name}”?") } else { format!("Replace “{name}”?") };
    let body = if both_dirs {
        format!("A folder with this name already exists in “{folder}”. Merging puts the contents of both into one folder; you'll be asked about any files with the same name.")
    } else {
        format!("An item with this name already exists in “{folder}”.")
    };
    let dialog = adw::AlertDialog::new(Some(&heading), Some(&body));

    let grid = gtk::Grid::builder().row_spacing(6).column_spacing(12).build();
    for (row, (label, path)) in [("Existing", &info.dest), ("Incoming", &info.source)].into_iter().enumerate() {
        let k = gtk::Label::builder().label(label).xalign(0.0).css_classes(["dim-label"]).build();
        let v = gtk::Label::builder().label(describe_item(path)).xalign(0.0).wrap(true).build();
        grid.attach(&k, 0, row as i32, 1, 1);
        grid.attach(&v, 1, row as i32, 1, 1);
    }
    let all = gtk::CheckButton::with_mnemonic("_Apply to all remaining conflicts");
    let extra = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).build();
    extra.append(&grid);
    extra.append(&all);
    dialog.set_extra_child(Some(&extra));

    dialog.add_responses(&[("cancel", "_Cancel"), ("skip", "_Skip"), ("keep", "_Keep Both")]);
    if both_dirs {
        dialog.add_response("replace", "_Merge");
        dialog.set_response_appearance("replace", adw::ResponseAppearance::Suggested);
    } else {
        dialog.add_response("replace", "_Replace");
        dialog.set_response_appearance("replace", adw::ResponseAppearance::Destructive);
    }
    dialog.set_default_response(Some("keep"));
    dialog.set_close_response("cancel");
    let resp = dialog.choose_future(Some(parent)).await;
    let r = match resp.as_str() {
        "skip" => Resolution::Skip,
        "keep" => Resolution::KeepBoth,
        "replace" if both_dirs => Resolution::Merge,
        "replace" => Resolution::Replace,
        _ => Resolution::Cancel,
    };
    (r, all.is_active())
}

/// Show errors from an operation. `title` like "Some Items Couldn't Be Copied".
pub fn show_errors(parent: &gtk::Widget, title: &str, errors: &[OpError]) {
    if errors.is_empty() {
        return;
    }
    let body = if errors.len() == 1 {
        format!("{}\n\n{}", errors[0].message, errors[0].hint)
    } else {
        let mut s = String::new();
        for e in errors.iter().take(6) {
            s.push_str(&format!("• {}\n", e.message));
        }
        if errors.len() > 6 {
            s.push_str(&format!("…and {} more.\n", errors.len() - 6));
        }
        s.push('\n');
        s.push_str(&errors[0].hint);
        s
    };
    let d = adw::AlertDialog::new(Some(title), Some(&body));
    d.add_response("ok", "_OK");
    d.present(Some(parent));
}

pub async fn confirm_delete(parent: &gtk::Widget, names: &[String]) -> bool {
    let heading = match names {
        [one] => format!("Permanently Delete “{one}”?"),
        many => format!("Permanently Delete {} Items?", many.len()),
    };
    let d = adw::AlertDialog::new(
        Some(&heading),
        Some("This can't be undone. The items won't go to the Trash."),
    );
    d.add_responses(&[("cancel", "_Cancel"), ("delete", "_Delete Permanently")]);
    d.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    d.set_default_response(Some("cancel"));
    d.set_close_response("cancel");
    d.choose_future(Some(parent)).await == "delete"
}

pub async fn confirm(parent: &gtk::Widget, heading: &str, body: &str, action: &str) -> bool {
    let d = adw::AlertDialog::new(Some(heading), Some(body));
    d.add_responses(&[("cancel", "_Cancel"), ("ok", action)]);
    d.set_response_appearance("ok", adw::ResponseAppearance::Destructive);
    d.set_default_response(Some("cancel"));
    d.set_close_response("cancel");
    d.choose_future(Some(parent)).await == "ok"
}

/// Ask for a name. `select_stem` selects the part before the extension.
/// `validate` returns an error to show, or None.
pub async fn ask_name(
    parent: &gtk::Widget,
    heading: &str,
    action: &str,
    initial: &str,
    is_dir: bool,
    allow_unchanged: bool,
    validate: impl Fn(&str) -> Option<String> + 'static,
) -> Option<String> {
    let d = adw::AlertDialog::new(Some(heading), None);
    let entry = gtk::Entry::builder().text(initial).activates_default(true).build();
    entry.update_property(&[gtk::accessible::Property::Label("Name")]);
    let note = gtk::Label::builder().xalign(0.0).wrap(true).css_classes(["caption"]).build();
    let b = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).build();
    b.append(&entry);
    b.append(&note);
    d.set_extra_child(Some(&b));
    d.add_responses(&[("cancel", "_Cancel"), ("ok", action)]);
    d.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    d.set_default_response(Some("ok"));
    d.set_close_response("cancel");

    let original = initial.to_string();
    let validate = Rc::new(validate);
    let check = {
        let d = d.clone();
        let note = note.clone();
        let validate = validate.clone();
        let original = original.clone();
        move |text: &str| {
            let err = names::validate(text).err().or_else(|| validate(text));
            match &err {
                Some(e) => {
                    note.set_label(e);
                    note.add_css_class("error");
                }
                None => {
                    note.remove_css_class("error");
                    note.set_label(&names::warnings(&original, text, is_dir).join(" "));
                }
            }
            d.set_response_enabled("ok", err.is_none() && (allow_unchanged || text != original));
        }
    };
    check(initial);
    entry.connect_changed(move |e| check(&e.text()));

    // Select the name without its extension, like Finder and Dolphin.
    let stem_len = if is_dir { initial.chars().count() } else { names::split_ext(initial).0.chars().count() };
    let e = entry.clone();
    glib::idle_add_local_once(move || {
        e.grab_focus();
        e.select_region(0, stem_len as i32);
    });

    let resp = d.choose_future(Some(parent)).await;
    (resp == "ok").then(|| entry.text().to_string())
}

/// Batch rename: pattern with # counters, or find and replace, with a live
/// preview. Returns the new names, in order.
pub async fn ask_batch_rename(parent: &gtk::Widget, current: Vec<String>) -> Option<Vec<String>> {
    let d = adw::AlertDialog::new(Some(&format!("Rename {} Items", current.len())), None);
    let mode = gtk::DropDown::from_strings(&["Rename using a pattern", "Find and replace"]);
    mode.update_property(&[gtk::accessible::Property::Label("Rename method")]);
    let a = gtk::Entry::builder().text("Item ###").activates_default(true).build();
    let b = gtk::Entry::builder().placeholder_text("Replace with").activates_default(true).visible(false).build();
    let keep_ext = gtk::CheckButton::with_mnemonic("_Keep file extensions");
    keep_ext.set_active(true);
    let case = gtk::CheckButton::with_mnemonic("Match _case");
    case.set_visible(false);
    let help = gtk::Label::builder()
        .label("Each # becomes a digit of the counter: ### gives 001, 002, 003…")
        .xalign(0.0)
        .wrap(true)
        .css_classes(["caption"])
        .build();
    let preview = gtk::Label::builder().xalign(0.0).wrap(true).selectable(true).css_classes(["caption", "monospace"]).build();
    let col = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).build();
    for w in [mode.upcast_ref::<gtk::Widget>(), a.upcast_ref(), b.upcast_ref(), help.upcast_ref(), keep_ext.upcast_ref(), case.upcast_ref(), preview.upcast_ref()] {
        col.append(w);
    }
    d.set_extra_child(Some(&col));
    d.add_responses(&[("cancel", "_Cancel"), ("ok", "_Rename")]);
    d.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    d.set_default_response(Some("ok"));
    d.set_close_response("cancel");

    let result: Rc<RefCell<Option<Vec<String>>>> = Rc::new(RefCell::new(None));
    let update = {
        let (d, mode, a, b, keep_ext, case, help, preview, result, current) =
            (d.clone(), mode.clone(), a.clone(), b.clone(), keep_ext.clone(), case.clone(), help.clone(), preview.clone(), result.clone(), current.clone());
        move || {
            let pattern = mode.selected() == 0;
            b.set_visible(!pattern);
            keep_ext.set_visible(pattern);
            help.set_visible(pattern);
            case.set_visible(!pattern);
            a.set_placeholder_text(Some(if pattern { "Pattern" } else { "Find" }));
            let rule = if pattern {
                BatchRule::Pattern { pattern: a.text().to_string(), start: 1, keep_ext: keep_ext.is_active() }
            } else {
                BatchRule::Replace { find: a.text().to_string(), replace: b.text().to_string(), case_sensitive: case.is_active() }
            };
            match names::batch_rename(&current, &rule) {
                Ok(new) => {
                    let lines: Vec<String> = current
                        .iter()
                        .zip(&new)
                        .take(5)
                        .map(|(o, n)| format!("{o}  →  {n}"))
                        .collect();
                    let more = if new.len() > 5 { format!("\n…and {} more", new.len() - 5) } else { String::new() };
                    preview.remove_css_class("error");
                    preview.set_label(&format!("{}{more}", lines.join("\n")));
                    let changed = new != current;
                    d.set_response_enabled("ok", changed);
                    *result.borrow_mut() = Some(new);
                }
                Err(e) => {
                    preview.add_css_class("error");
                    preview.set_label(&e);
                    d.set_response_enabled("ok", false);
                    *result.borrow_mut() = None;
                }
            }
        }
    };
    update();
    let u = Rc::new(update);
    for e in [&a, &b] {
        let u = u.clone();
        e.connect_changed(move |_| u());
    }
    {
        let u = u.clone();
        mode.connect_selected_notify(move |_| u());
    }
    for c in [&keep_ext, &case] {
        let u = u.clone();
        c.connect_toggled(move |_| u());
    }
    let resp = d.choose_future(Some(parent)).await;
    if resp == "ok" { result.borrow_mut().take() } else { None }
}
