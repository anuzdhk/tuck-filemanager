// SPDX-License-Identifier: GPL-3.0-or-later
//! Main window: sidebar, header (navigation, breadcrumbs, view controls),
//! tabs with optional split view, and the information panel.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use tuck_core::settings::{FolderViews, Session, TabState, ViewMode, save_json};
use tuck_core::sort::SortKey;

use crate::breadcrumb::Breadcrumb;
use crate::info::InfoPanel;
use crate::jobs::Jobs;
use crate::item::FileItem;
use crate::pane::{Pane, PaneEvent, folder_title};
use crate::sidebar::Sidebar;

pub struct Tab {
    page: adw::TabPage,
    paned: gtk::Paned,
    left: Pane,
    right: RefCell<Option<Pane>>,
    right_active: Cell<bool>,
}

impl Tab {
    fn active(&self) -> Pane {
        if self.right_active.get()
            && let Some(r) = self.right.borrow().as_ref() {
                return r.clone();
            }
        self.left.clone()
    }

    fn state(&self) -> TabState {
        let path = |p: &Pane| p.path().unwrap_or_else(|| p.location().uri().as_str().into());
        TabState {
            left: path(&self.left),
            right: self.right.borrow().as_ref().map(path),
            right_active: self.right_active.get(),
        }
    }
}

pub struct Inner {
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    outer: adw::OverlaySplitView,
    info_split: adw::OverlaySplitView,
    tab_view: adw::TabView,
    sidebar: Sidebar,
    info: InfoPanel,
    breadcrumb: Breadcrumb,
    back: gtk::Button,
    forward: gtk::Button,
    up: gtk::Button,
    view_buttons: Vec<(ViewMode, gtk::ToggleButton)>,
    split_button: gtk::ToggleButton,
    info_button: gtk::ToggleButton,
    tabs: RefCell<Vec<Rc<Tab>>>,
    closed: RefCell<Vec<TabState>>,
    folder_views: Rc<RefCell<FolderViews>>,
    syncing: Cell<bool>,
    jobs: Jobs,
    quicklook: RefCell<Option<crate::quicklook::QuickLook>>,
    terminal: crate::terminal_ui::TerminalPanel,
    select_bar: gtk::Revealer,
    select_count: gtk::Label,
    search: crate::searchbar::SearchBar,
    search_gen: Cell<u64>,
    search_cancel: RefCell<tuck_core::ops::Cancel>,
    /// Set when this window is a file picker for another app.
    picker: RefCell<Option<Rc<crate::picker::Picker>>>,
    picker_bar: gtk::Revealer,
}

#[derive(Clone)]
pub struct Window(Rc<Inner>);

const SIDEBAR_MIN: f64 = 180.0;
const SIDEBAR_DEFAULT: f64 = 240.0;
const SIDEBAR_MAX: f64 = 420.0;

thread_local! {
    /// Open windows. GTK owns the widgets; this keeps our state alive until
    /// the window closes.
    static WINDOWS: RefCell<Vec<Window>> = const { RefCell::new(Vec::new()) };
}

impl Window {
    pub fn new(app: &adw::Application, folder_views: Rc<RefCell<FolderViews>>) -> Self {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .default_width(1180)
            .default_height(760)
            .width_request(360)
            .height_request(360)
            .title("Tuck")
            .build();

        // ---- sidebar column
        let sidebar = Sidebar::new();
        let brand_label = gtk::Label::builder().label("Tuck").css_classes(["brand"]).build();
        let brand = gtk::Box::builder().spacing(8).build();
        brand.append(&gtk::Image::builder().icon_name(crate::APP_ID).pixel_size(22).build());
        brand.append(&brand_label);
        let sidebar_header = adw::HeaderBar::builder()
            .show_end_title_buttons(false)
            .title_widget(&brand)
            .build();
        let sidebar_tv = adw::ToolbarView::new();
        sidebar_tv.add_top_bar(&sidebar_header);
        sidebar_tv.set_content(Some(&sidebar.widget()));

        // ---- header bar
        let back = icon_button("tuck-back-symbolic", "Back (Alt+Left)", "win.back");
        let forward = icon_button("tuck-forward-symbolic", "Forward (Alt+Right)", "win.forward");
        let up = icon_button("tuck-up-symbolic", "Enclosing Folder (Alt+Up)", "win.up");
        let nav = gtk::Box::builder().css_classes(["linked", "nav-buttons"]).build();
        nav.append(&back);
        nav.append(&forward);
        nav.append(&up);
        let sidebar_toggle = gtk::ToggleButton::builder()
            .icon_name("tuck-sidebar-symbolic")
            .tooltip_text("Show Sidebar (F9)")
            .build();
        sidebar_toggle.update_property(&[gtk::accessible::Property::Label("Show Sidebar")]);

        let breadcrumb = Breadcrumb::new();

        let mut view_buttons = Vec::new();
        let views_box = gtk::Box::builder().css_classes(["linked", "view-switch"]).build();
        let mut group: Option<gtk::ToggleButton> = None;
        for (mode, icon, tip) in [
            (ViewMode::Icons, "tuck-grid-symbolic", "Icons (Ctrl+1)"),
            (ViewMode::Compact, "tuck-compact-symbolic", "Compact (Ctrl+2)"),
            (ViewMode::Details, "tuck-details-symbolic", "Details (Ctrl+3)"),
            (ViewMode::Columns, "tuck-columns-symbolic", "Columns (Ctrl+4)"),
        ] {
            let b = gtk::ToggleButton::builder().icon_name(icon).tooltip_text(tip).build();
            b.update_property(&[gtk::accessible::Property::Label(tip.split(" (").next().unwrap_or(tip))]);
            b.set_action_name(Some("win.view-mode"));
            b.set_action_target_value(Some(&mode.id().to_variant()));
            if let Some(g) = &group {
                b.set_group(Some(g));
            } else {
                group = Some(b.clone());
            }
            views_box.append(&b);
            view_buttons.push((mode, b));
        }
        let split_button = gtk::ToggleButton::builder()
            .icon_name("tuck-split-symbolic")
            .tooltip_text("Split View (F3)")
            .action_name("win.split")
            .build();
        split_button.update_property(&[gtk::accessible::Property::Label("Split View")]);
        let info_button = gtk::ToggleButton::builder()
            .icon_name("tuck-info-symbolic")
            .tooltip_text("Information Panel (F11)")
            .build();
        info_button.update_property(&[gtk::accessible::Property::Label("Information Panel")]);
        let menu = gtk::MenuButton::builder()
            .icon_name("tuck-menu-symbolic")
            .menu_model(&main_menu())
            .tooltip_text("Main Menu (F10)")
            .primary(true)
            .build();
        menu.update_property(&[gtk::accessible::Property::Label("Main Menu")]);

        let jobs = Jobs::new();
        // The path sits on the left, after the navigation buttons, and takes
        // the free space up to the search field.
        let middle = gtk::Box::builder().spacing(8).hexpand(true).build();
        middle.append(&breadcrumb.widget());
        let header = adw::HeaderBar::builder().show_title(false).build();
        header.pack_start(&sidebar_toggle);
        header.pack_start(&nav);
        header.pack_start(&middle);
        header.pack_end(&menu);
        header.pack_end(&info_button);
        header.pack_end(&split_button);
        header.pack_end(&views_box);
        header.pack_end(&jobs.widget());

        // ---- tabs + info
        let tab_view = adw::TabView::new();
        let tab_bar = adw::TabBar::builder().view(&tab_view).autohide(true).build();
        tab_bar.setup_extra_drop_target(
            gdk::DragAction::COPY | gdk::DragAction::MOVE,
            &[gdk::FileList::static_type()],
        );
        let new_tab = icon_button("tuck-new-tab-symbolic", "New Tab (Ctrl+T)", "win.new-tab");
        new_tab.add_css_class("flat");
        tab_bar.set_end_action_widget(Some(&new_tab));
        let info = InfoPanel::new();
        // Terminal below the files (F4), resizable.
        let terminal = crate::terminal_ui::TerminalPanel::new();
        let files_and_term = gtk::Paned::builder()
            .orientation(gtk::Orientation::Vertical)
            .start_child(&tab_view)
            .end_child(&terminal.widget())
            .resize_start_child(true)
            .resize_end_child(false)
            .shrink_end_child(false)
            .wide_handle(false)
            .build();
        let info_split = adw::OverlaySplitView::builder()
            .sidebar_position(gtk::PackType::End)
            .sidebar(&info.widget())
            .content(&files_and_term)
            .min_sidebar_width(240.0)
            .max_sidebar_width(320.0)
            .sidebar_width_fraction(0.24)
            .show_sidebar(true)
            .build();
        info_split.add_css_class("info-split");

        let search = crate::searchbar::SearchBar::new();
        let search_button = gtk::ToggleButton::builder().icon_name("tuck-search-symbolic").tooltip_text("Search (Ctrl+F)").build();
        search_button.update_property(&[gtk::accessible::Property::Label("Search")]);
        search.revealer.bind_property("reveal-child", &search_button, "active").bidirectional().sync_create().build();
        header.pack_end(&search_button);
        // A search field when the path is short; just the button once the
        // path needs the room. Either opens the search bar.
        let pill = gtk::Button::builder().css_classes(["search-pill"]).valign(gtk::Align::Center).tooltip_text("Search (Ctrl+F)").build();
        let pill_box = gtk::Box::builder().spacing(8).build();
        pill_box.append(&gtk::Image::from_icon_name("tuck-search-symbolic"));
        pill_box.append(&gtk::Label::builder().label("Search files…").hexpand(true).xalign(0.0).build());
        pill_box.append(&gtk::Label::builder().label("Ctrl+F").css_classes(["dim-label", "caption"]).build());
        pill.set_child(Some(&pill_box));
        pill.update_property(&[gtk::accessible::Property::Label("Search")]);
        middle.append(&pill);
        {
            let sb = search_button.clone();
            pill.connect_clicked(move |_| sb.set_active(true));
        }
        let fits = Rc::new(std::cell::Cell::new(true));
        let sync = {
            let (pill, sb, fits) = (pill.clone(), search_button.clone(), fits.clone());
            move || {
                let wide = fits.get() && !sb.is_active();
                pill.set_visible(wide);
                sb.set_visible(!wide);
            }
        };
        sync();
        // Follows the header's width: tells us when the window or the
        // sidebars change size.
        let probe = gtk::DrawingArea::builder().can_target(false).can_focus(false).build();
        {
            let (fits, sync, bc, mid, sb, pill) = (fits.clone(), sync.clone(), breadcrumb.clone(), middle.clone(), search_button.clone(), pill.clone());
            let check = move || {
                // The header lays out [start buttons + middle] … [end buttons];
                // what's between them is unused, and the middle could have it.
                let Some(row) = mid.parent().and_then(|start| start.parent()) else { return };
                let mut used = 0;
                let mut c = row.first_child();
                while let Some(w) = c {
                    if w.is_visible() {
                        used += w.width();
                    }
                    c = w.next_sibling();
                }
                let free = mid.width() + (row.width() - used).max(0);
                if free <= 0 {
                    return;
                }
                // Room for the path beside the field (and the spacing).
                let mut room = free - pill.measure(gtk::Orientation::Horizontal, -1).0 - 8;
                if !fits.get() {
                    // The search button and its spacing come back too, less a
                    // little slack so it doesn't flicker.
                    room += sb.width() + 6 - 4;
                }
                let now = bc.path_width() <= f64::from(room);
                if now != fits.get() {
                    fits.set(now);
                    sync();
                }
            };
            let c = check.clone();
            breadcrumb.width_adjustment().connect_changed(move |_| c());
            probe.connect_resize(move |_, _, _| check());
        }
        search_button.connect_active_notify(move |_| sync());
        let content_tv = adw::ToolbarView::new();
        let header_box = gtk::Overlay::builder().child(&header).build();
        header_box.add_overlay(&probe);
        content_tv.add_top_bar(&header_box);
        content_tv.add_top_bar(&search.revealer);
        content_tv.add_top_bar(&tab_bar);
        content_tv.set_content(Some(&info_split));
        // Selection mode: actions for the picked items along the bottom.
        let select_count = gtk::Label::builder().label("Nothing selected").css_classes(["heading"]).build();
        let bar = gtk::ActionBar::new();
        bar.set_center_widget(Some(&select_count));
        bar.pack_start(&gtk::Button::builder().label("Select All").action_name("win.select-all").build());
        for (icon, tip, action) in [
            ("tuck-trash-symbolic", "Move to Trash", "win.trash"),
            ("tuck-open-folder-symbolic", "Move To…", "win.move-to"),
            ("tuck-documents-symbolic", "Copy To…", "win.copy-to"),
        ] {
            let b = gtk::Button::builder().icon_name(icon).tooltip_text(tip).action_name(action).build();
            b.update_property(&[gtk::accessible::Property::Label(tip)]);
            bar.pack_end(&b);
        }
        let done = gtk::Button::builder().label("Done").css_classes(["suggested-action"]).action_name("win.selection-mode").build();
        bar.pack_end(&done);
        let select_bar = gtk::Revealer::builder().child(&bar).transition_type(gtk::RevealerTransitionType::SlideUp).build();
        content_tv.add_bottom_bar(&select_bar);
        let picker_bar = gtk::Revealer::new();
        content_tv.add_bottom_bar(&picker_bar);

        // Drag the sidebar's right edge to resize it.
        let sidebar_box = gtk::Overlay::builder().child(&sidebar_tv).build();
        let resize_handle = gtk::Box::builder()
            .width_request(8)
            .halign(gtk::Align::End)
            .valign(gtk::Align::Fill)
            .css_classes(["sidebar-resize"])
            .cursor(&gdk::Cursor::from_name("col-resize", None).expect("cursor"))
            .tooltip_text("Drag to resize, double-click to reset")
            .build();
        sidebar_box.add_overlay(&resize_handle);
        let outer = adw::OverlaySplitView::builder()
            .sidebar(&sidebar_box)
            .content(&content_tv)
            .min_sidebar_width(SIDEBAR_DEFAULT)
            .max_sidebar_width(SIDEBAR_DEFAULT)
            .sidebar_width_fraction(1.0)
            .build();
        outer
            .bind_property("show-sidebar", &sidebar_toggle, "active")
            .bidirectional()
            .sync_create()
            .build();
        info_split
            .bind_property("show-sidebar", &info_button, "active")
            .bidirectional()
            .sync_create()
            .build();

        let toasts = adw::ToastOverlay::new();
        let tab_bar_for_drop = tab_bar.clone();
        toasts.set_child(Some(&outer));
        window.set_content(Some(&toasts));

        // Collapse panels on narrow windows.
        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            720.0,
            adw::LengthUnit::Sp,
        ));
        narrow.add_setter(&outer, "collapsed", Some(&true.to_value()));
        narrow.add_setter(&info_split, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(narrow);
        let medium = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            1000.0,
            adw::LengthUnit::Sp,
        ));
        medium.add_setter(&info_split, "collapsed", Some(&true.to_value()));
        medium.add_setter(&info_split, "show-sidebar", Some(&false.to_value()));
        window.add_breakpoint(medium);

        let win = Window(Rc::new(Inner {
            window,
            toasts,
            outer,
            info_split,
            tab_view,
            sidebar,
            info,
            breadcrumb,
            back,
            forward,
            up,
            view_buttons,
            split_button,
            info_button,
            tabs: RefCell::new(Vec::new()),
            closed: RefCell::new(Vec::new()),
            folder_views,
            syncing: Cell::new(false),
            jobs,
            quicklook: RefCell::new(None),
            terminal,
            select_bar,
            select_count,
            search,
            search_gen: Cell::new(0),
            search_cancel: RefCell::new(tuck_core::ops::Cancel::new()),
            picker: RefCell::new(None),
            picker_bar,
        }));
        // Dropping files on a tab puts them in that tab's folder.
        let weak = win.downgrade();
        tab_bar_for_drop.connect_extra_drag_drop(move |bar, page, value| {
            let Some(w) = Window::from_weak(&weak) else { return false };
            let Ok(list) = value.get::<gdk::FileList>() else { return false };
            let Some(tab) = w.0.tabs.borrow().iter().find(|t| &t.page == page).cloned() else { return false };
            let action = bar.extra_drag_preferred_action();
            let action = if action.is_empty() { gdk::DragAction::COPY } else { action };
            w.handle_drop(list.files(), tab.active().location(), action);
            true
        });
        let weak = win.downgrade();
        win.0.terminal.connect_dir_changed(move |dir| {
            // `cd` in the terminal moves the file view along.
            let Some(w) = Window::from_weak(&weak) else { return };
            if let Some(p) = w.active_pane()
                && p.location().path().as_deref() != Some(dir.as_path()) {
                    p.navigate(gio::File::for_path(dir));
                }
        });
        let weak = win.downgrade();
        win.0.search.connect_changed(move || {
            if let Some(w) = Window::from_weak(&weak) {
                w.run_search();
            }
        });
        let weak = win.downgrade();
        win.0.search.connect_stop(move || {
            if let Some(w) = Window::from_weak(&weak) {
                w.close_search();
            }
        });
        let weak = win.downgrade();
        win.0.search.revealer.connect_reveal_child_notify(move |r| {
            if let Some(w) = Window::from_weak(&weak) {
                if r.reveals_child() {
                    w.0.search.entry.grab_focus();
                } else if let Some(p) = w.active_pane() {
                    p.end_search();
                    p.focus_view();
                }
            }
        });
        win.setup_sidebar_resize(&resize_handle);
        win.setup_actions();
        win.setup_file_actions();
        crate::addons_ui::setup_actions(&win);
        win.wire();
        if std::env::var_os("TUCK_DEBUG_KEYS").is_some() {
            // Diagnostics: which widget has focus when a key arrives.
            let keys = gtk::EventControllerKey::new();
            keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            let w2 = win.0.window.clone();
            keys.connect_key_pressed(move |_, key, _, state| {
                let mut chain = Vec::new();
                let mut cur = gtk::prelude::GtkWindowExt::focus(&w2);
                while let Some(f) = cur {
                    chain.push(format!("{}{:?}", f.type_().name(), f.css_classes().iter().map(|c| c.to_string()).collect::<Vec<_>>()));
                    if chain.len() >= 6 {
                        break;
                    }
                    cur = f.parent();
                }
                eprintln!("[keys] {:?} {:?} focus: {}", key.name(), state, if chain.is_empty() { "(none)".into() } else { chain.join(" < ") });
                glib::Propagation::Proceed
            });
            win.0.window.add_controller(keys);
        }
        WINDOWS.with(|w| w.borrow_mut().push(win.clone()));
        win.restyle();
        win
    }

    /// The widest the sidebar may get: leave room for the file view (and
    /// the information panel when it's beside it).
    fn sidebar_max(&self) -> f64 {
        let w = self.0.window.width() as f64;
        if w <= 0.0 {
            return SIDEBAR_MAX;
        }
        let info = if self.0.info_split.shows_sidebar() && !self.0.info_split.is_collapsed() { self.0.info_split.max_sidebar_width() } else { 0.0 };
        (w - info - 440.0).clamp(SIDEBAR_MIN, SIDEBAR_MAX)
    }

    fn set_sidebar_width(&self, width: f64) {
        let w = width.clamp(SIDEBAR_MIN, self.sidebar_max()).round();
        self.0.outer.set_min_sidebar_width(w);
        self.0.outer.set_max_sidebar_width(w);
    }

    fn setup_sidebar_resize(&self, handle: &gtk::Box) {
        self.set_sidebar_width(crate::theme_ui::settings().sidebar_width);
        let drag = gtk::GestureDrag::new();
        let start = Rc::new(Cell::new(0.0));
        let (weak, s) = (self.downgrade(), start.clone());
        drag.connect_drag_begin(move |_, _, _| {
            if let Some(w) = Window::from_weak(&weak) {
                s.set(w.0.outer.max_sidebar_width());
            }
        });
        let (weak, s) = (self.downgrade(), start.clone());
        drag.connect_drag_update(move |_, dx, _| {
            if let Some(w) = Window::from_weak(&weak) {
                w.set_sidebar_width(s.get() + dx);
            }
        });
        let weak = self.downgrade();
        drag.connect_drag_end(move |_, _, _| {
            if let Some(w) = Window::from_weak(&weak) {
                let width = w.0.outer.max_sidebar_width();
                crate::theme_ui::update_quiet(|s| s.sidebar_width = width);
            }
        });
        handle.add_controller(drag);
        // Double-click: back to the default width.
        let click = gtk::GestureClick::new();
        let weak = self.downgrade();
        click.connect_pressed(move |_, n, _, _| {
            if n == 2 && let Some(w) = Window::from_weak(&weak) {
                w.set_sidebar_width(SIDEBAR_DEFAULT);
                crate::theme_ui::update_quiet(|s| s.sidebar_width = SIDEBAR_DEFAULT);
            }
        });
        handle.add_controller(click);
        // A smaller window can't keep a wide sidebar.
        let weak = self.downgrade();
        self.0.window.connect_default_width_notify(move |_| {
            if let Some(w) = Window::from_weak(&weak) {
                let cur = w.0.outer.max_sidebar_width();
                if cur > w.sidebar_max() {
                    w.set_sidebar_width(cur);
                }
            }
        });
    }

    pub(crate) fn downgrade(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    pub(crate) fn from_weak(w: &Weak<Inner>) -> Option<Window> {
        w.upgrade().map(Window)
    }

    /// Re-apply theme-dependent window classes on every window.
    pub fn restyle_all() {
        WINDOWS.with(|list| {
            for w in list.borrow().iter() {
                w.restyle();
            }
        });
    }

    fn restyle(&self) {
        let win = &self.0.window;
        for c in crate::theme_ui::ALL_WINDOW_CLASSES {
            win.remove_css_class(c);
        }
        for c in crate::theme_ui::window_classes() {
            win.add_css_class(c);
        }
        self.0.terminal.apply_colors();
    }

    /// Rebuild file views so they pick up a new icon style or accent.
    pub fn refresh_all_icons() {
        WINDOWS.with(|list| {
            for w in list.borrow().iter() {
                for t in w.0.tabs.borrow().iter() {
                    t.left.refresh_icons();
                    if let Some(r) = t.right.borrow().as_ref() {
                        r.refresh_icons();
                    }
                }
                w.0.sidebar.rebuild_public();
                w.sync_info();
            }
        });
    }

    /// The Tuck window behind a GTK window (used by test hooks).
    pub fn find(w: &gtk::Window) -> Option<Window> {
        WINDOWS.with(|list| list.borrow().iter().find(|x| x.0.window.upcast_ref::<gtk::Window>() == w).cloned())
    }

    /// Ask for a server address (sftp://, smb://, ftp://, dav://…), mount
    /// it, and open it.
    fn connect_to_server(&self) {
        let entry = gtk::Entry::builder().placeholder_text("sftp://user@example.com/").activates_default(true).build();
        entry.update_property(&[gtk::accessible::Property::Label("Server address")]);
        let dialog = adw::AlertDialog::builder()
            .heading("Connect to Server")
            .body("SFTP (sftp://), Windows shares (smb://), FTP (ftp://) and WebDAV (dav://, davs://) are supported.")
            .extra_child(&entry)
            .default_response("connect")
            .close_response("cancel")
            .build();
        dialog.add_responses(&[("cancel", "Cancel"), ("connect", "Connect")]);
        dialog.set_response_appearance("connect", adw::ResponseAppearance::Suggested);
        let weak = self.downgrade();
        dialog.connect_response(None, move |_, r| {
            let Some(w) = Window::from_weak(&weak) else { return };
            let text = entry.text().trim().to_string();
            if r != "connect" || text.is_empty() {
                return;
            }
            let uri = if text.contains("://") { text } else { format!("sftp://{text}") };
            let file = gio::File::for_uri(&uri);
            let op = gtk::MountOperation::new(Some(&w.0.window));
            let weak = w.downgrade();
            let f2 = file.clone();
            file.mount_enclosing_volume(gio::MountMountFlags::NONE, Some(&op), gio::Cancellable::NONE, move |res| {
                let Some(w) = Window::from_weak(&weak) else { return };
                match res {
                    Err(e) if !e.matches(gio::IOErrorEnum::AlreadyMounted) => {
                        if !e.matches(gio::IOErrorEnum::FailedHandled) {
                            w.toast(&format!("Couldn't connect: {}", e.message()));
                        }
                    }
                    _ => {
                        if let Some(p) = w.active_pane() {
                            p.navigate(f2);
                        }
                    }
                }
            });
        });
        dialog.present(Some(&self.0.window));
    }

    pub(crate) fn terminal(&self) -> &crate::terminal_ui::TerminalPanel {
        &self.0.terminal
    }

    pub(crate) fn gtk_window(&self) -> adw::ApplicationWindow {
        self.0.window.clone()
    }

    pub(crate) fn jobs(&self) -> Jobs {
        self.0.jobs.clone()
    }

    fn quick_look(&self) -> crate::quicklook::QuickLook {
        if let Some(q) = self.0.quicklook.borrow().clone() {
            return q;
        }
        let q = crate::quicklook::QuickLook::new(&self.0.window);
        let w1 = self.downgrade();
        let w2 = self.downgrade();
        let w3 = self.downgrade();
        let w4 = self.downgrade();
        q.connect(
            move |delta| Window::from_weak(&w1).and_then(|w| w.active_pane()).and_then(|p| p.neighbor(delta)),
            move |item, choose| {
                let Some(w) = Window::from_weak(&w2) else { return };
                if item.is_dir() {
                    if let Some(p) = w.active_pane() {
                        p.navigate(item.target());
                    }
                } else {
                    w.open_files(vec![item], choose);
                }
            },
            move |path| {
                if let Some(w) = Window::from_weak(&w3) {
                    w.extract_archive(path, None);
                }
            },
            move || {
                if let Some(w) = Window::from_weak(&w4) {
                    w.0.window.present();
                    if let Some(p) = w.active_pane() {
                        p.focus_view();
                    }
                }
            },
        );
        let w5 = self.downgrade();
        q.set_install(move |path| {
            if let Some(w) = Window::from_weak(&w5) {
                w.install_file(path);
            }
        });
        *self.0.quicklook.borrow_mut() = Some(q.clone());
        q
    }

    /// Same as pressing an arrow key in Quick Look (test hooks).
    pub(crate) fn quicklook_step(&self, delta: i32) {
        if let Some(q) = self.0.quicklook.borrow().clone() {
            q.step(delta);
        }
    }

    pub(crate) fn quicklook_window(&self) -> Option<adw::Window> {
        self.0.quicklook.borrow().as_ref().filter(|q| q.is_visible()).map(|q| q.window())
    }

    // ------------------------------------------------------------ search

    /// Type into the search field (test hooks).
    pub(crate) fn set_search_text(&self, text: &str) {
        self.0.search.open();
        // "contents:" and "here:" prefixes flip the toggles (test hooks).
        let mut t = text;
        if let Some(rest) = t.strip_prefix("contents:") {
            self.0.search.set_contents(true);
            t = rest;
        }
        if let Some(rest) = t.strip_prefix("here:") {
            self.0.search.set_everywhere(false);
            t = rest;
        }
        self.0.search.entry.set_text(t);
    }

    fn close_search(&self) {
        self.0.search_cancel.borrow().cancel();
        self.0.search.close();
        self.0.search.set_results("");
    }

    /// Run the search bar's query in the active pane.
    fn run_search(&self) {
        use crate::search_ui::{self, Found, State};
        let Some(pane) = self.active_pane() else { return };
        let mut q = self.0.search.query();
        let gen_ = self.0.search_gen.get() + 1;
        self.0.search_gen.set(gen_);
        self.0.search_cancel.borrow().cancel();
        let cancel = tuck_core::ops::Cancel::new();
        *self.0.search_cancel.borrow_mut() = cancel.clone();
        if q.is_empty() {
            pane.end_search();
            self.0.search.set_status("");
            self.0.search.set_results("");
            return;
        }
        let contents = self.0.search.contents();
        let folder = if self.0.search.everywhere() { None } else { pane.path() };
        if self.0.search.everywhere() && q.location.is_none() {
            // Everywhere = the indexed home folder.
        } else if let Some(f) = &folder {
            q.location.get_or_insert(f.clone());
        }
        let (state, entries) = search_ui::state();
        let use_index = state == State::Ready && q.location.as_ref().is_none_or(|l| search_ui::covers(l));
        let live_root = q.location.clone().unwrap_or_else(tuck_core::paths::home);

        let (tx, rx) = async_channel::unbounded::<(Vec<Found>, Option<(usize, u128, bool)>)>();
        std::thread::spawn(move || {
            if use_index
                && let Some(o) = search_ui::indexed(&q, contents, 500) {
                    let _ = tx.send_blocking((o.results, Some((o.total, o.micros, true))));
                    return;
                }
            let start = std::time::Instant::now();
            let mut count = 0usize;
            let tx2 = tx.clone();
            search_ui::walk(&live_root, &q, contents, &cancel, |batch| {
                count += batch.len();
                tx2.send_blocking((batch, None)).is_ok() && count < 5000
            });
            let _ = tx.send_blocking((Vec::new(), Some((count, start.elapsed().as_micros(), false))));
        });

        let weak = self.downgrade();
        let pane_weak = pane.clone();
        glib::MainContext::default().spawn_local(async move {
            let mut first = true;
            let mut shown = 0usize;
            while let Ok((batch, done)) = rx.recv().await {
                let Some(w) = Window::from_weak(&weak) else { return };
                if w.0.search_gen.get() != gen_ {
                    return; // a newer query took over
                }
                let items: Vec<FileItem> = batch
                    .into_iter()
                    .map(|f| {
                        let item = FileItem::from_raw(&gio::File::for_path(&f.parent), f.entry);
                        item.set_found(f.score, f.in_content);
                        item
                    })
                    .collect();
                shown += items.len();
                let status = match done {
                    Some((total, micros, true)) => {
                        let more = if total > shown { format!(" (showing best {shown})") } else { String::new() };
                        format!("{} result{}{more} · {:.1} ms", total, if total == 1 { "" } else { "s" }, micros as f64 / 1000.0)
                    }
                    Some((_, micros, false)) => format!("{shown} result{} · searched live in {:.1} s", if shown == 1 { "" } else { "s" }, micros as f64 / 1e6),
                    None => format!("Searching… {shown} found"),
                };
                if first {
                    pane_weak.show_results(items, &status);
                    first = false;
                } else {
                    pane_weak.append_results(items, &status);
                }
                let bar_status = match (done.is_some(), use_index, state) {
                    (_, true, _) => format!("· Index: {entries} items"),
                    (false, false, _) => String::new(),
                    (true, false, State::Ready) => "· searched directly (not in the index)".into(),
                    (true, false, _) => "· index is being built".into(),
                };
                w.0.search.set_status(&bar_status);
                if let Some(s) = pane_weak.search_summary() {
                    w.0.search.set_results(&s);
                }
            }
        });
    }

    /// A toast with an Undo button.
    pub(crate) fn toast_undo(&self, text: &str) {
        let t = adw::Toast::builder()
            .title(glib::markup_escape_text(text))
            .button_label("_Undo")
            .action_name("win.undo")
            .timeout(6)
            .build();
        self.0.toasts.add_toast(t);
    }

    pub fn present(&self) {
        self.0.window.present();
    }

    /// A toast with a button that runs `f`.
    pub fn toast_with_button(&self, text: &str, label: &str, f: impl Fn() + 'static) {
        let t = adw::Toast::builder().title(glib::markup_escape_text(text)).button_label(label).timeout(10).build();
        t.connect_button_clicked(move |_| f());
        self.0.toasts.add_toast(t);
    }

    /// Any open window (for dialogs that outlive their parent).
    pub fn any() -> Option<Window> {
        WINDOWS.with(|l| l.borrow().first().cloned())
    }

    pub fn toast(&self, text: &str) {
        let t = adw::Toast::builder().title(glib::markup_escape_text(text)).timeout(5).build();
        self.0.toasts.add_toast(t);
    }

    // ---------------------------------------------------------------- tabs

    fn current_tab(&self) -> Option<Rc<Tab>> {
        let page = self.0.tab_view.selected_page()?;
        self.0.tabs.borrow().iter().find(|t| t.page == page).cloned()
    }

    pub(crate) fn picker(&self) -> Option<Rc<crate::picker::Picker>> {
        self.0.picker.borrow().clone()
    }

    /// Turn this window into a file picker with `bar` along the bottom.
    pub(crate) fn set_picker(&self, picker: Rc<crate::picker::Picker>, bar: &gtk::Widget) {
        *self.0.picker.borrow_mut() = Some(picker);
        self.0.picker_bar.set_child(Some(bar));
        self.0.picker_bar.set_reveal_child(true);
    }

    pub fn active_pane(&self) -> Option<Pane> {
        self.current_tab().map(|t| t.active())
    }

    fn tab_of(&self, pane: &Pane) -> Option<Rc<Tab>> {
        self.0.tabs.borrow().iter().find(|t| {
            t.left.ptr_eq(pane) || t.right.borrow().as_ref().is_some_and(|r| r.ptr_eq(pane))
        }).cloned()
    }

    fn make_pane(&self, location: gio::File) -> Pane {
        let pane = Pane::new(self.0.folder_views.clone(), location);
        let weak = self.downgrade();
        pane.connect(move |p, ev| {
            if let Some(w) = Window::from_weak(&weak) {
                w.on_pane_event(p, ev);
            }
        });
        pane
    }

    pub fn open_tab(&self, location: gio::File, select: bool) -> Rc<Tab> {
        let left = self.make_pane(location);
        let paned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .start_child(&left.widget())
            .resize_start_child(true)
            .shrink_start_child(false)
            .resize_end_child(true)
            .shrink_end_child(false)
            .wide_handle(true)
            .build();
        let page = self.0.tab_view.append(&paned);
        page.set_title(&left.title());
        page.set_icon(Some(&gio::ThemedIcon::new("tuck-folder-symbolic")));
        let tab = Rc::new(Tab { page: page.clone(), paned, left, right: RefCell::new(None), right_active: Cell::new(false) });
        self.0.tabs.borrow_mut().push(tab.clone());
        if select {
            self.0.tab_view.set_selected_page(&page);
            tab.left.focus_view();
        }
        self.sync();
        tab
    }

    fn restore_tab(&self, state: &TabState, select: bool) {
        let file = |p: &std::path::Path| {
            let s = p.to_string_lossy();
            if s.contains("://") { gio::File::for_uri(&s) } else { gio::File::for_path(p) }
        };
        let tab = self.open_tab(file(&state.left), select);
        if let Some(r) = &state.right {
            self.set_split(&tab, Some(file(r)));
            tab.right_active.set(state.right_active);
            self.update_active_styles(&tab);
        }
    }

    fn set_split(&self, tab: &Rc<Tab>, right: Option<gio::File>) {
        match right {
            Some(loc) => {
                if tab.right.borrow().is_some() {
                    return;
                }
                let pane = self.make_pane(loc);
                tab.paned.set_end_child(Some(&pane.widget()));
                tab.paned.add_css_class("split");
                *tab.right.borrow_mut() = Some(pane.clone());
                tab.right_active.set(true);
                // Even split once the paned has a size.
                let paned = tab.paned.clone();
                glib::idle_add_local_once(move || {
                    let w = paned.width();
                    if w > 0 {
                        paned.set_position(w / 2);
                    }
                });
                pane.focus_view();
            }
            None => {
                let right = tab.right.borrow_mut().take();
                if right.is_some() {
                    // Closing split keeps the active pane's location on the left.
                    if tab.right_active.get()
                        && let Some(r) = &right {
                            tab.left.navigate(r.location());
                        }
                    tab.paned.set_end_child(None::<&gtk::Widget>);
                    tab.paned.remove_css_class("split");
                    tab.right_active.set(false);
                    tab.left.focus_view();
                }
            }
        }
        self.update_active_styles(tab);
        self.sync();
    }

    fn update_active_styles(&self, tab: &Tab) {
        let split = tab.right.borrow().is_some();
        tab.left.set_active(!split || !tab.right_active.get());
        if let Some(r) = tab.right.borrow().as_ref() {
            r.set_active(tab.right_active.get());
        }
    }

    fn close_tab(&self, page: &adw::TabPage) {
        if self.0.tab_view.n_pages() <= 1 {
            return;
        }
        self.0.tab_view.close_page(page);
    }

    // ------------------------------------------------------------- events

    fn on_pane_event(&self, pane: &Pane, ev: PaneEvent) {
        match ev {
            PaneEvent::Focused => {
                if let Some(tab) = self.tab_of(pane) {
                    let is_right = tab.right.borrow().as_ref().is_some_and(|r| r.ptr_eq(pane));
                    if tab.right_active.get() != is_right {
                        tab.right_active.set(is_right);
                        self.update_active_styles(&tab);
                        self.sync();
                    }
                }
            }
            PaneEvent::Location => {
                if let Some(tab) = self.tab_of(pane) {
                    tab.page.set_title(&tab.active().title());
                    tab.page.set_tooltip(&tab.active().location().parse_name());
                }
                if self.is_active(pane) {
                    self.sync();
                }
            }
            PaneEvent::Selection | PaneEvent::Counts => {
                if self.is_active(pane) {
                    if let Some(s) = pane.search_summary() {
                        self.0.search.set_results(&s);
                    }
                    self.sync_info();
                    self.sync_selection_bar();
                    // Quick Look follows the selection while it's open.
                    let ql = self.0.quicklook.borrow().clone();
                    if let Some(ql) = ql.filter(|q| q.is_visible())
                        && let Some(first) = pane.selected_items().into_iter().next() {
                            ql.show(&first);
                        }
                }
            }
            PaneEvent::Settings => {
                if self.is_active(pane) {
                    self.sync_actions();
                    self.sync_selection_bar();
                    self.0.back.set_sensitive(pane.can_go_back());
                }
            }
            PaneEvent::OpenInNewTab(f) => {
                self.open_tab(f, false);
            }
            PaneEvent::Open(items) => self.open_files(items, false),
            PaneEvent::Drop { files, target, action } => self.handle_drop(files, target, action),
        }
    }

    fn is_active(&self, pane: &Pane) -> bool {
        self.active_pane().is_some_and(|a| a.ptr_eq(pane))
    }

    /// Bring header, sidebar, info panel and actions in line with the active pane.
    fn sync(&self) {
        let Some(pane) = self.active_pane() else { return };
        let loc = pane.location();
        self.0.breadcrumb.set_location(&loc);
        self.0.sidebar.set_location(&loc);
        self.0.window.set_title(Some(&format!("{} — Tuck", folder_title(&loc))));
        self.0.back.set_sensitive(pane.can_go_back());
        self.0.forward.set_sensitive(pane.can_go_forward());
        self.0.up.set_sensitive(pane.can_go_up());
        self.0.search.set_results(&pane.search_summary().unwrap_or_default());
        if let Some(dir) = loc.path() {
            self.0.terminal.follow(&dir);
        }
        let split = self.current_tab().is_some_and(|t| t.right.borrow().is_some());
        if let Some(a) = self.action("split") {
            a.set_state(&split.to_variant());
        }
        self.sync_actions();
        self.sync_info();
        self.sync_selection_bar();
    }

    fn sync_selection_bar(&self) {
        let picker = self.picker();
        if let Some(pk) = picker {
            pk.sync(self);
        }
        let Some(p) = self.active_pane() else { return };
        self.0.select_bar.set_reveal_child(p.selection_mode());
        let n = p.selected_items().len();
        self.0.select_count.set_label(&match n {
            0 => "Click items to select them".to_string(),
            1 => "1 item selected".to_string(),
            n => format!("{n} items selected"),
        });
    }

    fn sync_info(&self) {
        if !self.0.info_split.shows_sidebar() {
            return;
        }
        if let Some(pane) = self.active_pane() {
            self.0.info.show(&pane.selected_items(), &pane.location());
        }
    }

    fn sync_actions(&self) {
        let Some(pane) = self.active_pane() else { return };
        let v = pane.settings();
        self.0.syncing.set(true);
        let set = |name: &str, value: glib::Variant| {
            if let Some(a) = self.action(name) {
                a.set_state(&value);
            }
        };
        set("view-mode", v.mode.id().to_variant());
        set("sort-by", v.sort.key.id().to_variant());
        set("sort-descending", v.sort.descending.to_variant());
        set("folders-first", v.sort.folders_first.to_variant());
        set("show-groups", v.grouped.to_variant());
        set("show-hidden", v.show_hidden.to_variant());
        set("show-previews", v.previews.to_variant());
        for (id, ..) in tuck_core::settings::COLUMNS {
            set(&format!("column-{id}"), v.has_column(id).to_variant());
        }
        self.0.syncing.set(false);
        let _ = &self.0.view_buttons;
    }

    fn action(&self, name: &str) -> Option<gio::SimpleAction> {
        self.0.window.lookup_action(name).and_downcast::<gio::SimpleAction>()
    }

    // ------------------------------------------------------------ actions

    fn setup_actions(&self) {
        let w = &self.0.window;
        let add = |name: &str, f: Box<dyn Fn(&Window)>| {
            let a = gio::SimpleAction::new(name, None);
            let weak = self.downgrade();
            a.connect_activate(move |_, _| {
                if let Some(win) = Window::from_weak(&weak) {
                    f(&win);
                }
            });
            w.add_action(&a);
        };
        add("back", Box::new(|w| if let Some(p) = w.active_pane() { p.go_back() }));
        add("forward", Box::new(|w| if let Some(p) = w.active_pane() { p.go_forward() }));
        add("up", Box::new(|w| if let Some(p) = w.active_pane() { p.go_up() }));
        add("home", Box::new(|w| {
            if let Some(p) = w.active_pane() {
                p.navigate(gio::File::for_path(tuck_core::paths::home()))
            }
        }));
        add("reload", Box::new(|w| if let Some(p) = w.active_pane() { p.reload() }));
        add("location", Box::new(|w| w.0.breadcrumb.edit()));
        add("filter", Box::new(|w| if let Some(p) = w.active_pane() { p.toggle_filter_bar() }));
        add("zoom-in", Box::new(|w| if let Some(p) = w.active_pane() { p.zoom(1) }));
        add("zoom-out", Box::new(|w| if let Some(p) = w.active_pane() { p.zoom(-1) }));
        add("zoom-reset", Box::new(|w| {
            if let Some(p) = w.active_pane() {
                p.update_settings(|v| v.zoom_reset())
            }
        }));
        add("select-all", Box::new(|w| if let Some(p) = w.active_pane() { p.select_all() }));
        add("invert-selection", Box::new(|w| if let Some(p) = w.active_pane() { p.invert_selection() }));
        add("new-tab", Box::new(|w| {
            let loc = w.active_pane().map(|p| p.location()).unwrap_or_else(|| gio::File::for_path(tuck_core::paths::home()));
            w.open_tab(loc, true);
        }));
        add("close-tab", Box::new(|w| {
            if let Some(page) = w.0.tab_view.selected_page() {
                w.close_tab(&page);
            }
        }));
        add("reopen-tab", Box::new(|w| {
            let last = w.0.closed.borrow_mut().pop();
            if let Some(s) = last {
                w.restore_tab(&s, true);
            }
        }));
        add("toggle-sidebar", Box::new(|w| w.0.outer.set_show_sidebar(!w.0.outer.shows_sidebar())));
        add("add-to-places", Box::new(|w| {
            let Some(p) = w.active_pane() else { return };
            let dir = p.selected_items().into_iter().find(|i| i.is_dir()).map(|i| i.target()).unwrap_or_else(|| p.location());
            w.0.sidebar.add_bookmark(&dir);
        }));
        add("connect-server", Box::new(|w| w.connect_to_server()));
        add("cleaner", Box::new(|w| crate::cleaner_ui::show(&w.0.window)));
        // Details columns: one toggle per optional column.
        for (id, ..) in tuck_core::settings::COLUMNS {
            let a = gio::SimpleAction::new_stateful(&format!("column-{id}"), None, &true.to_variant());
            let weak = self.downgrade();
            a.connect_change_state(move |a, v| {
                let (Some(w), Some(on)) = (Window::from_weak(&weak), v.and_then(|v| v.get::<bool>())) else { return };
                a.set_state(&on.to_variant());
                if w.0.syncing.get() {
                    return;
                }
                if let Some(p) = w.active_pane() {
                    p.update_settings(|s| s.set_column(id, on));
                }
            });
            self.0.window.add_action(&a);
        }
        add("selection-mode", Box::new(|w| {
            if let Some(p) = w.active_pane() {
                p.set_selection_mode(!p.selection_mode());
                w.sync_selection_bar();
            }
        }));
        add("toggle-folders", Box::new(|w| {
            w.0.outer.set_show_sidebar(true);
            w.0.sidebar.toggle_folders();
        }));
        add("toggle-terminal", Box::new(|w| {
            let dir = w.active_pane().and_then(|p| p.location().path());
            let was_open = w.0.terminal.is_open();
            w.0.terminal.toggle(dir.as_deref());
            if was_open && let Some(p) = w.active_pane() {
                p.focus_view();
            }
        }));
        add("toggle-info", Box::new(|w| w.0.info_split.set_show_sidebar(!w.0.info_split.shows_sidebar())));
        add("apply-view-to-all", Box::new(|w| {
            if let Some(p) = w.active_pane() {
                let mut fv = w.0.folder_views.borrow_mut();
                fv.apply_to_all(p.settings());
                let _ = save_json(&crate::views_file(), &*fv);
                drop(fv);
                w.toast("This view is now the default for all folders.");
            }
        }));
        add("switch-pane", Box::new(|w| {
            if let Some(tab) = w.current_tab()
                && tab.right.borrow().is_some() {
                    tab.right_active.set(!tab.right_active.get());
                    w.update_active_styles(&tab);
                    tab.active().focus_view();
                    w.sync();
                }
        }));
        add("about", Box::new(|w| w.show_about()));
        add("search", Box::new(|w| {
            if w.0.search.is_open() && w.0.search.entry.has_focus() {
                w.close_search();
            } else {
                w.0.search.open();
            }
        }));
        add("quick-look", Box::new(|w| {
            let first = w.active_pane().and_then(|p| p.selected_items().into_iter().next());
            if std::env::var_os("TUCK_DEBUG_KEYS").is_some() {
                eprintln!("[keys] quick-look: selected item = {:?}", first.as_ref().map(|i| i.name()));
            }
            if first.is_none() && !w.quick_look().is_visible() {
                w.toast("Select an item to preview it with Quick Look.");
                return;
            }
            w.quick_look().toggle(first);
        }));

        // Stateful actions apply to the active pane's view settings.
        let split = gio::SimpleAction::new_stateful("split", None, &false.to_variant());
        let weak = self.downgrade();
        split.connect_change_state(move |a, value| {
            let (Some(win), Some(v)) = (Window::from_weak(&weak), value.and_then(|v| v.get::<bool>())) else { return };
            a.set_state(&v.to_variant());
            if let Some(tab) = win.current_tab() {
                let loc = if v { Some(tab.active().location()) } else { None };
                win.set_split(&tab, loc);
            }
        });
        w.add_action(&split);

        self.string_action("view-mode", ViewMode::Icons.id(), |v, s| {
            if let Some(m) = ViewMode::from_id(s) {
                v.mode = m;
            }
        });
        self.string_action("sort-by", SortKey::Name.id(), |v, s| {
            if let Some(k) = SortKey::from_id(s) {
                v.sort.key = k;
            }
        });
        self.bool_action("sort-descending", |v, b| v.sort.descending = b);
        self.bool_action("folders-first", |v, b| v.sort.folders_first = b);
        self.bool_action("show-groups", |v, b| v.grouped = b);
        self.bool_action("show-hidden", |v, b| v.show_hidden = b);
        self.bool_action("show-previews", |v, b| v.previews = b);
    }

    fn string_action(&self, name: &str, initial: &str, apply: fn(&mut tuck_core::settings::ViewSettings, &str)) {
        let a = gio::SimpleAction::new_stateful(name, Some(glib::VariantTy::STRING), &initial.to_variant());
        let weak = self.downgrade();
        a.connect_change_state(move |a, value| {
            let (Some(win), Some(s)) = (Window::from_weak(&weak), value.and_then(|v| v.get::<String>())) else { return };
            a.set_state(&s.to_variant());
            if win.0.syncing.get() {
                return;
            }
            if let Some(p) = win.active_pane() {
                p.update_settings(|v| apply(v, &s));
                p.focus_view();
            }
        });
        self.0.window.add_action(&a);
    }

    fn bool_action(&self, name: &str, apply: fn(&mut tuck_core::settings::ViewSettings, bool)) {
        let a = gio::SimpleAction::new_stateful(name, None, &false.to_variant());
        let weak = self.downgrade();
        a.connect_change_state(move |a, value| {
            let (Some(win), Some(b)) = (Window::from_weak(&weak), value.and_then(|v| v.get::<bool>())) else { return };
            a.set_state(&b.to_variant());
            if win.0.syncing.get() {
                return;
            }
            if let Some(p) = win.active_pane() {
                p.update_settings(|v| apply(v, b));
            }
        });
        self.0.window.add_action(&a);
    }

    fn wire(&self) {
        let weak = self.downgrade();
        self.0.breadcrumb.connect_navigate(move |f| {
            if let Some(w) = Window::from_weak(&weak)
                && let Some(p) = w.active_pane() {
                    p.navigate(f);
                    p.focus_view();
                }
        });
        let weak = self.downgrade();
        self.0.breadcrumb.connect_error(move |m| {
            if let Some(w) = Window::from_weak(&weak) {
                w.toast(&m);
            }
        });
        let weak = self.downgrade();
        self.0.sidebar.connect_open(move |f| {
            if let Some(w) = Window::from_weak(&weak) {
                if let Some(p) = w.active_pane() {
                    p.navigate(f);
                    p.focus_view();
                }
                if w.0.outer.is_collapsed() {
                    w.0.outer.set_show_sidebar(false);
                }
            }
        });
        let weak = self.downgrade();
        self.0.sidebar.connect_drop(move |files, dest, action| {
            if let Some(w) = Window::from_weak(&weak) {
                w.handle_drop(files, dest, action);
            }
        });
        let weak = self.downgrade();
        self.0.sidebar.connect_server(move || {
            if let Some(w) = Window::from_weak(&weak) {
                w.connect_to_server();
            }
        });
        let weak = self.downgrade();
        self.0.sidebar.connect_tag(move |t| {
            // Files with this tag, anywhere in the index.
            if let Some(w) = Window::from_weak(&weak) {
                w.0.search.set_everywhere(true);
                w.0.search.open();
                w.0.search.entry.set_text(&format!("tag:{t}"));
            }
        });
        let weak = self.downgrade();
        self.0.sidebar.connect_message(move |m| {
            if let Some(w) = Window::from_weak(&weak) {
                w.toast(&m);
            }
        });

        let weak = self.downgrade();
        self.0.tab_view.connect_selected_page_notify(move |_| {
            if let Some(w) = Window::from_weak(&weak) {
                w.sync();
                if let Some(p) = w.active_pane() {
                    p.focus_view();
                }
            }
        });
        let weak = self.downgrade();
        self.0.tab_view.connect_close_page(move |tv, page| {
            let Some(w) = Window::from_weak(&weak) else { return glib::Propagation::Proceed };
            if tv.n_pages() <= 1 {
                tv.close_page_finish(page, false);
                return glib::Propagation::Stop;
            }
            let mut tabs = w.0.tabs.borrow_mut();
            if let Some(i) = tabs.iter().position(|t| &t.page == page) {
                let t = tabs.remove(i);
                w.0.closed.borrow_mut().push(t.state());
            }
            drop(tabs);
            tv.close_page_finish(page, true);
            glib::Propagation::Stop
        });
        let weak = self.downgrade();
        self.0.info_split.connect_show_sidebar_notify(move |_| {
            if let Some(w) = Window::from_weak(&weak) {
                w.sync_info();
            }
        });

        // Mouse back/forward buttons.
        let click = gtk::GestureClick::builder().button(0).propagation_phase(gtk::PropagationPhase::Capture).build();
        let weak = self.downgrade();
        click.connect_pressed(move |g, _, _, _| {
            let Some(w) = Window::from_weak(&weak) else { return };
            match g.current_button() {
                8 => {
                    if let Some(p) = w.active_pane() {
                        p.go_back();
                    }
                }
                9 => {
                    if let Some(p) = w.active_pane() {
                        p.go_forward();
                    }
                }
                _ => return,
            }
            g.set_state(gtk::EventSequenceState::Claimed);
        });
        self.0.window.add_controller(click);

        let weak = self.downgrade();
        self.0.window.connect_close_request(move |win| {
            let Some(w) = Window::from_weak(&weak) else { return glib::Propagation::Proceed };
            if !w.0.jobs.is_idle() {
                // Never quit mid-operation: ask, cancel, let jobs clean up, then close.
                let weak = w.downgrade();
                let win = win.clone();
                glib::MainContext::default().spawn_local(async move {
                    let Some(w) = Window::from_weak(&weak) else { return };
                    let ok = crate::dialogs::confirm(
                        &win.clone().upcast(),
                        "Stop File Operations?",
                        "Some operations are still running. Closing the window cancels them; anything half-finished is cleaned up.",
                        "_Stop and Close",
                    )
                    .await;
                    if ok {
                        w.0.jobs.cancel_all(move || win.close());
                    }
                });
                return glib::Propagation::Stop;
            }
            // A picker answers "cancelled" and leaves the session alone.
            let picker = w.picker();
            match picker {
                Some(pk) => pk.cancel(),
                None => w.save_session(),
            }
            WINDOWS.with(|list| list.borrow_mut().retain(|x| !Rc::ptr_eq(&x.0, &w.0)));
            glib::Propagation::Proceed
        });
        let _ = (&self.0.split_button, &self.0.info_button, gdk::Key::Escape);
    }

    // ------------------------------------------------------------ session

    pub fn restore_session(&self, session: &Session) -> bool {
        if session.tabs.is_empty() {
            return false;
        }
        if session.width > 0 && session.height > 0 {
            self.0.window.set_default_size(session.width, session.height);
        }
        if session.maximized {
            self.0.window.maximize();
        }
        for t in &session.tabs {
            self.restore_tab(t, false);
        }
        let active = session.active.min(self.0.tabs.borrow().len().saturating_sub(1));
        let page = self.0.tabs.borrow()[active].page.clone();
        self.0.tab_view.set_selected_page(&page);
        if let Some(v) = session.sidebar_visible {
            self.0.outer.set_show_sidebar(v);
        }
        if let Some(v) = session.info_visible {
            self.0.info_split.set_show_sidebar(v);
        }
        self.sync();
        true
    }

    fn save_session(&self) {
        let tabs = self.0.tabs.borrow();
        // Keep the order the user arranged in the tab bar.
        let mut ordered: Vec<&Rc<Tab>> = tabs.iter().collect();
        ordered.sort_by_key(|t| self.0.tab_view.page_position(&t.page));
        let active = self
            .0
            .tab_view
            .selected_page()
            .map(|p| self.0.tab_view.page_position(&p).max(0) as usize)
            .unwrap_or(0);
        let (width, height) = self.0.window.default_size();
        let session = Session {
            tabs: ordered.iter().map(|t| t.state()).collect(),
            active,
            width,
            height,
            maximized: self.0.window.is_maximized(),
            sidebar_visible: Some(self.0.outer.shows_sidebar()),
            info_visible: Some(self.0.info_split.shows_sidebar()),
        };
        let _ = save_json(&crate::session_file(), &session);
    }

    fn show_about(&self) {
        let about = adw::AboutDialog::builder()
            .application_name("Tuck")
            .application_icon(crate::APP_ID)
            .version(env!("CARGO_PKG_VERSION"))
            .developer_name("anuzdhk")
            .license_type(gtk::License::Gpl30)
            .comments("A fast, friendly file manager for Linux.")
            .build();
        about.add_credit_section(Some("Includes"), &["Teku by anuzdhk (GPL-3.0-or-later)"]);
        about.present(Some(&self.0.window));
    }
}

fn icon_button(icon: &str, tooltip: &str, action: &str) -> gtk::Button {
    let b = gtk::Button::builder().icon_name(icon).tooltip_text(tooltip).action_name(action).build();
    b.update_property(&[gtk::accessible::Property::Label(tooltip.split(" (").next().unwrap_or(tooltip))]);
    b
}

fn main_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let s1 = gio::Menu::new();
    s1.append(Some("New Window"), Some("app.new-window"));
    s1.append(Some("New Tab"), Some("win.new-tab"));
    s1.append(Some("Reopen Closed Tab"), Some("win.reopen-tab"));
    menu.append_section(None, &s1);
    let s2 = gio::Menu::new();
    s2.append(Some("Selection Mode"), Some("win.selection-mode"));
    s2.append(Some("Filter Items…"), Some("win.filter"));
    s2.append(Some("Select All"), Some("win.select-all"));
    s2.append(Some("Invert Selection"), Some("win.invert-selection"));
    menu.append_section(None, &s2);
    let s3 = gio::Menu::new();
    s3.append(Some("Show Hidden Files"), Some("win.show-hidden"));
    s3.append(Some("Use This View for All Folders"), Some("win.apply-view-to-all"));
    s3.append(Some("Zoom In"), Some("win.zoom-in"));
    s3.append(Some("Zoom Out"), Some("win.zoom-out"));
    s3.append(Some("Reset Zoom"), Some("win.zoom-reset"));
    menu.append_section(None, &s3);
    let places = gio::Menu::new();
    places.append(Some("Add to Places"), Some("win.add-to-places"));
    places.append(Some("Connect to Server…"), Some("win.connect-server"));
    places.append(Some("Cleaner…"), Some("win.cleaner"));
    menu.append_section(None, &places);
    let panels = gio::Menu::new();
    panels.append(Some("Folders Panel"), Some("win.toggle-folders"));
    panels.append(Some("Terminal Panel"), Some("win.toggle-terminal"));
    menu.append_section(None, &panels);
    let s4 = gio::Menu::new();
    s4.append(Some("_Preferences"), Some("app.preferences"));
    s4.append(Some("Installed Apps…"), Some("win.installed-apps"));
    s4.append(Some("Keyboard Shortcuts"), Some("app.shortcuts"));
    s4.append(Some("About Tuck"), Some("win.about"));
    menu.append_section(None, &s4);
    menu
}
