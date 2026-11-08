// SPDX-License-Identifier: GPL-3.0-or-later
//! Embedded terminal panel (F4), like Dolphin's. It follows the current
//! folder, but only when the shell is idle (never types into a running
//! program), and `cd` inside it moves the file view along. The shell's
//! folder comes from its OSC 7 reports where the shell sends them, and
//! otherwise from /proc/<pid>/cwd whenever the shell is back at its prompt.

use std::cell::{Cell, RefCell};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use vte4::prelude::*;

type OnDir = Rc<dyn Fn(PathBuf)>;

pub struct Inner {
    root: gtk::Box,
    term: vte4::Terminal,
    shell: Cell<Option<i32>>,
    cwd: RefCell<Option<PathBuf>>,
    /// A `cd` we typed that the shell may not have run yet.
    pending: RefCell<Option<(PathBuf, Instant)>>,
    on_dir: RefCell<Option<OnDir>>,
}

#[derive(Clone)]
pub struct TerminalPanel(Rc<Inner>);

/// Quote for a POSIX shell: 'it'\''s'.
pub fn shell_quote(p: &Path) -> String {
    format!("'{}'", p.to_string_lossy().replace('\'', "'\\''"))
}

impl TerminalPanel {
    pub fn new() -> Self {
        let term = vte4::Terminal::builder().scrollback_lines(10_000).vexpand(true).hexpand(true).build();
        term.update_property(&[gtk::accessible::Property::Label("Terminal")]);
        let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["terminal-panel"]).height_request(140).visible(false).build();
        let css = gtk::CssProvider::new();
        css.load_from_string("vte-terminal { padding: 6px 10px; }");
        #[allow(deprecated)]
        term.style_context().add_provider(&css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        root.append(&term);
        let panel = TerminalPanel(Rc::new(Inner { root, term, shell: Cell::new(None), cwd: RefCell::new(None), pending: RefCell::new(None), on_dir: RefCell::new(None) }));
        panel.wire();
        panel.apply_colors();
        panel
    }

    pub fn widget(&self) -> gtk::Widget {
        self.0.root.clone().upcast()
    }

    pub fn is_open(&self) -> bool {
        self.0.root.is_visible()
    }

    pub fn connect_dir_changed(&self, f: impl Fn(PathBuf) + 'static) {
        *self.0.on_dir.borrow_mut() = Some(Rc::new(f));
    }

    fn wire(&self) {
        // Dropped files type their quoted paths, like other terminals.
        let drop = gtk::DropTarget::new(gtk::gdk::FileList::static_type(), gtk::gdk::DragAction::COPY);
        let t = self.0.term.downgrade();
        drop.connect_drop(move |_, value, _, _| {
            let (Some(t), Ok(list)) = (t.upgrade(), value.get::<gtk::gdk::FileList>()) else { return false };
            let paths: Vec<String> = list.files().iter().filter_map(|f| f.path()).map(|p| shell_quote(&p)).collect();
            if paths.is_empty() {
                return false;
            }
            t.feed_child(format!("{} ", paths.join(" ")).as_bytes());
            t.grab_focus();
            true
        });
        self.0.term.add_controller(drop);
        let weak = Rc::downgrade(&self.0);
        self.0.term.connect_child_exited(move |_, _| {
            if let Some(i) = weak.upgrade() {
                // The shell ended (e.g. `exit`): close; F4 starts a new one.
                i.shell.set(None);
                i.root.set_visible(false);
            }
        });
        let weak = Rc::downgrade(&self.0);
        self.0.term.connect_current_directory_uri_changed(move |t| {
            let Some(i) = weak.upgrade() else { return };
            if let Some(path) = t.current_directory_uri().and_then(|u| gio::File::for_uri(&u).path()) {
                TerminalPanel(i).shell_moved(path);
            }
        });
        // Output appeared: maybe a command finished and the folder changed.
        let weak = Rc::downgrade(&self.0);
        self.0.term.connect_contents_changed(move |_| {
            let Some(i) = weak.upgrade() else { return };
            let p = TerminalPanel(i);
            if let (Some(pid), true) = (p.0.shell.get(), p.shell_idle())
                && let Ok(path) = std::fs::read_link(format!("/proc/{pid}/cwd")) {
                    p.shell_moved(path);
                }
        });
    }

    fn shell_moved(&self, path: PathBuf) {
        let i = &self.0;
        // Ignore the old folder while our own `cd` is still on its way.
        let pending = i.pending.borrow().clone();
        if let Some((target, at)) = pending {
            if path == target || at.elapsed() > Duration::from_secs(2) {
                *i.pending.borrow_mut() = None;
            } else {
                return;
            }
        }
        if i.cwd.borrow().as_deref() == Some(path.as_path()) {
            return;
        }
        *i.cwd.borrow_mut() = Some(path.clone());
        let cb = i.on_dir.borrow().clone();
        if let Some(cb) = cb {
            cb(path);
        }
    }

    /// Type text into the shell (test hook).
    pub fn type_text(&self, text: &str) {
        self.0.term.feed_child(text.as_bytes());
    }

    /// Theme colours, so the terminal matches the window.
    pub fn apply_colors(&self) {
        let p = crate::theme_ui::palette();
        let rgba = |c: tuck_core::theme::Rgb| gdk::RGBA::new(c.r as f32, c.g as f32, c.b as f32, 1.0);
        self.0.term.set_colors(Some(&rgba(p.view_fg)), Some(&rgba(p.view_bg)), &[]);
        self.0.term.set_color_cursor(Some(&rgba(p.accent_color)));
    }

    pub fn toggle(&self, dir: Option<&Path>) {
        if self.is_open() {
            self.0.root.set_visible(false);
        } else {
            self.open(dir);
        }
    }

    pub fn open(&self, dir: Option<&Path>) {
        self.0.root.set_visible(true);
        if self.0.shell.get().is_none() {
            self.spawn(dir);
        } else if let Some(d) = dir {
            self.follow(d);
        }
        self.0.term.grab_focus();
    }

    fn spawn(&self, dir: Option<&Path>) {
        let shell = std::env::var("SHELL").ok().filter(|s| Path::new(s).exists()).unwrap_or_else(|| "/bin/sh".into());
        let cwd = dir.map(|d| d.to_string_lossy().into_owned());
        *self.0.cwd.borrow_mut() = dir.map(Path::to_path_buf);
        let weak = Rc::downgrade(&self.0);
        self.0.term.spawn_async(
            vte4::PtyFlags::DEFAULT,
            cwd.as_deref(),
            &[shell.as_str()],
            &[],
            glib::SpawnFlags::DEFAULT,
            || {},
            -1,
            gio::Cancellable::NONE,
            move |res| {
                if let (Some(i), Ok(pid)) = (weak.upgrade(), res) {
                    i.shell.set(Some(pid.0));
                }
            },
        );
    }

    /// Is the shell waiting at its prompt (no program in the foreground)?
    fn shell_idle(&self) -> bool {
        let (Some(pid), Some(pty)) = (self.0.shell.get(), self.0.term.pty()) else { return false };
        // SAFETY: tcgetpgrp on the terminal's own pty fd.
        let fg = unsafe { libc::tcgetpgrp(pty.fd().as_raw_fd()) };
        fg == pid
    }

    /// Follow the file view to `dir`, if the shell is idle.
    pub fn follow(&self, dir: &Path) {
        if !self.is_open() || self.0.cwd.borrow().as_deref() == Some(dir) || !self.shell_idle() {
            return;
        }
        *self.0.cwd.borrow_mut() = Some(dir.to_path_buf());
        *self.0.pending.borrow_mut() = Some((dir.to_path_buf(), Instant::now()));
        // Leading space keeps it out of shell history (HISTCONTROL=ignorespace).
        self.0.term.feed_child(format!(" cd -- {}\r", shell_quote(dir)).as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::shell_quote;
    use std::path::Path;

    #[test]
    fn quotes_paths_for_the_shell() {
        assert_eq!(shell_quote(Path::new("/home/u/My Files")), "'/home/u/My Files'");
        assert_eq!(shell_quote(Path::new("/tmp/it's")), "'/tmp/it'\\''s'");
        assert_eq!(shell_quote(Path::new("/a/$(rm -rf)")), "'/a/$(rm -rf)'");
    }
}
