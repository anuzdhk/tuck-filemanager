// SPDX-License-Identifier: GPL-3.0-or-later
//! The install flow (Teku, built in): confirm → live progress → done or
//! error with Show Log. Double-clicking an installer, right-click › Install,
//! and Quick Look's Install button all end up here.

use std::cell::RefCell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use kivo_core::install::{self, FileType, Method, Route, appimage};
use kivo_core::paths;
use kivo_core::trash::Trash;

use crate::window::Window;

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// One row of "key — value" in a dialog.
fn rows(items: &[(&str, String)]) -> gtk::Grid {
    let g = gtk::Grid::builder().row_spacing(6).column_spacing(14).build();
    for (i, (k, v)) in items.iter().enumerate() {
        g.attach(&gtk::Label::builder().label(*k).xalign(1.0).css_classes(["dim-label"]).build(), 0, i as i32, 1, 1);
        g.attach(&gtk::Label::builder().label(v).xalign(0.0).wrap(true).selectable(true).build(), 1, i as i32, 1, 1);
    }
    g
}

/// Lets the Remove buttons rebuild the list they're in.
type Refill = Rc<RefCell<Option<Box<dyn Fn()>>>>;

enum Msg {
    Line(String),
    Done(Result<String, (String, String)>),
}

impl Window {
    /// Start the install flow for `path`.
    pub(crate) fn install_file(&self, path: PathBuf) {
        let ty = install::file_type(&path);
        let release = install::read_os_release();
        let fam = install::family(&release);
        let os = install::os_name(&release);
        let route = install::route(&path, ty, fam, kivo_core::terminal::on_path("flatpak"));
        let parent = self.gtk_window().upcast::<gtk::Widget>();
        let name = name_of(&path);
        match route {
            Route::Refuse(headline, hint) => {
                install::history_add(&install::state_dir(), "error", ty.id(), &path);
                let d = adw::AlertDialog::new(Some(&headline), Some(&hint));
                d.set_extra_child(Some(&rows(&[("File", name), ("Detected", ty.label().to_string()), ("System", os)])));
                d.add_response("ok", "_OK");
                d.present(Some(&parent));
            }
            Route::Install(method) => {
                let mut info = vec![("Type", ty.label().to_string()), ("System", os), ("Method", method.label().to_string())];
                if method == Method::AppImage {
                    let dest = appimage::dest(&appimage::Dirs::user(), &path);
                    info.push(("Destination", paths::display(&dest, &paths::home())));
                }
                let body = if method.needs_admin() {
                    "You'll be asked for your password to install it for everyone on this computer."
                } else if method == Method::Flatpak {
                    "It will be downloaded and installed by Flatpak."
                } else {
                    "It will be moved to your Applications folder and added to your app menu."
                };
                let d = adw::AlertDialog::new(Some(&format!("Install “{name}”?")), Some(body));
                d.set_extra_child(Some(&rows(&info)));
                d.add_responses(&[("cancel", "_Cancel"), ("install", "_Install")]);
                d.set_response_appearance("install", adw::ResponseAppearance::Suggested);
                d.set_default_response(Some("install"));
                d.set_close_response("cancel");
                let weak = self.downgrade();
                glib::MainContext::default().spawn_local(async move {
                    if d.choose_future(Some(&parent)).await == "install"
                        && let Some(w) = Window::from_weak(&weak) {
                            w.run_install(path, ty, method);
                        }
                });
            }
        }
    }

    fn run_install(&self, path: PathBuf, ty: FileType, method: Method) {
        let name = name_of(&path);
        let parent = self.gtk_window().upcast::<gtk::Widget>();

        // Progress: a pulsing bar and the installer's own output, live.
        let bar = gtk::ProgressBar::builder().pulse_step(0.08).build();
        bar.update_property(&[gtk::accessible::Property::Label("Installing")]);
        let status = gtk::Label::builder().label("Starting…").xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).css_classes(["dim-label"]).build();
        let buffer = gtk::TextBuffer::new(None);
        let view = gtk::TextView::builder().buffer(&buffer).editable(false).cursor_visible(false).monospace(true).wrap_mode(gtk::WrapMode::WordChar).top_margin(8).bottom_margin(8).left_margin(8).right_margin(8).build();
        view.update_property(&[gtk::accessible::Property::Label("Installer output")]);
        let scroll = gtk::ScrolledWindow::builder().child(&view).min_content_height(220).vexpand(true).css_classes(["card"]).build();
        let col = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).margin_top(12).margin_bottom(18).margin_start(18).margin_end(18).build();
        col.append(&bar);
        col.append(&status);
        col.append(&scroll);
        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&adw::HeaderBar::builder().show_end_title_buttons(false).build());
        tv.set_content(Some(&col));
        // Not closable while the installer runs: stopping apt halfway is unsafe.
        let dialog = adw::Dialog::builder().title(format!("Installing {name}")).content_width(560).content_height(420).can_close(false).child(&tv).build();
        dialog.present(Some(&parent));
        let pulse = glib::timeout_add_local(std::time::Duration::from_millis(120), {
            let bar = bar.clone();
            move || {
                bar.pulse();
                glib::ControlFlow::Continue
            }
        });

        let logs = install::state_dir().join("logs");
        let _ = std::fs::create_dir_all(&logs);
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let log_path = logs.join(format!("{stamp}-{}.log", appimage::id(&path)));
        let lp = log_path.clone();
        let (tx, rx) = async_channel::unbounded::<Msg>();
        let p = path.clone();
        std::thread::spawn(move || {
            let mut log = std::fs::File::create(&lp).map(|f| Box::new(f) as Box<dyn Write>).unwrap_or_else(|_| Box::new(std::io::sink()));
            let send = |l: &str| {
                let _ = tx.send_blocking(Msg::Line(l.to_string()));
            };
            let result = if method == Method::AppImage {
                let mut lines: Vec<String> = Vec::new();
                let r = appimage::install(&appimage::Dirs::user(), &p, &Trash::for_user(), &mut |l| {
                    lines.push(l.to_string());
                    send(l);
                });
                for l in &lines {
                    let _ = writeln!(log, "{l}");
                }
                match r {
                    Ok(done) => Ok(format!("Find {} in your app menu.", done.name)),
                    Err(e) => {
                        let _ = writeln!(log, "{} {}", e.message, e.hint);
                        Err((format!("Install failed: {}", e.message), e.hint))
                    }
                }
            } else {
                let argv = install::command(method, &p).expect("system installs have a command");
                let _ = writeln!(log, "$ {}", argv.join(" "));
                let code = install::run(&argv, &mut log, &mut |l| send(l));
                if code == 0 { Ok(String::new()) } else { Err(install::failure(method, code)) }
            };
            let _ = tx.send_blocking(Msg::Done(result));
        });

        let weak = self.downgrade();
        let state = install::state_dir();
        glib::MainContext::default().spawn_local(async move {
            let mut result = Err(("Install failed".to_string(), "The installer stopped unexpectedly.".to_string()));
            while let Ok(msg) = rx.recv().await {
                match msg {
                    Msg::Line(l) => {
                        let mut end = buffer.end_iter();
                        buffer.insert(&mut end, &format!("{l}\n"));
                        status.set_label(&format!("▸ {}", l.chars().take(90).collect::<String>()));
                        let mark = buffer.create_mark(None, &buffer.end_iter(), false);
                        view.scroll_mark_onscreen(&mark);
                    }
                    Msg::Done(r) => {
                        result = r;
                        break;
                    }
                }
            }
            pulse.remove();
            dialog.set_can_close(true);
            dialog.force_close();
            let _ = std::fs::copy(&log_path, state.join("last.log"));
            let Some(w) = Window::from_weak(&weak) else { return };
            let parent = w.gtk_window().upcast::<gtk::Widget>();
            match result {
                Ok(note) => {
                    install::history_add(&state, "ok", ty.id(), &path);
                    let d = adw::AlertDialog::new(Some(&format!("Installed “{name}”")), Some(if note.is_empty() { "It's ready to use from your app menu." } else { &note }));
                    d.add_responses(&[("log", "Show _Log"), ("done", "_Done")]);
                    d.set_response_appearance("done", adw::ResponseAppearance::Suggested);
                    d.set_default_response(Some("done"));
                    d.set_close_response("done");
                    if d.choose_future(Some(&parent)).await == "log" {
                        show_log(&parent, &log_path);
                    }
                }
                Err((headline, hint)) => {
                    install::history_add(&state, "failed", ty.id(), &path);
                    let d = adw::AlertDialog::new(Some(&headline), Some(&format!("{hint}\n\n{name}")));
                    d.add_responses(&[("log", "Show _Log"), ("close", "_Close")]);
                    d.set_default_response(Some("close"));
                    d.set_close_response("close");
                    if d.choose_future(Some(&parent)).await == "log" {
                        show_log(&parent, &log_path);
                    }
                }
            }
        });
    }

    /// AppImages installed by Kivo or Teku, with Remove, plus recent history.
    pub(crate) fn show_installed_apps(&self) {
        let parent = self.gtk_window().upcast::<gtk::Widget>();
        let page = adw::PreferencesPage::new();
        let apps_group = adw::PreferencesGroup::builder()
            .title("AppImages")
            .description("Installed with Kivo or Teku. Removing one moves it to the Trash and takes it out of your app menu.")
            .build();
        page.add(&apps_group);
        let dialog = adw::PreferencesDialog::builder().title("Installed Apps").content_width(560).build();
        let fill: Refill = Rc::new(RefCell::new(None));
        let rows_holder: Rc<RefCell<Vec<adw::ActionRow>>> = Rc::new(RefCell::new(Vec::new()));
        let f = {
            let (apps_group, dialog, fill2, rows_holder) = (apps_group.clone(), dialog.clone(), fill.clone(), rows_holder.clone());
            move || {
                for r in rows_holder.borrow_mut().drain(..) {
                    apps_group.remove(&r);
                }
                let dirs = appimage::Dirs::user();
                let list = appimage::list(&dirs);
                if list.is_empty() {
                    let r = adw::ActionRow::builder().title("No AppImages installed yet").subtitle("Double-click an .AppImage file to install it.").build();
                    apps_group.add(&r);
                    rows_holder.borrow_mut().push(r);
                }
                for (app, name) in list {
                    let title = if name.is_empty() { app.to_string_lossy().into_owned() } else { name.clone() };
                    let r = adw::ActionRow::builder()
                        .title(glib::markup_escape_text(&title))
                        .subtitle(glib::markup_escape_text(&paths::display(&app, &paths::home())))
                        .build();
                    let rm = gtk::Button::builder().label("Remove").valign(gtk::Align::Center).css_classes(["destructive-action"]).build();
                    rm.update_property(&[gtk::accessible::Property::Label(&format!("Remove {name}"))]);
                    let (dialog, fill3) = (dialog.clone(), fill2.clone());
                    rm.connect_clicked(move |_| {
                        let dirs = appimage::Dirs::user();
                        match appimage::uninstall(&dirs, &app, &Trash::for_user()) {
                            Ok(()) => {
                                install::history_add(&install::state_dir(), "removed", "appimage", &app);
                                dialog.add_toast(adw::Toast::new("Removed. The AppImage is in the Trash if you need it back."));
                            }
                            Err(e) => dialog.add_toast(adw::Toast::new(&format!("{} {}", e.message, e.hint))),
                        }
                        if let Some(f) = fill3.borrow().as_ref() {
                            f();
                        }
                    });
                    r.add_suffix(&rm);
                    apps_group.add(&r);
                    rows_holder.borrow_mut().push(r);
                }
            }
        };
        f();
        *fill.borrow_mut() = Some(Box::new(f));

        let hist = adw::PreferencesGroup::builder().title("Recent Activity").build();
        let entries = install::history(&install::state_dir());
        if entries.is_empty() {
            hist.add(&adw::ActionRow::builder().title("Nothing installed with Kivo yet").build());
        }
        for e in entries.iter().take(30) {
            let status = match e.status.as_str() {
                "ok" => "Installed",
                "failed" => "Failed",
                "error" => "Not installable",
                "removed" => "Removed",
                other => other,
            };
            let when = e.when.replace('T', " ").chars().take(16).collect::<String>();
            hist.add(&adw::ActionRow::builder().title(glib::markup_escape_text(&name_of(Path::new(&e.file)))).subtitle(format!("{status} · {when}")).build());
        }
        page.add(&hist);
        let logs = adw::PreferencesGroup::new();
        let last = install::state_dir().join("last.log");
        if last.exists() {
            let row = adw::ActionRow::builder().title("Last Install Log").activatable(true).build();
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let parent2 = parent.clone();
            row.connect_activated(move |_| show_log(&parent2, &last));
            logs.add(&row);
            page.add(&logs);
        }
        dialog.add(&page);
        dialog.present(Some(&parent));
    }
}

/// Show an install log in a scrollable, selectable view.
pub fn show_log(parent: &gtk::Widget, path: &Path) {
    let text = std::fs::read_to_string(path).unwrap_or_else(|_| "The log is empty.".into());
    let buffer = gtk::TextBuffer::new(None);
    buffer.set_text(&text);
    let view = gtk::TextView::builder().buffer(&buffer).editable(false).monospace(true).wrap_mode(gtk::WrapMode::WordChar).top_margin(12).bottom_margin(12).left_margin(12).right_margin(12).build();
    view.update_property(&[gtk::accessible::Property::Label("Install log")]);
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    tv.set_content(Some(&gtk::ScrolledWindow::builder().child(&view).vexpand(true).build()));
    adw::Dialog::builder().title("Install Log").content_width(720).content_height(460).child(&tv).build().present(Some(parent));
}
