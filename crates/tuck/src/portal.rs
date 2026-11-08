// SPDX-License-Identifier: GPL-3.0-or-later
//! `tuck --portal`: the xdg-desktop-portal FileChooser backend, so other
//! apps' Open and Save dialogs are Tuck windows. Started by D-Bus on demand
//! (`org.freedesktop.impl.portal.desktop.tuck.service`); picked as the file
//! chooser in Preferences › Updates (see `tuck_core::portal`). It quits a
//! few minutes after the last picker closes.
//!
//! shortcut: file type filters are ignored (every file shows) and the picker
//! isn't attached to the app's window; add both when apps need them.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};

use crate::picker::{Mode, Request};

const PATH: &str = "/org/freedesktop/portal/desktop";

const FILE_CHOOSER_XML: &str = r#"<node><interface name="org.freedesktop.impl.portal.FileChooser">
  <method name="OpenFile"><arg type="o" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="a{sv}" direction="in"/><arg type="u" direction="out"/><arg type="a{sv}" direction="out"/></method>
  <method name="SaveFile"><arg type="o" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="a{sv}" direction="in"/><arg type="u" direction="out"/><arg type="a{sv}" direction="out"/></method>
  <method name="SaveFiles"><arg type="o" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="a{sv}" direction="in"/><arg type="u" direction="out"/><arg type="a{sv}" direction="out"/></method>
</interface></node>"#;

const REQUEST_XML: &str = r#"<node><interface name="org.freedesktop.impl.portal.Request"><method name="Close"/></interface></node>"#;

/// How long to stay around with no picker open.
const IDLE_SECS: u32 = 180;

thread_local! {
    static HOLD: RefCell<Option<gio::ApplicationHoldGuard>> = const { RefCell::new(None) };
    static OPEN: Cell<u32> = const { Cell::new(0) };
}

/// Claim the bus name and serve requests.
pub fn start(app: &adw::Application, folder_views: Rc<RefCell<tuck_core::settings::FolderViews>>) {
    HOLD.with(|h| *h.borrow_mut() = Some(app.hold()));
    let app2 = app.clone();
    gio::bus_own_name(
        gio::BusType::Session,
        tuck_core::portal::BUS_NAME,
        gio::BusNameOwnerFlags::NONE,
        move |conn, _| register(&conn, &app2, folder_views.clone()),
        |_, _| {},
        {
            let app = app.clone();
            move |_, name| {
                eprintln!("tuck --portal: couldn't own {name} (another one is running?)");
                app.quit();
            }
        },
    );
    idle_later();
}

fn register(conn: &gio::DBusConnection, app: &adw::Application, fv: Rc<RefCell<tuck_core::settings::FolderViews>>) {
    let info = gio::DBusNodeInfo::for_xml(FILE_CHOOSER_XML).expect("interface XML").lookup_interface("org.freedesktop.impl.portal.FileChooser").expect("interface");
    let app = app.clone();
    let res = conn
        .register_object(PATH, &info)
        .method_call(move |conn, _sender, _path, _iface, method, params, invocation| {
            let handle = params.child_value(0).str().unwrap_or_default().to_string();
            let title = params.child_value(3).str().unwrap_or_default().to_string();
            let options = glib::VariantDict::new(Some(&params.child_value(4)));
            let Some(req) = request(method, title, &options) else {
                invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", "Unknown method");
                return;
            };
            serve(&conn, &app, fv.clone(), &handle, req, invocation);
        })
        .build();
    if let Err(e) = res {
        eprintln!("tuck --portal: {e}");
    }
}

/// The options of one call, as a picker request.
fn request(method: &str, title: String, o: &glib::VariantDict) -> Option<Request> {
    let flag = |k: &str| o.lookup::<bool>(k).ok().flatten().unwrap_or(false);
    let text = |k: &str| o.lookup::<String>(k).ok().flatten();
    let mode = match method {
        "OpenFile" => Mode::Open { multiple: flag("multiple"), directory: flag("directory") },
        "SaveFile" => {
            // `current_file` (an existing file to save over) names it too.
            let name = text("current_name").or_else(|| path_opt(o, "current_file").and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))).unwrap_or_default();
            Mode::Save { name }
        }
        "SaveFiles" => {
            let names = o
                .lookup_value("files", Some(glib::VariantTy::BYTE_STRING_ARRAY))
                .map(|v| v.iter().map(|b| bytes_path(&b)).filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).collect())
                .unwrap_or_default();
            Mode::SaveFiles { names }
        }
        _ => return None,
    };
    let folder = path_opt(o, "current_folder").or_else(|| path_opt(o, "current_file").and_then(|p| p.parent().map(PathBuf::from)));
    Some(Request { mode, title, accept_label: text("accept_label"), folder })
}

/// A file path sent as bytes (`ay`, NUL-terminated).
fn bytes_path(v: &glib::Variant) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let b = v.fixed_array::<u8>().unwrap_or_default();
    let b = b.strip_suffix(&[0]).unwrap_or(b);
    PathBuf::from(std::ffi::OsStr::from_bytes(b))
}

fn path_opt(o: &glib::VariantDict, key: &str) -> Option<PathBuf> {
    o.lookup_value(key, Some(glib::VariantTy::BYTE_STRING)).map(|v| bytes_path(&v)).filter(|p| !p.as_os_str().is_empty())
}

fn serve(conn: &gio::DBusConnection, app: &adw::Application, fv: Rc<RefCell<tuck_core::settings::FolderViews>>, handle: &str, req: Request, invocation: gio::DBusMethodInvocation) {
    OPEN.with(|n| n.set(n.get() + 1));
    let invocation = Rc::new(RefCell::new(Some(invocation)));
    let request_id: Rc<Cell<Option<gio::RegistrationId>>> = Rc::default();
    let (conn2, rid) = (conn.clone(), request_id.clone());
    let inv = invocation.clone();
    let window = crate::picker::open(app, fv, req, move |uris| {
        // Reply once: 0 = picked, 1 = cancelled.
        if let Some(i) = inv.borrow_mut().take() {
            let results = glib::VariantDict::new(None);
            let code: u32 = match uris {
                Some(u) => {
                    results.insert_value("uris", &u.to_variant());
                    0
                }
                None => 1,
            };
            i.return_value(Some(&glib::Variant::tuple_from_iter([code.to_variant(), results.end()])));
        }
        if let Some(id) = rid.take() {
            let _ = conn2.unregister_object(id);
        }
        OPEN.with(|n| n.set(n.get().saturating_sub(1)));
        idle_later();
    });
    // The app can withdraw its request: close the picker (it then answers).
    let info = gio::DBusNodeInfo::for_xml(REQUEST_XML).expect("request XML").lookup_interface("org.freedesktop.impl.portal.Request").expect("interface");
    let weak = window.gtk_window().downgrade();
    if let Ok(id) = conn
        .register_object(handle, &info)
        .method_call(move |_, _, _, _, _, _, invocation| {
            if let Some(w) = weak.upgrade() {
                w.close();
            }
            invocation.return_value(None);
        })
        .build()
    {
        request_id.set(Some(id));
    }
    if OPEN.with(|n| n.get()) == 1 {
        crate::debug::run_script(app);
    }
}

/// Quit after a while with nothing open (D-Bus starts us again on demand).
fn idle_later() {
    glib::timeout_add_seconds_local_once(IDLE_SECS, || {
        if OPEN.with(|n| n.get()) == 0 {
            HOLD.with(|h| h.borrow_mut().take());
        }
    });
}
