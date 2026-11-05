// SPDX-License-Identifier: GPL-3.0-or-later
//! Test hooks for driving the real app from scripts (used for screenshots and
//! smoke tests). Inactive unless the environment variables are set.
//!
//! KIVO_ACTIONS="win.view-mode::details;win.split;sleep"  steps, in order:
//!     an action name (win.x / app.x, with ::target), "sleep",
//!     "select:a|b" (select names in the active pane),
//!     "type:text" (set the focused text field), "respond:id" (answer the
//!     open dialog), "go:/path" (navigate the active pane)
//! KIVO_SCREENSHOT=/path/out.png                          capture, then quit
//! KIVO_STEP_MS=500                                       delay between steps
//! KIVO_DEBUG_TIMING=1                                    print listing timings
//! KIVO_LIST_DELAY_MS=2000                                hold back the bulk of a
//!     listing, to measure opening a big folder in an already-mapped window

use adw::prelude::*;
use gtk::{gio, glib, graphene};

pub fn run_script(app: &adw::Application) {
    let actions = std::env::var("KIVO_ACTIONS").unwrap_or_default();
    let shot = std::env::var("KIVO_SCREENSHOT").ok();
    if actions.is_empty() && shot.is_none() {
        return;
    }
    let step: u64 = std::env::var("KIVO_STEP_MS").ok().and_then(|s| s.parse().ok()).unwrap_or(500);
    let steps: Vec<String> =
        actions.split(';').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
    let app = app.clone();
    glib::MainContext::default().spawn_local(async move {
        glib::timeout_future(std::time::Duration::from_millis(step * 2)).await;
        for s in steps {
            step_once(&app, &s);
            glib::timeout_future(std::time::Duration::from_millis(step)).await;
        }
        if let Some(path) = shot {
            glib::timeout_future(std::time::Duration::from_millis(step)).await;
            // The main window, even if Quick Look is the active one.
            let main = app.windows().into_iter().find(|w| crate::window::Window::find(w).is_some());
            if let Some(w) = main {
                match capture(&w, &path) {
                    Ok(()) => eprintln!("screenshot: {path}"),
                    Err(e) => eprintln!("screenshot failed: {e}"),
                }
                // Quick Look is its own window.
                let ql = crate::window::Window::find(&w).and_then(|x| x.quicklook_window());
                if let Some(q) = ql {
                    let qp = path.replace(".png", "-ql.png");
                    if capture(q.upcast_ref(), &qp).is_ok() {
                        eprintln!("screenshot: {qp}");
                    }
                }
                // Popovers are separate surfaces; save an open menu too.
                let menu = crate::window::Window::find(&w).and_then(|x| x.active_pane()).and_then(|p| p.open_menu());
                // SAFETY: only ever set to a gtk::Popover above.
                if let Some(p) = unsafe { app.steal_data::<gtk::Popover>("kivo-jobs-popover") } {
                    let jp = path.replace(".png", "-jobs.png");
                    if capture_widget(p.upcast_ref(), &jp).is_ok() {
                        eprintln!("screenshot: {jp}");
                    }
                }
                if let Some(m) = menu {
                    let mp = path.replace(".png", "-menu.png");
                    if capture_widget(m.upcast_ref(), &mp).is_ok() {
                        eprintln!("screenshot: {mp}");
                    }
                }
            }
            // Cancel running jobs, then close normally so close handlers
            // (session save) run.
            let jobs = app.windows().iter().find_map(crate::window::Window::find).map(|w| w.jobs());
            let app2 = app.clone();
            let close = move || {
                for w in app2.windows() {
                    w.close();
                }
            };
            match jobs {
                Some(j) => j.cancel_all(close),
                None => close(),
            }
        }
    });
}

fn step_once(app: &adw::Application, s: &str) {
    // Always the main window, even while Quick Look has focus.
    let win = app.windows().into_iter().find(|w| crate::window::Window::find(w).is_some());
    if s == "sleep" {
        return;
    }
    if let Some(names) = s.strip_prefix("select:") {
        let w = win.as_ref().and_then(crate::window::Window::find);
        if let Some(p) = w.and_then(|w| w.active_pane()) {
            p.select_names(names.split('|').map(std::ffi::OsString::from).collect());
        }
        return;
    }
    // drop-copy:/a|/b, drop-move:…, drop-link:… onto the active pane's folder.
    for (prefix, action) in [("drop-copy:", gtk::gdk::DragAction::COPY), ("drop-move:", gtk::gdk::DragAction::MOVE), ("drop-link:", gtk::gdk::DragAction::LINK)] {
        if let Some(list) = s.strip_prefix(prefix) {
            if let Some(w) = win.as_ref().and_then(crate::window::Window::find)
                && let Some(p) = w.active_pane() {
                    let files = list.split('|').map(gio::File::for_path).collect();
                    w.handle_drop(files, p.location(), action);
                }
            return;
        }
    }
    if s == "jobs" {
        if let Some(w) = win.as_ref().and_then(crate::window::Window::find)
            && let Some(p) = w.jobs().popup() {
                unsafe { app.set_data("kivo-jobs-popover", p) };
            }
        return;
    }
    if let Some(d) = s.strip_prefix("ql-step:") {
        if let (Some(w), Ok(d)) = (win.as_ref().and_then(crate::window::Window::find), d.parse::<i32>()) {
            w.quicklook_step(d);
        }
        return;
    }
    if let Some(text) = s.strip_prefix("search:") {
        if let Some(w) = win.as_ref().and_then(crate::window::Window::find) {
            w.set_search_text(text);
        }
        return;
    }
    if s == "menu" {
        let w = win.as_ref().and_then(crate::window::Window::find);
        if let Some(p) = w.and_then(|w| w.active_pane()) {
            p.keyboard_menu();
        }
        return;
    }
    if let Some(path) = s.strip_prefix("go:") {
        let w = win.as_ref().and_then(crate::window::Window::find);
        if let Some(p) = w.and_then(|w| w.active_pane()) {
            p.navigate(if path.contains("://") { gio::File::for_uri(path) } else { gio::File::for_path(path) });
        }
        return;
    }
    if let Some(text) = s.strip_prefix("type:") {
        // The focused widget may be the GtkText inside an entry.
        let focus = win.as_ref().and_then(gtk::prelude::GtkWindowExt::focus).or_else(|| {
            win.clone().and_downcast::<adw::ApplicationWindow>().and_then(|w| w.visible_dialog()).and_then(|d| d.focus())
        });
        let mut cur = focus;
        while let Some(w) = cur {
            if let Some(e) = w.dynamic_cast_ref::<gtk::Editable>() {
                e.set_text(text);
                return;
            }
            cur = w.parent();
        }
        // Otherwise the first text entry in the open dialog.
        let dialog = win.clone().and_downcast::<adw::ApplicationWindow>().and_then(|w| w.visible_dialog());
        if let Some(e) = dialog.and_then(|d| first_entry(d.upcast_ref())) {
            e.set_text(text);
            return;
        }
        eprintln!("type: no focused text field");
        return;
    }
    if let Some(id) = s.strip_prefix("respond:") {
        let dialog = win.clone().and_downcast::<adw::ApplicationWindow>().and_then(|w| w.visible_dialog());
        match dialog.and_downcast::<adw::AlertDialog>() {
            Some(d) => {
                d.emit_by_name::<()>("response", &[&id]);
                d.force_close();
            }
            None => eprintln!("respond: no alert dialog open"),
        }
        return;
    }
    activate(app, s);
}

fn activate(app: &adw::Application, detailed: &str) {
    let Ok((name, target)) = gio::Action::parse_detailed_name(detailed) else {
        eprintln!("bad action: {detailed}");
        return;
    };
    let (scope, name) = name.split_once('.').unwrap_or(("win", name.as_str()));
    let group: Option<gio::ActionGroup> = match scope {
        "app" => Some(app.clone().upcast()),
        _ => app.windows().into_iter().find(|w| crate::window::Window::find(w).is_some()).and_downcast::<gtk::ApplicationWindow>().map(|w| w.upcast()),
    };
    if let Some(g) = group {
        g.activate_action(name, target.as_ref());
    }
}

pub fn capture(win: &gtk::Window, path: &str) -> Result<(), String> {
    capture_widget(win.upcast_ref(), path)
}

fn capture_widget(win: &gtk::Widget, path: &str) -> Result<(), String> {
    let (w, h) = (win.width(), win.height());
    let paintable = gtk::WidgetPaintable::new(Some(win));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, w as f64, h as f64);
    let node = snapshot.to_node().ok_or("nothing rendered")?;
    let renderer = win.native().and_then(|n| n.renderer()).ok_or("no renderer")?;
    let tex = renderer.render_texture(&node, Some(&graphene::Rect::new(0.0, 0.0, w as f32, h as f32)));
    tex.save_to_png(path).map_err(|e| e.to_string())
}

fn first_entry(w: &gtk::Widget) -> Option<gtk::Entry> {
    if let Some(e) = w.downcast_ref::<gtk::Entry>() {
        return Some(e.clone());
    }
    let mut c = w.first_child();
    while let Some(child) = c {
        if let Some(e) = first_entry(&child) {
            return Some(e);
        }
        c = child.next_sibling();
    }
    None
}
