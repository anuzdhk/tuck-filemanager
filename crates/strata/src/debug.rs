// SPDX-License-Identifier: GPL-3.0-or-later
//! Test hooks for driving the real app from scripts (used for screenshots and
//! smoke tests). Inactive unless the environment variables are set.
//!
//! STRATA_ACTIONS="win.view-mode::details;win.split;sleep"  actions, one per step
//! STRATA_SCREENSHOT=/path/out.png                          capture, then quit
//! STRATA_STEP_MS=500                                       delay between steps
//! STRATA_DEBUG_TIMING=1                                    print listing timings
//! STRATA_LIST_DELAY_MS=2000                                hold back the bulk of a
//!     listing, to measure opening a big folder in an already-mapped window

use adw::prelude::*;
use gtk::{gio, glib, graphene};

pub fn run_script(app: &adw::Application) {
    let actions = std::env::var("STRATA_ACTIONS").unwrap_or_default();
    let shot = std::env::var("STRATA_SCREENSHOT").ok();
    if actions.is_empty() && shot.is_none() {
        return;
    }
    let step: u64 = std::env::var("STRATA_STEP_MS").ok().and_then(|s| s.parse().ok()).unwrap_or(500);
    let steps: Vec<String> =
        actions.split(';').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
    let app = app.clone();
    glib::MainContext::default().spawn_local(async move {
        glib::timeout_future(std::time::Duration::from_millis(step * 2)).await;
        for s in steps {
            if s != "sleep" {
                activate(&app, &s);
            }
            glib::timeout_future(std::time::Duration::from_millis(step)).await;
        }
        if let Some(path) = shot {
            glib::timeout_future(std::time::Duration::from_millis(step)).await;
            if let Some(w) = app.active_window() {
                match capture(&w, &path) {
                    Ok(()) => eprintln!("screenshot: {path}"),
                    Err(e) => eprintln!("screenshot failed: {e}"),
                }
            }
            // Close windows normally so close handlers (session save) run.
            for w in app.windows() {
                w.close();
            }
        }
    });
}

fn activate(app: &adw::Application, detailed: &str) {
    let Ok((name, target)) = gio::Action::parse_detailed_name(detailed) else {
        eprintln!("bad action: {detailed}");
        return;
    };
    let (scope, name) = name.split_once('.').unwrap_or(("win", name.as_str()));
    let group: Option<gio::ActionGroup> = match scope {
        "app" => Some(app.clone().upcast()),
        _ => app.active_window().and_downcast::<gtk::ApplicationWindow>().map(|w| w.upcast()),
    };
    if let Some(g) = group {
        g.activate_action(name, target.as_ref());
    }
}

pub fn capture(win: &gtk::Window, path: &str) -> Result<(), String> {
    let (w, h) = (win.width(), win.height());
    let paintable = gtk::WidgetPaintable::new(Some(win));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, w as f64, h as f64);
    let node = snapshot.to_node().ok_or("nothing rendered")?;
    let renderer = win.native().and_then(|n| n.renderer()).ok_or("no renderer")?;
    let tex = renderer.render_texture(&node, Some(&graphene::Rect::new(0.0, 0.0, w as f32, h as f32)));
    tex.save_to_png(path).map_err(|e| e.to_string())
}
