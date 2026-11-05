// SPDX-License-Identifier: GPL-3.0-or-later
//! Thumbnail service for the UI: a small worker pool that serves the newest
//! requests first (what's on screen right now), plus an in-memory texture
//! cache. All disk and decoding work happens off the UI thread.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex};

use gtk::prelude::*;
use gtk::{gdk, glib};
use kivo_core::thumbs::{Outcome, Size, Thumbnails};

type Key = (PathBuf, i64, Size);
type Waiter = Box<dyn FnOnce(&gdk::Texture)>;

struct Queue {
    jobs: Mutex<VecDeque<(Key, String)>>,
    ready: Condvar,
}

struct Service {
    queue: Arc<Queue>,
    can: HashMap<String, bool>,
    checker: Thumbnails,
    cache: HashMap<Key, gdk::Texture>,
    order: VecDeque<Key>,
    waiting: HashMap<Key, Vec<Waiter>>,
}

const CACHE_LIMIT: usize = 600;
const WORKERS: usize = 4;

thread_local! {
    static SERVICE: RefCell<Option<Service>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Service) -> R) -> R {
    SERVICE.with(|s| {
        let mut s = s.borrow_mut();
        let svc = s.get_or_insert_with(start);
        f(svc)
    })
}

fn start() -> Service {
    let queue = Arc::new(Queue { jobs: Mutex::new(VecDeque::new()), ready: Condvar::new() });
    let (tx, rx) = async_channel::unbounded::<(Key, Option<PathBuf>)>();
    for i in 0..WORKERS {
        let q = queue.clone();
        let tx = tx.clone();
        std::thread::Builder::new()
            .name(format!("kivo-thumb-{i}"))
            .spawn(move || {
                let thumbs = Thumbnails::new();
                loop {
                    let job = {
                        let mut jobs = q.jobs.lock().expect("queue");
                        loop {
                            // Newest first: the user is looking at it now.
                            if let Some(j) = jobs.pop_back() {
                                break j;
                            }
                            jobs = q.ready.wait(jobs).expect("queue");
                        }
                    };
                    let (key, mime) = job;
                    let out = match thumbs.get(&key.0, &mime, key.2) {
                        Outcome::Ready(p) => Some(p),
                        // The system PDF thumbnailer is missing or broken:
                        // draw page 1 ourselves with Poppler.
                        _ if mime == "application/pdf" => crate::pdf::first_page_rgba(&key.0, key.2.pixels())
                            .and_then(|(w, h, px)| thumbs.store_rgba(&key.0, key.2, w, h, px).ok()),
                        _ => None,
                    };
                    if tx.send_blocking((key, out)).is_err() {
                        return;
                    }
                }
            })
            .expect("spawn thumbnail worker");
    }
    glib::MainContext::default().spawn_local(async move {
        while let Ok((key, path)) = rx.recv().await {
            let texture = path.and_then(|p| gdk::Texture::from_filename(&p).ok());
            let cbs = with(|s| {
                let cbs = s.waiting.remove(&key).unwrap_or_default();
                if let Some(t) = &texture {
                    s.cache.insert(key.clone(), t.clone());
                    s.order.push_back(key.clone());
                    while s.order.len() > CACHE_LIMIT {
                        if let Some(old) = s.order.pop_front() {
                            s.cache.remove(&old);
                        }
                    }
                }
                cbs
            });
            if let Some(t) = texture {
                for cb in cbs {
                    cb(&t);
                }
            }
        }
    });
    Service { queue, can: HashMap::new(), checker: Thumbnails::new(), cache: HashMap::new(), order: VecDeque::new(), waiting: HashMap::new() }
}

/// Can this type be thumbnailed at all? (Cached per MIME type.)
pub fn supported(mime: &str) -> bool {
    if mime == "application/pdf" {
        return true;
    }
    with(|s| {
        if let Some(&b) = s.can.get(mime) {
            return b;
        }
        let b = s.checker.can_thumbnail(mime);
        s.can.insert(mime.to_string(), b);
        b
    })
}

/// Ask for a thumbnail of `path` at least `px` pixels. `done` runs on the UI
/// thread if one becomes available; it may run immediately from the cache.
pub fn request(path: PathBuf, mtime: i64, mime: &str, px: u32, done: impl FnOnce(&gdk::Texture) + 'static) {
    if !supported(mime) {
        return;
    }
    let key = (path, mtime, Size::for_pixels(px));
    let cached = with(|s| s.cache.get(&key).cloned());
    if let Some(t) = cached {
        done(&t);
        return;
    }
    with(|s| {
        let first = !s.waiting.contains_key(&key);
        s.waiting.entry(key.clone()).or_default().push(Box::new(done));
        if first {
            s.queue.jobs.lock().expect("queue").push_back((key, mime.to_string()));
            s.queue.ready.notify_one();
        }
    });
}

/// Set a thumbnail on `image` for `item`, unless the cell has been reused
/// for another item by the time it arrives.
pub fn apply(image: &gtk::Image, item: &crate::item::FileItem, px: u32) {
    let Some(path) = item.path() else { return };
    if item.is_dir() {
        return;
    }
    let mtime = item.meta().modified;
    let mime = item.content_type();
    let token = Rc::new(());
    // SAFETY: the key is only read back as Rc<()> here.
    unsafe { image.set_data("kivo-thumb-token", token.clone()) };
    let weak_token = Rc::downgrade(&token);
    let img = image.downgrade();
    request(path, mtime, &mime, px, move |tex| {
        let Some(img) = img.upgrade() else { return };
        // SAFETY: as above.
        let current = unsafe { img.data::<Rc<()>>("kivo-thumb-token") }.map(|p| unsafe { p.as_ref() }.clone());
        let still_ours = match (current, weak_token.upgrade()) {
            (Some(a), Some(b)) => Rc::ptr_eq(&a, &b),
            _ => false,
        };
        if still_ours {
            img.set_paintable(Some(tex));
            img.add_css_class("thumbnail");
        }
    });
}

/// Forget any pending thumbnail for a recycled cell.
pub fn reset(image: &gtk::Image) {
    // SAFETY: only ever Rc<()> under this key.
    unsafe {
        image.steal_data::<Rc<()>>("kivo-thumb-token");
    }
    image.remove_css_class("thumbnail");
}
