// SPDX-License-Identifier: GPL-3.0-or-later
//! Background jobs: run a core operation on a worker thread, show its progress
//! in the header's job list, let the user cancel it, and ask about conflicts
//! on the UI thread while the worker waits.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc;

use adw::prelude::*;
use gtk::glib;
use kivo_core::format;
use kivo_core::ops::{Cancel, ConflictInfo, Observer, Progress, Resolution};

enum Msg {
    Progress(Progress),
    Conflict(ConflictInfo, mpsc::Sender<(Resolution, bool)>),
}

/// Forwards core callbacks to the UI thread.
struct ChannelObserver {
    tx: async_channel::Sender<Msg>,
}

impl Observer for ChannelObserver {
    fn progress(&mut self, p: &Progress) {
        let _ = self.tx.try_send(Msg::Progress(p.clone()));
    }

    fn conflict(&mut self, c: &ConflictInfo) -> (Resolution, bool) {
        let (rtx, rrx) = mpsc::channel();
        if self.tx.send_blocking(Msg::Conflict(c.clone(), rtx)).is_err() {
            return (Resolution::Cancel, true);
        }
        // The worker blocks here until the user answers.
        rrx.recv().unwrap_or((Resolution::Cancel, true))
    }
}

pub struct Inner {
    button: gtk::MenuButton,
    list: gtk::Box,
    running: Cell<usize>,
    cancels: std::cell::RefCell<Vec<Cancel>>,
    idle_waiters: std::cell::RefCell<Vec<Box<dyn FnOnce()>>>,
}

/// The job list shown from the header bar.
#[derive(Clone)]
pub struct Jobs(Rc<Inner>);

impl Jobs {
    pub fn new() -> Self {
        let list = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .css_classes(["job-list"])
            .width_request(320)
            .build();
        let popover = gtk::Popover::builder().child(&list).build();
        let spinner = adw::Spinner::new();
        let button = gtk::MenuButton::builder()
            .child(&spinner)
            .popover(&popover)
            .tooltip_text("File Operations")
            .visible(false)
            .build();
        button.update_property(&[gtk::accessible::Property::Label("File Operations")]);
        Jobs(Rc::new(Inner { button, list, running: Cell::new(0), cancels: Default::default(), idle_waiters: Default::default() }))
    }

    pub fn widget(&self) -> gtk::Widget {
        self.0.button.clone().upcast()
    }

    pub fn is_idle(&self) -> bool {
        self.0.running.get() == 0
    }

    /// Cancel every running job; `then` runs once they've all cleaned up.
    pub fn cancel_all(&self, then: impl FnOnce() + 'static) {
        for c in self.0.cancels.borrow().iter() {
            c.cancel();
        }
        if self.is_idle() {
            then();
        } else {
            self.0.idle_waiters.borrow_mut().push(Box::new(then));
        }
    }

    /// Open the job list (test hooks), returning its popover.
    pub fn popup(&self) -> Option<gtk::Popover> {
        if !self.0.button.is_visible() {
            return None;
        }
        self.0.button.popup();
        self.0.button.popover()
    }

    /// Run `work` on a worker thread; `done` gets its result on the UI thread.
    /// `parent` is used for conflict dialogs.
    pub fn run<T: Send + 'static>(
        &self,
        parent: &gtk::Widget,
        title: &str,
        work: impl FnOnce(&mut dyn Observer, &Cancel) -> T + Send + 'static,
        done: impl FnOnce(T) + 'static,
    ) -> Cancel {
        let cancel = Cancel::new();
        let (tx, rx) = async_channel::unbounded::<Msg>();
        let (rtx, rrx) = async_channel::bounded::<T>(1);
        let c = cancel.clone();
        std::thread::Builder::new()
            .name("kivo-job".into())
            .spawn(move || {
                let mut obs = ChannelObserver { tx };
                let result = work(&mut obs, &c);
                let _ = rtx.send_blocking(result);
            })
            .expect("spawn job thread");

        let row = JobRow::new(title, cancel.clone());
        self.0.running.set(self.0.running.get() + 1);
        self.0.cancels.borrow_mut().push(cancel.clone());
        let my_cancel = cancel.clone();

        // Show the job only if it takes noticeable time, to avoid flicker.
        let shown = Rc::new(Cell::new(false));
        {
            let jobs = self.clone();
            let row = row.clone();
            let shown = shown.clone();
            let rrx = rrx.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
                if rrx.is_empty() && !rrx.is_closed() {
                    jobs.0.list.append(&row.root);
                    jobs.0.button.set_visible(true);
                    shown.set(true);
                }
            });
        }

        let jobs = self.clone();
        let parent = parent.clone();
        glib::MainContext::default().spawn_local(async move {
            // Pump messages until the worker finishes.
            loop {
                let msg = futures_select(&rx, &rrx).await;
                match msg {
                    Event::Msg(Msg::Progress(p)) => row.update(&p),
                    Event::Msg(Msg::Conflict(info, reply)) => {
                        let answer = crate::dialogs::ask_conflict(&parent, &info).await;
                        let _ = reply.send(answer);
                    }
                    Event::Done(result) => {
                        if shown.get() {
                            jobs.0.list.remove(&row.root);
                        }
                        jobs.0.running.set(jobs.0.running.get() - 1);
                        jobs.0.cancels.borrow_mut().retain(|c| !std::sync::Arc::ptr_eq(c.inner(), my_cancel.inner()));
                        if jobs.0.running.get() == 0 {
                            jobs.0.button.set_visible(false);
                            jobs.0.button.popdown();
                        }
                        done(result);
                        if jobs.is_idle() {
                            let waiters: Vec<_> = jobs.0.idle_waiters.borrow_mut().drain(..).collect();
                            for w in waiters {
                                w();
                            }
                        }
                        return;
                    }
                }
            }
        });
        cancel
    }
}

enum Event<T> {
    Msg(Msg),
    Done(T),
}

/// Wait for whichever comes first: a message, or the result. Messages are
/// drained before the result so the last progress update isn't lost.
async fn futures_select<T>(rx: &async_channel::Receiver<Msg>, done: &async_channel::Receiver<T>) -> Event<T> {
    loop {
        if let Ok(m) = rx.try_recv() {
            return Event::Msg(m);
        }
        if let Ok(r) = done.try_recv() {
            return Event::Done(r);
        }
        // Sleep until either channel has something.
        let a = rx.recv();
        let b = done.recv();
        let a = std::pin::pin!(a);
        let b = std::pin::pin!(b);
        match futures_either(a, b).await {
            Either::Left(Ok(m)) => return Event::Msg(m),
            Either::Right(Ok(r)) => return Event::Done(r),
            // A closed message channel just means the worker is finishing.
            Either::Left(Err(_)) => {
                if let Ok(r) = done.recv().await {
                    return Event::Done(r);
                }
            }
            Either::Right(Err(_)) => {
                // Worker died without a result (panic). Nothing to deliver.
                std::future::pending::<()>().await;
            }
        }
    }
}

enum Either<A, B> {
    Left(A),
    Right(B),
}

async fn futures_either<A: std::future::Future, B: std::future::Future>(
    mut a: std::pin::Pin<&mut A>,
    mut b: std::pin::Pin<&mut B>,
) -> Either<A::Output, B::Output> {
    std::future::poll_fn(move |cx| {
        if let std::task::Poll::Ready(v) = a.as_mut().poll(cx) {
            return std::task::Poll::Ready(Either::Left(v));
        }
        if let std::task::Poll::Ready(v) = b.as_mut().poll(cx) {
            return std::task::Poll::Ready(Either::Right(v));
        }
        std::task::Poll::Pending
    })
    .await
}

#[derive(Clone)]
struct JobRow {
    root: gtk::Box,
    bar: gtk::ProgressBar,
    detail: gtk::Label,
}

impl JobRow {
    fn new(title: &str, cancel: Cancel) -> Self {
        let label = gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .hexpand(true)
            .css_classes(["job-title"])
            .build();
        let stop = gtk::Button::builder()
            .icon_name("process-stop-symbolic")
            .tooltip_text("Cancel")
            .css_classes(["flat", "circular"])
            .valign(gtk::Align::Center)
            .build();
        stop.update_property(&[gtk::accessible::Property::Label(&format!("Cancel {title}"))]);
        stop.connect_clicked(move |b| {
            cancel.cancel();
            b.set_sensitive(false);
        });
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        top.append(&label);
        top.append(&stop);
        let bar = gtk::ProgressBar::new();
        bar.update_property(&[gtk::accessible::Property::Label(title)]);
        let detail = gtk::Label::builder().xalign(0.0).css_classes(["job-detail", "numeric"]).label("Preparing…").build();
        let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
        root.append(&top);
        root.append(&bar);
        root.append(&detail);
        JobRow { root, bar, detail }
    }

    fn update(&self, p: &Progress) {
        self.bar.set_fraction(p.fraction());
        let text = if p.bytes_total > 0 {
            format!(
                "{} of {} · {} of {} items",
                format::size(p.bytes_done),
                format::size(p.bytes_total),
                p.files_done.min(p.files_total),
                p.files_total
            )
        } else {
            format!("{} of {} items", p.files_done.min(p.files_total), p.files_total)
        };
        self.detail.set_label(&text);
    }
}
