// SPDX-License-Identifier: GPL-3.0-or-later
//! Addons: the Preferences page with a switch for each (see
//! `tuck_core::addons`), and the Image Converter and Video Compressor
//! dialogs and jobs. The 3D viewer lives in Quick Look and thumbnails.

use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gio, glib};
use tuck_core::addons::{self, Addon};
use tuck_core::format;
use tuck_core::media::{self, ImageFormat, ImageOptions, VideoOptions, VideoQuality};

use crate::theme_ui;
use crate::window::Window;

pub fn enabled(id: &str) -> bool {
    addons::enabled(&theme_ui::settings(), id)
}

/// Preferences › Addons.
pub fn page() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder().title("Addons").name("addons").icon_name("tuck-addons-symbolic").build();
    let group = adw::PreferencesGroup::builder()
        .description("Optional tools built into Tuck. Turn off the ones you don't use and they disappear from menus and previews.")
        .build();
    for a in addons::ALL {
        group.add(&row(a));
    }
    page.add(&group);
    page
}

fn row(a: &'static Addon) -> adw::SwitchRow {
    let subtitle = match addons::programs(a) {
        Some(_) => a.summary.to_string(),
        None => format!("{}\nNeeds a program that isn't installed. {}", a.summary, a.install_hint),
    };
    let row = adw::SwitchRow::builder().title(a.name).subtitle(glib::markup_escape_text(&subtitle).as_str()).active(enabled(a.id)).build();
    row.set_subtitle_lines(0);
    row.connect_active_notify(move |r| {
        let on = r.is_active();
        theme_ui::update_quiet(|s| {
            s.addons.insert(a.id.to_string(), on);
        });
        // Thumbnails and previews follow at once.
        if a.id == addons::MODEL_VIEWER {
            Window::refresh_all_icons();
        }
    });
    row
}

/// Menu entries the selection gets from addons: (label, action). Folders
/// offer Convert Images for every image inside.
pub fn menu_items(paths: &[PathBuf], has_dir: bool) -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    if enabled(addons::IMAGE_CONVERTER) && (has_dir || paths.iter().any(|p| media::is_image(p))) {
        out.push(("Con_vert Images…", "win.convert-images"));
    }
    // Videos only when picked: folders of them are rare, and the menu is long.
    if enabled(addons::VIDEO_COMPRESSOR) && paths.iter().any(|p| media::is_video(p)) {
        out.push(("Compress _Video…", "win.compress-video"));
    }
    out
}

pub fn setup_actions(w: &Window) {
    for (name, f) in [("convert-images", convert_images as fn(&Window)), ("compress-video", compress_video)] {
        let a = gio::SimpleAction::new(name, None);
        let weak = w.downgrade();
        a.connect_activate(move |_, _| {
            if let Some(w) = Window::from_weak(&weak) {
                f(&w);
            }
        });
        w.gtk_window().add_action(&a);
    }
}

/// The program an addon runs, or a toast saying what to install.
fn program(w: &Window, id: &str) -> Option<&'static str> {
    let a = addons::find(id)?;
    match addons::programs(a) {
        Some(p) => Some(p.first().copied().unwrap_or_default()),
        None => {
            w.toast(&format!("{} needs a program that isn't installed. {}", a.name, a.install_hint));
            None
        }
    }
}

fn combo(title: &str, items: &[&str], selected: u32) -> adw::ComboRow {
    adw::ComboRow::builder().title(title).model(&gtk::StringList::new(items)).selected(selected).build()
}

fn boxed(rows: &[&gtk::Widget]) -> gtk::ListBox {
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
    for r in rows {
        list.append(*r);
    }
    list
}

const SIDES: &[(&str, Option<u32>)] = &[("Original size", None), ("At most 3840 px", Some(3840)), ("At most 1920 px", Some(1920)), ("At most 1280 px", Some(1280))];

fn convert_images(w: &Window) {
    let Some(paths) = w.selected_paths() else { return };
    let Some(prog) = program(w, addons::IMAGE_CONVERTER) else { return };
    let formats: Vec<&str> = ImageFormat::ALL.iter().map(|f| f.label()).collect();
    let fmt = combo("Format", &formats, 0);
    let quality = adw::SpinRow::with_range(10.0, 100.0, 5.0);
    quality.set_title("Quality");
    quality.set_subtitle("Lower makes smaller files");
    quality.set_value(70.0);
    let size = combo("Size", &SIDES.iter().map(|s| s.0).collect::<Vec<_>>(), 0);
    {
        let q = quality.clone();
        fmt.connect_selected_notify(move |f| q.set_visible(ImageFormat::ALL[f.selected() as usize].lossy()));
    }
    let what = if paths.iter().any(|p| p.is_dir()) { "Every image in the selected folders is included. " } else { "" };
    let d = adw::AlertDialog::new(Some("Convert Images"), Some(&format!("{what}New files are saved next to the originals, which stay as they are.")));
    d.set_extra_child(Some(&boxed(&[fmt.upcast_ref(), quality.upcast_ref(), size.upcast_ref()])));
    d.add_responses(&[("cancel", "_Cancel"), ("ok", "_Convert")]);
    d.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    d.set_default_response(Some("ok"));
    d.set_close_response("cancel");
    let weak = w.downgrade();
    glib::MainContext::default().spawn_local(async move {
        let Some(w) = Window::from_weak(&weak) else { return };
        if d.choose_future(Some(&w.parent_widget())).await != "ok" {
            return;
        }
        let o = ImageOptions {
            format: ImageFormat::ALL[fmt.selected() as usize],
            quality: quality.value() as u8,
            max_side: SIDES[size.selected() as usize].1,
        };
        let weak = w.downgrade();
        w.jobs().run(
            &w.parent_widget(),
            &format!("Converting to {}", o.format.label()),
            move |obs, cancel| {
                let files = media::collect(&paths, media::is_image);
                media::convert_images(prog, &files, &o, obs, cancel)
            },
            move |out| finished(&weak, out, "image", "Converted"),
        );
    });
}

const HEIGHTS: &[(&str, Option<u32>)] = &[("Original size", None), ("1080p", Some(1080)), ("720p", Some(720)), ("480p", Some(480))];

fn compress_video(w: &Window) {
    let Some(paths) = w.selected_paths() else { return };
    if program(w, addons::VIDEO_COMPRESSOR).is_none() {
        return;
    }
    let quality = combo("Quality", &VideoQuality::ALL.iter().map(|q| q.label()).collect::<Vec<_>>(), 1);
    let size = combo("Size", &HEIGHTS.iter().map(|s| s.0).collect::<Vec<_>>(), 0);
    let d = adw::AlertDialog::new(
        Some("Compress Video"),
        Some("Saved as MP4 (H.264) next to the original, which stays as it is. Long videos take a while; you can keep working."),
    );
    d.set_extra_child(Some(&boxed(&[quality.upcast_ref(), size.upcast_ref()])));
    d.add_responses(&[("cancel", "_Cancel"), ("ok", "_Compress")]);
    d.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    d.set_default_response(Some("ok"));
    d.set_close_response("cancel");
    let weak = w.downgrade();
    glib::MainContext::default().spawn_local(async move {
        let Some(w) = Window::from_weak(&weak) else { return };
        if d.choose_future(Some(&w.parent_widget())).await != "ok" {
            return;
        }
        let o = VideoOptions { quality: VideoQuality::ALL[quality.selected() as usize], max_height: HEIGHTS[size.selected() as usize].1 };
        let weak = w.downgrade();
        w.jobs().run(
            &w.parent_widget(),
            "Compressing video",
            move |obs, cancel| {
                let files: Vec<PathBuf> = paths.into_iter().filter(|p| media::is_video(p)).collect();
                media::compress_videos(&files, &o, obs, cancel)
            },
            move |out| finished(&weak, out, "video", "Compressed"),
        );
    });
}

fn finished(weak: &std::rc::Weak<crate::window::Inner>, out: media::Outcome, noun: &str, verb: &str) {
    let Some(w) = Window::from_weak(weak) else { return };
    let n = out.made.len();
    if n > 0 {
        let what = if n == 1 { format!("1 {noun}") } else { format!("{n} {noun}s") };
        let change = if out.after < out.before {
            let pct = 100 - out.after * 100 / out.before.max(1);
            format!(" · {} smaller ({pct}%)", format::size(out.before - out.after))
        } else {
            format!(" · {} larger", format::size(out.after - out.before))
        };
        w.toast(&format!("{verb} {what}{change}"));
        w.select_results(&out.made);
    } else if out.cancelled {
        w.toast("Cancelled. Unfinished files were removed.");
    } else if out.errors.is_empty() {
        w.toast(&format!("No {noun}s found in the selection."));
    }
    crate::dialogs::show_errors(&w.parent_widget(), &format!("Some {noun}s couldn't be {}", verb.to_lowercase()), &out.errors);
}
