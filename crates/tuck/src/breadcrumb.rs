// SPDX-License-Identifier: GPL-3.0-or-later
//! Location bar: clickable breadcrumbs that turn into an editable path entry
//! (click the empty area, or Ctrl+L). Tab completes folder names.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use tuck_core::paths;

type OnNavigate = Rc<dyn Fn(gio::File)>;
type OnError = Rc<dyn Fn(String)>;

pub struct Inner {
    stack: gtk::Stack,
    crumbs: gtk::Box,
    scroller: gtk::ScrolledWindow,
    entry: gtk::Entry,
    current: RefCell<Option<gio::File>>,
    on_navigate: RefCell<Option<OnNavigate>>,
    on_error: RefCell<Option<OnError>>,
}

#[derive(Clone)]
pub struct Breadcrumb(Rc<Inner>);

impl Breadcrumb {
    pub fn new() -> Self {
        let crumbs = gtk::Box::builder().spacing(2).css_classes(["crumbs"]).build();
        let scroller = gtk::ScrolledWindow::builder()
            .child(&crumbs)
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true)
            .propagate_natural_width(true)
            .build();
        let entry = gtk::Entry::builder()
            .hexpand(true)
            .placeholder_text("Type a path, e.g. ~/Documents")
            .input_purpose(gtk::InputPurpose::Url)
            .build();
        entry.update_property(&[gtk::accessible::Property::Label("Location")]);
        let stack = gtk::Stack::builder()
            .hexpand(true)
            .hhomogeneous(false)
            .css_classes(["location-bar"])
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(120)
            .build();
        stack.add_named(&scroller, Some("crumbs"));
        stack.add_named(&entry, Some("entry"));

        let bc = Breadcrumb(Rc::new(Inner {
            stack,
            crumbs,
            scroller,
            entry,
            current: RefCell::new(None),
            on_navigate: RefCell::new(None),
            on_error: RefCell::new(None),
        }));
        bc.wire();
        bc
    }

    pub fn widget(&self) -> gtk::Widget {
        self.0.stack.clone().upcast()
    }

    /// The path's scroll range: `upper` is the width the path wants,
    /// `page_size` the width it has.
    pub fn width_adjustment(&self) -> gtk::Adjustment {
        self.0.scroller.hadjustment()
    }

    pub fn connect_navigate(&self, f: impl Fn(gio::File) + 'static) {
        *self.0.on_navigate.borrow_mut() = Some(Rc::new(f));
    }

    pub fn connect_error(&self, f: impl Fn(String) + 'static) {
        *self.0.on_error.borrow_mut() = Some(Rc::new(f));
    }

    fn wire(&self) {
        // Clicking blank space in the crumb bar starts editing.
        let click = gtk::GestureClick::new();
        let weak = Rc::downgrade(&self.0);
        click.connect_released(move |g, _, x, y| {
            let Some(inner) = weak.upgrade() else { return };
            let hit = inner.scroller.pick(x, y, gtk::PickFlags::DEFAULT);
            let on_button = hit.is_some_and(|w| {
                w.is::<gtk::Button>() || w.ancestor(gtk::Button::static_type()).is_some()
            });
            if !on_button {
                g.set_state(gtk::EventSequenceState::Claimed);
                Breadcrumb(inner).edit();
            }
        });
        self.0.scroller.add_controller(click);

        let weak = Rc::downgrade(&self.0);
        self.0.entry.connect_activate(move |e| {
            if let Some(inner) = weak.upgrade() {
                Breadcrumb(inner).submit(&e.text());
            }
        });

        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&self.0);
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(inner) = weak.upgrade() else { return glib::Propagation::Proceed };
            let bc = Breadcrumb(inner);
            match key {
                gdk::Key::Escape => {
                    bc.stop_editing();
                    glib::Propagation::Stop
                }
                gdk::Key::Tab => {
                    bc.complete();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        self.0.entry.add_controller(keys);

        let focus = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(&self.0);
        focus.connect_leave(move |_| {
            if let Some(inner) = weak.upgrade() {
                // Leave edit mode when focus moves elsewhere.
                glib::idle_add_local_once(move || {
                    if !inner.entry.has_focus() {
                        inner.stack.set_visible_child_name("crumbs");
                    }
                });
            }
        });
        self.0.entry.add_controller(focus);
    }

    pub fn set_location(&self, file: &gio::File) {
        *self.0.current.borrow_mut() = Some(file.clone());
        let crumbs = &self.0.crumbs;
        while let Some(c) = crumbs.first_child() {
            crumbs.remove(&c);
        }
        let home = paths::home();
        let segments: Vec<(String, gio::File)> = match file.path() {
            Some(p) => paths::breadcrumbs(&p, &home)
                .into_iter()
                .map(|c| (c.label, gio::File::for_path(c.path)))
                .collect(),
            None => remote_segments(file),
        };
        let last = segments.len().saturating_sub(1);
        for (i, (label, target)) in segments.into_iter().enumerate() {
            if i > 0 {
                let sep = gtk::Image::builder()
                    .icon_name("tuck-chevron-symbolic")
                    .pixel_size(12)
                    .css_classes(["crumb-sep"])
                    .build();
                sep.set_accessible_role(gtk::AccessibleRole::Presentation);
                crumbs.append(&sep);
            }
            let btn = gtk::Button::builder()
                .label(&label)
                .css_classes(["flat", "crumb"])
                .tooltip_text(target.parse_name().as_str())
                .build();
            if i == last {
                btn.add_css_class("current");
                btn.update_state(&[gtk::accessible::State::Selected(Some(true))]);
            }
            let weak = Rc::downgrade(&self.0);
            btn.connect_clicked(move |_| {
                if let Some(inner) = weak.upgrade() {
                    let cb = inner.on_navigate.borrow().clone();
                    if let Some(cb) = cb {
                        cb(target.clone());
                    }
                }
            });
            crumbs.append(&btn);
        }
        // Keep the current folder visible on long paths.
        let adj = self.0.scroller.hadjustment();
        glib::idle_add_local_once(move || adj.set_value(adj.upper()));
        self.0.stack.set_visible_child_name("crumbs");
    }

    pub fn edit(&self) {
        let text = self
            .0
            .current
            .borrow()
            .as_ref()
            .map(|f| match f.path() {
                Some(p) => paths::display(&p, &paths::home()),
                None => f.uri().to_string(),
            })
            .unwrap_or_default();
        self.0.entry.set_text(&text);
        self.0.stack.set_visible_child_name("entry");
        self.0.entry.grab_focus();
        self.0.entry.set_position(-1);
    }

    fn stop_editing(&self) {
        self.0.stack.set_visible_child_name("crumbs");
    }

    fn cwd(&self) -> std::path::PathBuf {
        self.0.current.borrow().as_ref().and_then(|f| f.path()).unwrap_or_else(paths::home)
    }

    fn submit(&self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            self.stop_editing();
            return;
        }
        let file = if text.contains("://") && !text.starts_with("file://") {
            gio::File::for_uri(text)
        } else {
            gio::File::for_path(paths::expand_input(text, &self.cwd(), &paths::home()))
        };
        let is_local = file.path().is_some();
        if is_local {
            let ty = file.query_file_type(gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE);
            if ty != gio::FileType::Directory {
                let msg = if ty == gio::FileType::Unknown {
                    format!("“{text}” doesn't exist. Check the spelling and try again.")
                } else {
                    format!("“{text}” is a file, not a folder.")
                };
                let cb = self.0.on_error.borrow().clone();
                if let Some(cb) = cb {
                    cb(msg);
                }
                return;
            }
        }
        self.stop_editing();
        let cb = self.0.on_navigate.borrow().clone();
        if let Some(cb) = cb {
            cb(file);
        }
    }

    fn complete(&self) {
        let typed = self.0.entry.text().to_string();
        if typed.contains("://") {
            return;
        }
        let expanded = paths::expand_input(&typed, &self.cwd(), &paths::home());
        // expand_input normalizes away a trailing slash; keep the intent.
        let expanded = if typed.ends_with('/') { expanded.join("") } else { expanded };
        if let Some(done) = paths::complete(&typed, Path::new(&expanded), false) {
            self.0.entry.set_text(&done);
            self.0.entry.set_position(-1);
        } else {
            self.0.entry.error_bell();
        }
    }
}

/// Segments for non-local locations: one crumb per URI path component.
fn remote_segments(file: &gio::File) -> Vec<(String, gio::File)> {
    let mut chain = Vec::new();
    let mut cur = Some(file.clone());
    while let Some(f) = cur {
        let label = if f.parent().is_none() {
            let uri = f.uri().to_string();
            match f.uri_scheme().as_deref() {
                Some("trash") => "Trash".to_string(),
                Some("recent") => "Recent".to_string(),
                _ => uri.trim_end_matches('/').to_string(),
            }
        } else {
            f.basename().map(|b| b.to_string_lossy().into_owned()).unwrap_or_default()
        };
        cur = f.parent();
        chain.push((label, f));
    }
    chain.reverse();
    chain
}
