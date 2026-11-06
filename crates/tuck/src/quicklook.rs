// SPDX-License-Identifier: GPL-3.0-or-later
//! Quick Look: Space previews the selected item; Space or Esc closes it;
//! arrow keys move through the folder while it stays open.
//!
//! The window is built once and reused, and shows the item's icon at once;
//! anything heavy (decoding, rendering, reading) happens on a worker thread
//! and is dropped if the user has already moved on.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use tuck_core::preview::{self, Block, Kind, Span};
use tuck_core::{format, paths};
use sourceview5::prelude::*;

use crate::item::FileItem;

type Navigate = Rc<dyn Fn(i32) -> Option<FileItem>>;
type OpenFn = Rc<dyn Fn(FileItem, bool)>;
type ExtractFn = Rc<dyn Fn(PathBuf)>;
/// (width, height, RGBA pixels, delay in ms)
type GifFrame = (u32, u32, Vec<u8>, u32);

pub struct Inner {
    win: adw::Window,
    title: adw::WindowTitle,
    content: adw::Bin,
    open_btn: gtk::Button,
    open_with_btn: gtk::Button,
    extract_btn: gtk::Button,
    install_btn: gtk::Button,
    current: RefCell<Option<FileItem>>,
    generation: Cell<u64>,
    anim: RefCell<Option<glib::SourceId>>,
    media: RefCell<Option<gtk::MediaStream>>,
    navigate: RefCell<Option<Navigate>>,
    on_open: RefCell<Option<OpenFn>>,
    on_extract: RefCell<Option<ExtractFn>>,
    on_install: RefCell<Option<ExtractFn>>,
    on_hidden: RefCell<Option<Rc<dyn Fn()>>>,
}

#[derive(Clone)]
pub struct QuickLook(Rc<Inner>);

/// Run `work` on a worker thread and hand its result to `done` on the UI
/// thread, unless the preview moved on in the meantime.
fn background<T: Send + 'static>(ql: &QuickLook, work: impl FnOnce() -> T + Send + 'static, done: impl FnOnce(&QuickLook, T) + 'static) {
    let gen_ = ql.0.generation.get();
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(work());
    });
    let weak = Rc::downgrade(&ql.0);
    glib::MainContext::default().spawn_local(async move {
        let Ok(v) = rx.recv().await else { return };
        let Some(inner) = weak.upgrade() else { return };
        let ql = QuickLook(inner);
        if ql.0.generation.get() == gen_ {
            done(&ql, v);
        }
    });
}

impl QuickLook {
    pub fn new(parent: &impl IsA<gtk::Window>) -> Self {
        let title = adw::WindowTitle::new("Quick Look", "");
        let open_btn = gtk::Button::builder().label("Open").css_classes(["suggested-action"]).build();
        let open_with_btn = gtk::Button::builder().icon_name("tuck-more-symbolic").tooltip_text("Open With…").build();
        open_with_btn.update_property(&[gtk::accessible::Property::Label("Open With…")]);
        let extract_btn = gtk::Button::builder().label("Extract").visible(false).build();
        let install_btn = gtk::Button::builder().label("Install…").visible(false).css_classes(["suggested-action"]).build();
        let open_box = gtk::Box::builder().css_classes(["linked"]).build();
        open_box.append(&open_btn);
        open_box.append(&open_with_btn);
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        header.pack_start(&open_box);
        header.pack_end(&extract_btn);
        header.pack_end(&install_btn);
        let content = adw::Bin::builder().vexpand(true).hexpand(true).css_classes(["quicklook-content"]).build();
        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&header);
        tv.set_content(Some(&content));
        let win = adw::Window::builder()
            .transient_for(parent)
            .default_width(960)
            .default_height(680)
            .width_request(360)
            .height_request(300)
            .hide_on_close(true)
            .title("Quick Look")
            .content(&tv)
            .build();
        // Add, don't replace: the window's default `background` class is
        // what paints it (without it the area behind the header shows the
        // desktop).
        win.add_css_class("quicklook");

        let ql = QuickLook(Rc::new(Inner {
            win,
            title,
            content,
            open_btn,
            open_with_btn,
            extract_btn,
            install_btn,
            current: RefCell::new(None),
            generation: Cell::new(0),
            anim: RefCell::new(None),
            media: RefCell::new(None),
            navigate: RefCell::new(None),
            on_open: RefCell::new(None),
            on_extract: RefCell::new(None),
            on_install: RefCell::new(None),
            on_hidden: RefCell::new(None),
        }));
        ql.wire();
        ql
    }

    pub fn connect(&self, navigate: impl Fn(i32) -> Option<FileItem> + 'static, open: impl Fn(FileItem, bool) + 'static, extract: impl Fn(PathBuf) + 'static, hidden: impl Fn() + 'static) {
        *self.0.navigate.borrow_mut() = Some(Rc::new(navigate));
        *self.0.on_open.borrow_mut() = Some(Rc::new(open));
        *self.0.on_extract.borrow_mut() = Some(Rc::new(extract));
        *self.0.on_hidden.borrow_mut() = Some(Rc::new(hidden));
    }

    pub fn set_install(&self, f: impl Fn(PathBuf) + 'static) {
        *self.0.on_install.borrow_mut() = Some(Rc::new(f));
    }

    pub fn window(&self) -> adw::Window {
        self.0.win.clone()
    }

    pub fn is_visible(&self) -> bool {
        self.0.win.is_visible()
    }

    pub fn current(&self) -> Option<FileItem> {
        self.0.current.borrow().clone()
    }

    fn wire(&self) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&self.0);
        keys.connect_key_pressed(move |_, key, _, state| {
            let Some(inner) = weak.upgrade() else { return glib::Propagation::Proceed };
            let ql = QuickLook(inner);
            let mods = state & gtk::accelerator_get_default_mod_mask();
            if !mods.is_empty() {
                return glib::Propagation::Proceed;
            }
            match key {
                gdk::Key::space | gdk::Key::Escape => ql.hide(),
                gdk::Key::Left | gdk::Key::Up => ql.step(-1),
                gdk::Key::Right | gdk::Key::Down => ql.step(1),
                gdk::Key::Return | gdk::Key::KP_Enter => ql.open(false),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        self.0.win.add_controller(keys);

        let weak = Rc::downgrade(&self.0);
        self.0.open_btn.connect_clicked(move |_| {
            if let Some(i) = weak.upgrade() {
                QuickLook(i).open(false);
            }
        });
        let weak = Rc::downgrade(&self.0);
        self.0.open_with_btn.connect_clicked(move |_| {
            if let Some(i) = weak.upgrade() {
                QuickLook(i).open(true);
            }
        });
        let weak = Rc::downgrade(&self.0);
        self.0.extract_btn.connect_clicked(move |_| {
            let Some(i) = weak.upgrade() else { return };
            let ql = QuickLook(i);
            let path = ql.current().and_then(|c| c.path());
            let cb = ql.0.on_extract.borrow().clone();
            if let (Some(p), Some(cb)) = (path, cb) {
                ql.hide();
                cb(p);
            }
        });
        let weak = Rc::downgrade(&self.0);
        self.0.install_btn.connect_clicked(move |_| {
            let Some(i) = weak.upgrade() else { return };
            let ql = QuickLook(i);
            let path = ql.current().and_then(|c| c.path());
            let cb = ql.0.on_install.borrow().clone();
            if let (Some(p), Some(cb)) = (path, cb) {
                ql.hide();
                cb(p);
            }
        });
        let weak = Rc::downgrade(&self.0);
        self.0.win.connect_hide(move |_| {
            if let Some(i) = weak.upgrade() {
                let ql = QuickLook(i);
                ql.stop();
                let cb = ql.0.on_hidden.borrow().clone();
                if let Some(cb) = cb {
                    cb();
                }
            }
        });
    }

    fn open(&self, choose: bool) {
        let item = self.current();
        let cb = self.0.on_open.borrow().clone();
        if let (Some(item), Some(cb)) = (item, cb) {
            self.hide();
            cb(item, choose);
        }
    }

    pub(crate) fn step(&self, delta: i32) {
        let nav = self.0.navigate.borrow().clone();
        if let Some(item) = nav.and_then(|n| n(delta)) {
            self.show(&item);
        }
    }

    pub fn hide(&self) {
        self.0.win.set_visible(false);
    }

    /// Stop animations and playback, forget pending loads.
    fn stop(&self) {
        self.0.generation.set(self.0.generation.get() + 1);
        if let Some(id) = self.0.anim.borrow_mut().take() {
            id.remove();
        }
        if let Some(m) = self.0.media.borrow_mut().take() {
            m.pause();
        }
    }

    pub fn toggle(&self, item: Option<FileItem>) {
        if self.is_visible() {
            self.hide();
        } else if let Some(i) = item {
            self.show(&i);
        }
    }

    pub fn show(&self, item: &FileItem) {
        let same = self.current().as_ref() == Some(item) && self.is_visible();
        if same {
            return;
        }
        self.stop();
        *self.0.current.borrow_mut() = Some(item.clone());
        let meta = item.meta().clone();
        let path = item.path();
        let kind = match &path {
            Some(p) => preview::classify(p, &item.content_type(), item.is_dir()),
            None if item.is_dir() => Kind::Folder,
            None => Kind::Unsupported,
        };

        // Header: name, kind and size; buttons that make sense here.
        let subtitle = if item.is_dir() {
            meta.type_name.clone()
        } else {
            format!("{} · {}", if meta.type_name.is_empty() { "File" } else { &meta.type_name }, format::size(meta.size))
        };
        self.0.title.set_title(&meta.name);
        self.0.title.set_subtitle(&subtitle);
        self.0.win.set_title(Some(&format!("{} — Quick Look", meta.name)));
        let app = (!item.is_dir())
            .then(|| gio::AppInfo::default_for_type(&item.content_type(), false))
            .flatten()
            .map(|a| a.display_name().to_string());
        self.0.open_btn.set_label(&match app {
            Some(a) => format!("Open with {a}"),
            None => "Open".into(),
        });
        self.0.open_with_btn.set_visible(!item.is_dir());
        self.0.extract_btn.set_visible(kind == Kind::Archive);
        let installable = kind == Kind::Package && path.as_ref().is_some_and(|p| tuck_core::install::is_installable(p));
        self.0.install_btn.set_visible(installable);

        // Show something immediately; replace it when the real preview is ready.
        self.0.content.set_child(Some(&placeholder(item, false)));
        if !self.is_visible() {
            self.0.win.present();
        }

        let Some(path) = path else { return };
        match kind {
            Kind::Image | Kind::Svg => self.image(path),
            Kind::Gif => self.gif(path),
            Kind::Video => self.media(item, true),
            Kind::Audio => self.media(item, false),
            Kind::Pdf => self.pdf(path),
            Kind::Text => self.text(path, item),
            Kind::Markdown => self.markdown(path),
            Kind::Archive => self.archive(path),
            Kind::Folder => self.folder(path, item),
            Kind::Package | Kind::Unsupported => self.0.content.set_child(Some(&placeholder(item, true))),
        }
    }

    // ------------------------------------------------------------ images

    fn image(&self, path: PathBuf) {
        background(
            self,
            // GDK's image loaders are thread-safe, so even a 50-megapixel
            // photo decodes here, not on the UI thread.
            move || gdk::Texture::from_filename(&path).ok(),
            |ql, texture| match texture {
                Some(t) => ql.0.content.set_child(Some(&picture(&t))),
                None => {
                    if let Some(i) = ql.current() {
                        let page = placeholder(&i, true);
                        if let Some(b) = page.downcast_ref::<gtk::Box>() {
                            b.append(&note("This image couldn't be decoded. It may be damaged, or in a format this system can't read."));
                        }
                        ql.0.content.set_child(Some(&page));
                    }
                }
            },
        );
    }

    fn gif(&self, path: PathBuf) {
        background(
            self,
            move || -> Option<Vec<GifFrame>> {
                use image::AnimationDecoder;
                let f = std::io::BufReader::new(std::fs::File::open(&path).ok()?);
                let dec = image::codecs::gif::GifDecoder::new(f).ok()?;
                let mut frames = Vec::new();
                let mut bytes = 0usize;
                for fr in dec.into_frames() {
                    let fr = fr.ok()?;
                    let (n, d) = fr.delay().numer_denom_ms();
                    let delay = n.checked_div(d).map_or(100, |v| v.max(20));
                    let buf = fr.into_buffer();
                    bytes += buf.len();
                    frames.push((buf.width(), buf.height(), buf.into_raw(), delay));
                    // Huge animations: show what fits in memory.
                    if frames.len() >= 500 || bytes > 300 * 1024 * 1024 {
                        break;
                    }
                }
                Some(frames)
            },
            |ql, frames| {
                let Some(frames) = frames.filter(|f| !f.is_empty()) else { return };
                let textures: Vec<(gdk::Texture, u32)> = frames
                    .into_iter()
                    .map(|(w, h, px, d)| {
                        let t: gdk::Texture = gdk::MemoryTexture::new(w as i32, h as i32, gdk::MemoryFormat::R8g8b8a8, &glib::Bytes::from_owned(px), (w * 4) as usize).upcast();
                        (t, d)
                    })
                    .collect();
                let pic = picture(&textures[0].0);
                ql.0.content.set_child(Some(&pic));
                // Respect "reduce motion": show the first frame only.
                let animations = gtk::Settings::default().is_none_or(|s| s.is_gtk_enable_animations());
                if textures.len() > 1 && animations
                    && let Ok(p) = pic.downcast::<gtk::Picture>() {
                        animate(ql, p, Rc::new(textures), 0);
                    }
            },
        );
    }

    // ------------------------------------------------------- video/audio

    fn media(&self, item: &FileItem, video: bool) {
        let file = item.target();
        let media = gtk::MediaFile::for_file(&file);
        media.set_loop(false);
        let stream: gtk::MediaStream = media.clone().upcast();
        let child: gtk::Widget = if video {
            let v = gtk::Video::builder().media_stream(&stream).autoplay(true).vexpand(true).hexpand(true).build();
            // autoplay only applies to files GtkVideo opens itself.
            stream.play();
            v.upcast()
        } else {
            let b = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(18).valign(gtk::Align::Center).halign(gtk::Align::Center).width_request(420).build();
            b.append(&gtk::Image::builder().gicon(&item.icon()).pixel_size(160).build());
            b.append(&gtk::Label::builder().label(item.name()).css_classes(["title-2"]).wrap(true).build());
            b.append(&gtk::MediaControls::new(Some(&stream)));
            stream.play();
            b.upcast()
        };
        // Explain playback failures (usually missing GStreamer plugins).
        let weak = Rc::downgrade(&self.0);
        let gen_ = self.0.generation.get();
        let it = item.clone();
        stream.connect_error_notify(move |s| {
            let Some(inner) = weak.upgrade() else { return };
            let ql = QuickLook(inner);
            if ql.0.generation.get() != gen_ || s.error().is_none() {
                return;
            }
            let page = placeholder(&it, true);
            if let Some(b) = page.downcast_ref::<gtk::Box>() {
                b.append(&note("This can't be played here. Installing the GStreamer plugins for this format (for example gstreamer1.0-libav) usually fixes it."));
            }
            ql.0.content.set_child(Some(&page));
        });
        *self.0.media.borrow_mut() = Some(stream);
        self.0.content.set_child(Some(&child));
    }

    // --------------------------------------------------------------- pdf

    fn pdf(&self, path: PathBuf) {
        const MAX_PAGES: i32 = 12;
        let scroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();
        let col = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(18).margin_top(18).margin_bottom(18).margin_start(18).margin_end(18).build();
        scroll.set_child(Some(&col));
        let shown = Rc::new(Cell::new(false));
        // Pages arrive one by one; the first replaces the placeholder.
        let (tx, rx) = async_channel::unbounded::<(i32, i32, crate::pdf::Page)>();
        let p = path.clone();
        std::thread::spawn(move || {
            let n = crate::pdf::page_count(&p).unwrap_or(0);
            for i in 0..n.min(MAX_PAGES) {
                match crate::pdf::render(&p, i, 1400) {
                    Some(page) => {
                        if tx.send_blocking((i, n, page)).is_err() {
                            return;
                        }
                    }
                    None => return,
                }
            }
        });
        let weak = Rc::downgrade(&self.0);
        let gen_ = self.0.generation.get();
        glib::MainContext::default().spawn_local(async move {
            while let Ok((i, n, page)) = rx.recv().await {
                let Some(inner) = weak.upgrade() else { return };
                let ql = QuickLook(inner);
                if ql.0.generation.get() != gen_ {
                    return;
                }
                let pic = gtk::Picture::for_paintable(&page.texture());
                pic.set_can_shrink(true);
                pic.set_content_fit(gtk::ContentFit::Contain);
                pic.add_css_class("pdf-page");
                pic.update_property(&[gtk::accessible::Property::Label(&format!("Page {} of {n}", i + 1))]);
                col.append(&pic);
                if !shown.get() {
                    shown.set(true);
                    ql.0.content.set_child(Some(&scroll));
                }
                if i + 1 == MAX_PAGES.min(n) && n > MAX_PAGES {
                    col.append(&note(&format!("Showing the first {MAX_PAGES} of {n} pages. Open the document to see the rest.")));
                }
            }
        });
    }

    // -------------------------------------------------------------- text

    fn text(&self, path: PathBuf, item: &FileItem) {
        const MAX: usize = 2 * 1024 * 1024;
        let name = item.name();
        let ct = item.content_type();
        background(
            self,
            move || preview::read_text(&path, MAX).ok(),
            move |ql, res| {
                let Some((text, cut)) = res else { return };
                let buffer = sourceview5::Buffer::new(None);
                buffer.set_text(&text);
                let lm = sourceview5::LanguageManager::default();
                if let Some(lang) = lm.guess_language(Some(name.as_str()), Some(ct.as_str())) {
                    buffer.set_language(Some(&lang));
                    buffer.set_highlight_syntax(true);
                }
                let dark = adw::StyleManager::default().is_dark();
                let schemes = sourceview5::StyleSchemeManager::default();
                if let Some(s) = schemes.scheme(if dark { "Adwaita-dark" } else { "Adwaita" }) {
                    buffer.set_style_scheme(Some(&s));
                }
                let view = sourceview5::View::with_buffer(&buffer);
                view.set_editable(false);
                view.set_cursor_visible(false);
                view.set_monospace(true);
                view.set_show_line_numbers(true);
                view.set_top_margin(12);
                view.set_bottom_margin(12);
                view.set_left_margin(8);
                view.upcast_ref::<gtk::Widget>().update_property(&[gtk::accessible::Property::Label(&format!("Contents of {name}"))]);
                let scroll = gtk::ScrolledWindow::builder().child(&view).vexpand(true).build();
                let b = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
                b.append(&scroll);
                if cut {
                    b.append(&note("Showing the first 2 MB. Open the file to see all of it."));
                }
                ql.0.content.set_child(Some(&b));
            },
        );
    }

    fn markdown(&self, path: PathBuf) {
        background(
            self,
            move || preview::read_text(&path, 2 * 1024 * 1024).ok().map(|(t, _)| preview::markdown(&t)),
            |ql, blocks| {
                let Some(blocks) = blocks else { return };
                let view = render_markdown(&blocks);
                let scroll = gtk::ScrolledWindow::builder().child(&view).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build();
                ql.0.content.set_child(Some(&scroll));
            },
        );
    }

    // ----------------------------------------------------------- archive

    fn archive(&self, path: PathBuf) {
        background(
            self,
            move || tuck_core::archive::list(&path, 2000, Duration::from_secs(3)),
            |ql, res| {
                let page = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).margin_top(18).margin_start(24).margin_end(24).margin_bottom(18).build();
                match res {
                    Ok(l) => {
                        let mut summary = format!(
                            "{} · {}{} · {} when extracted",
                            l.format.map(|f| f.label()).unwrap_or("Archive"),
                            format::item_count(l.entries.len() as u64),
                            if l.truncated { "+" } else { "" },
                            format::size(l.total)
                        );
                        if l.encrypted {
                            summary.push_str(" · password-protected");
                        }
                        page.append(&gtk::Label::builder().label(summary).xalign(0.0).wrap(true).css_classes(["heading"]).build());
                        if l.entries.is_empty() && l.encrypted {
                            page.append(&note("The file names are encrypted too. Extract it with the password to see the contents."));
                        }
                        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).css_classes(["boxed-list"]).build();
                        list.update_property(&[gtk::accessible::Property::Label("Archive contents")]);
                        for e in l.entries.iter().take(1000) {
                            let icon = if e.is_dir { "tuck-folder-symbolic" } else { "tuck-file-symbolic" };
                            let row = adw::ActionRow::builder().title(glib::markup_escape_text(&e.name)).build();
                            row.add_prefix(&gtk::Image::from_icon_name(icon));
                            if !e.is_dir && e.size > 0 {
                                row.add_suffix(&gtk::Label::builder().label(format::size(e.size)).css_classes(["dim-label", "numeric"]).build());
                            }
                            list.append(&row);
                        }
                        page.append(&list);
                        if l.truncated || l.entries.len() > 1000 {
                            page.append(&note("Only part of the contents is listed."));
                        }
                    }
                    Err(e) => {
                        page.append(&note(&format!("{} {}", e.message("This archive"), e.hint())));
                    }
                }
                let scroll = gtk::ScrolledWindow::builder().child(&page).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build();
                ql.0.content.set_child(Some(&scroll));
            },
        );
    }

    // ------------------------------------------------------------ folder

    fn folder(&self, path: PathBuf, item: &FileItem) {
        let it = item.clone();
        background(
            self,
            move || {
                let mut names: Vec<(String, bool)> = std::fs::read_dir(&path)
                    .map(|rd| rd.flatten().map(|e| (e.file_name().to_string_lossy().into_owned(), e.path().is_dir())).collect())
                    .unwrap_or_default();
                names.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| tuck_core::sort::natural_cmp(&a.0, &b.0)));
                let (bytes, _) = tuck_core::ops::measure(&path, &tuck_core::ops::Cancel::new());
                (names, bytes)
            },
            move |ql, (names, bytes)| {
                let page = placeholder(&it, false);
                if let Some(b) = page.downcast_ref::<gtk::Box>() {
                    b.append(&gtk::Label::builder().label(format!("{} · {}", format::item_count(names.len() as u64), format::size(bytes))).css_classes(["dim-label"]).build());
                    let flow = gtk::FlowBox::builder().selection_mode(gtk::SelectionMode::None).max_children_per_line(6).column_spacing(12).row_spacing(12).margin_top(12).build();
                    for (n, is_dir) in names.iter().take(24) {
                        let cell = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).width_request(96).build();
                        let icon = crate::theme_ui::file_icon(n, *is_dir, None);
                        cell.append(&gtk::Image::builder().gicon(&icon).pixel_size(48).build());
                        cell.append(&gtk::Label::builder().label(n).ellipsize(gtk::pango::EllipsizeMode::Middle).max_width_chars(12).css_classes(["caption"]).build());
                        flow.append(&cell);
                    }
                    b.append(&flow);
                    if names.len() > 24 {
                        b.append(&note(&format!("…and {} more", names.len() - 24)));
                    }
                }
                let scroll = gtk::ScrolledWindow::builder().child(&page).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build();
                ql.0.content.set_child(Some(&scroll));
            },
        );
    }
}

fn animate(ql: &QuickLook, pic: gtk::Picture, frames: Rc<Vec<(gdk::Texture, u32)>>, i: usize) {
    let delay = frames[i].1;
    let weak = Rc::downgrade(&ql.0);
    let id = glib::timeout_add_local_once(Duration::from_millis(delay as u64), move || {
        let Some(inner) = weak.upgrade() else { return };
        let ql = QuickLook(inner);
        ql.0.anim.borrow_mut().take();
        let next = (i + 1) % frames.len();
        pic.set_paintable(Some(&frames[next].0));
        animate(&ql, pic, frames, next);
    });
    *ql.0.anim.borrow_mut() = Some(id);
}

fn picture(t: &gdk::Texture) -> gtk::Widget {
    let pic = gtk::Picture::for_paintable(t);
    pic.set_can_shrink(true);
    pic.set_content_fit(gtk::ContentFit::Contain);
    pic.set_margin_top(12);
    pic.set_margin_bottom(12);
    pic.set_margin_start(12);
    pic.set_margin_end(12);
    pic.add_css_class("ql-picture");
    pic.upcast()
}

fn note(text: &str) -> gtk::Label {
    gtk::Label::builder().label(text).wrap(true).justify(gtk::Justification::Center).css_classes(["dim-label", "ql-note"]).margin_top(6).margin_bottom(6).build()
}

/// Large icon with the item's details: the instant placeholder, and the
/// final preview for types we can't show (with `details`).
fn placeholder(item: &FileItem, details: bool) -> gtk::Widget {
    let b = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).valign(gtk::Align::Center).halign(gtk::Align::Center).margin_top(24).margin_bottom(24).margin_start(24).margin_end(24).build();
    let img = gtk::Image::builder().gicon(&item.icon()).pixel_size(192).build();
    crate::thumbs_ui::apply(&img, item, 512);
    b.append(&img);
    if details {
        let meta = item.meta().clone();
        b.append(&gtk::Label::builder().label(&meta.name).wrap(true).justify(gtk::Justification::Center).css_classes(["title-2"]).build());
        b.append(&gtk::Label::builder().label(if meta.type_name.is_empty() { "Unknown kind".into() } else { meta.type_name.clone() }).css_classes(["dim-label"]).build());
        let grid = gtk::Grid::builder().row_spacing(6).column_spacing(16).halign(gtk::Align::Center).margin_top(12).build();
        let mut rows: Vec<(&str, String)> = Vec::new();
        if !item.is_dir() {
            rows.push(("Size", format::size(meta.size)));
        }
        rows.push(("Modified", format::date_full(meta.modified)));
        if let Some(p) = item.file().parent().and_then(|p| p.path()) {
            rows.push(("Where", paths::display(&p, &paths::home())));
        }
        for (r, (k, v)) in rows.into_iter().enumerate() {
            grid.attach(&gtk::Label::builder().label(k).xalign(1.0).css_classes(["dim-label"]).build(), 0, r as i32, 1, 1);
            grid.attach(&gtk::Label::builder().label(v).xalign(0.0).selectable(true).wrap(true).build(), 1, r as i32, 1, 1);
        }
        b.append(&grid);
    }
    b.upcast()
}

/// Render Markdown blocks into a read-only TextView.
fn render_markdown(blocks: &[Block]) -> gtk::TextView {
    let buf = gtk::TextBuffer::new(None);
    let table = buf.tag_table();
    let tag = |name: &str, f: &dyn Fn(&gtk::TextTag)| {
        let t = gtk::TextTag::new(Some(name));
        f(&t);
        table.add(&t);
    };
    for (lvl, scale) in [(1, 1.9), (2, 1.5), (3, 1.25), (4, 1.1), (5, 1.0), (6, 0.95)] {
        tag(&format!("h{lvl}"), &|t| {
            t.set_scale(scale);
            t.set_weight(800);
            t.set_pixels_above_lines(if lvl <= 2 { 14 } else { 8 });
            t.set_pixels_below_lines(6);
        });
    }
    tag("bold", &|t| t.set_weight(700));
    tag("italic", &|t| t.set_style(gtk::pango::Style::Italic));
    tag("strike", &|t| t.set_strikethrough(true));
    tag("code", &|t| {
        t.set_family(Some("monospace"));
        t.set_background_rgba(Some(&gdk::RGBA::new(0.5, 0.5, 0.5, 0.15)));
    });
    tag("link", &|t| {
        t.set_underline(gtk::pango::Underline::Single);
        t.set_foreground(Some("#3584e4"));
    });
    tag("para", &|t| t.set_pixels_below_lines(8));
    tag("quote", &|t| {
        t.set_left_margin(36);
        t.set_style(gtk::pango::Style::Italic);
        t.set_foreground_rgba(Some(&gdk::RGBA::new(0.5, 0.5, 0.5, 1.0)));
    });
    tag("codeblock", &|t| {
        t.set_family(Some("monospace"));
        t.set_paragraph_background_rgba(Some(&gdk::RGBA::new(0.5, 0.5, 0.5, 0.12)));
        t.set_left_margin(48);
        t.set_pixels_above_lines(2);
    });
    tag("rule", &|t| {
        t.set_foreground_rgba(Some(&gdk::RGBA::new(0.5, 0.5, 0.5, 0.6)));
        t.set_justification(gtk::Justification::Center);
    });
    tag("table", &|t| t.set_family(Some("monospace")));
    for d in 0..6 {
        tag(&format!("indent{d}"), &|t| {
            t.set_left_margin(54 + 24 * d);
            t.set_indent(-18);
        });
    }

    let put_spans = |spans: &[Span], extra: &[&str]| {
        for s in spans {
            let mut tags: Vec<&str> = extra.to_vec();
            if s.bold {
                tags.push("bold");
            }
            if s.italic {
                tags.push("italic");
            }
            if s.strike {
                tags.push("strike");
            }
            if s.code {
                tags.push("code");
            }
            if s.link.is_some() {
                tags.push("link");
            }
            let mut end = buf.end_iter();
            buf.insert_with_tags_by_name(&mut end, &s.text, &tags);
        }
    };
    let nl = |extra: &[&str]| {
        let mut end = buf.end_iter();
        buf.insert_with_tags_by_name(&mut end, "\n", extra);
    };
    for b in blocks {
        match b {
            Block::Heading(l, s) => {
                let t = format!("h{l}");
                put_spans(s, &[&t]);
                nl(&[&t]);
            }
            Block::Paragraph(s) => {
                put_spans(s, &["para"]);
                nl(&["para"]);
            }
            Block::ListItem(depth, bullet, s) => {
                let t = format!("indent{}", (*depth).min(5));
                let mut end = buf.end_iter();
                buf.insert_with_tags_by_name(&mut end, &format!("{bullet} "), &[&t]);
                put_spans(s, &[&t]);
                nl(&[&t]);
            }
            Block::Quote(s) => {
                put_spans(s, &["quote"]);
                nl(&["quote", "para"]);
            }
            Block::Code(_, code) => {
                let mut end = buf.end_iter();
                buf.insert_with_tags_by_name(&mut end, &format!("{code}\n"), &["codeblock"]);
                nl(&["para"]);
            }
            Block::Rule => {
                let mut end = buf.end_iter();
                buf.insert_with_tags_by_name(&mut end, "────────────────────\n", &["rule"]);
            }
            Block::Table(rows) => {
                // Aligned monospace columns.
                let cells: Vec<Vec<String>> = rows.iter().map(|r| r.iter().map(|c| c.iter().map(|s| s.text.as_str()).collect()).collect()).collect();
                let cols = cells.iter().map(Vec::len).max().unwrap_or(0);
                let widths: Vec<usize> = (0..cols).map(|i| cells.iter().filter_map(|r| r.get(i)).map(|c| c.chars().count()).max().unwrap_or(0)).collect();
                for (ri, r) in cells.iter().enumerate() {
                    let line: Vec<String> = (0..cols).map(|i| format!("{:<w$}", r.get(i).map(String::as_str).unwrap_or(""), w = widths[i])).collect();
                    let mut end = buf.end_iter();
                    let tags: &[&str] = if ri == 0 { &["table", "bold"] } else { &["table"] };
                    buf.insert_with_tags_by_name(&mut end, &format!("{}\n", line.join("   ")), tags);
                }
                nl(&["para"]);
            }
        }
    }
    let view = gtk::TextView::builder()
        .buffer(&buf)
        .editable(false)
        .cursor_visible(false)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(24)
        .bottom_margin(24)
        .left_margin(36)
        .right_margin(36)
        .css_classes(["ql-markdown"])
        .build();
    view.update_property(&[gtk::accessible::Property::Label("Document preview")]);
    view
}
