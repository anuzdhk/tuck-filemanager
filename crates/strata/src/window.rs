// SPDX-License-Identifier: GPL-3.0-or-later
//! Main window: sidebar, header (navigation, breadcrumbs, view controls),
//! tabs with optional split view, and the information panel.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use strata_core::settings::{FolderViews, Session, TabState, ViewMode, save_json};
use strata_core::sort::SortKey;

use crate::breadcrumb::Breadcrumb;
use crate::info::InfoPanel;
use crate::jobs::Jobs;
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
}

#[derive(Clone)]
pub struct Window(Rc<Inner>);

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
            .title("Strata")
            .build();

        // ---- sidebar column
        let sidebar = Sidebar::new();
        let brand = gtk::Label::builder().label("Strata").css_classes(["brand"]).build();
        let sidebar_header = adw::HeaderBar::builder()
            .show_end_title_buttons(false)
            .title_widget(&brand)
            .build();
        let sidebar_tv = adw::ToolbarView::new();
        sidebar_tv.add_top_bar(&sidebar_header);
        sidebar_tv.set_content(Some(&sidebar.widget()));

        // ---- header bar
        let back = icon_button("go-previous-symbolic", "Back (Alt+Left)", "win.back");
        let forward = icon_button("go-next-symbolic", "Forward (Alt+Right)", "win.forward");
        let up = icon_button("go-up-symbolic", "Enclosing Folder (Alt+Up)", "win.up");
        let nav = gtk::Box::builder().css_classes(["linked", "nav-buttons"]).build();
        nav.append(&back);
        nav.append(&forward);
        nav.append(&up);
        let sidebar_toggle = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text("Show Sidebar (F9)")
            .build();
        sidebar_toggle.update_property(&[gtk::accessible::Property::Label("Show Sidebar")]);

        let breadcrumb = Breadcrumb::new();

        let mut view_buttons = Vec::new();
        let views_box = gtk::Box::builder().css_classes(["linked", "view-switch"]).build();
        let mut group: Option<gtk::ToggleButton> = None;
        for (mode, icon, tip) in [
            (ViewMode::Icons, "view-grid-symbolic", "Icons (Ctrl+1)"),
            (ViewMode::Compact, "view-list-symbolic", "Compact (Ctrl+2)"),
            (ViewMode::Details, "view-continuous-symbolic", "Details (Ctrl+3)"),
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
            .icon_name("view-dual-symbolic")
            .tooltip_text("Split View (F3)")
            .action_name("win.split")
            .build();
        split_button.update_property(&[gtk::accessible::Property::Label("Split View")]);
        let info_button = gtk::ToggleButton::builder()
            .icon_name("dialog-information-symbolic")
            .tooltip_text("Information Panel (F11)")
            .build();
        info_button.update_property(&[gtk::accessible::Property::Label("Information Panel")]);
        let menu = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&main_menu())
            .tooltip_text("Main Menu (F10)")
            .primary(true)
            .build();
        menu.update_property(&[gtk::accessible::Property::Label("Main Menu")]);

        let jobs = Jobs::new();
        let header = adw::HeaderBar::builder().title_widget(&breadcrumb.widget()).build();
        header.pack_start(&sidebar_toggle);
        header.pack_start(&nav);
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
        let new_tab = icon_button("tab-new-symbolic", "New Tab (Ctrl+T)", "win.new-tab");
        new_tab.add_css_class("flat");
        tab_bar.set_end_action_widget(Some(&new_tab));
        let info = InfoPanel::new();
        let info_split = adw::OverlaySplitView::builder()
            .sidebar_position(gtk::PackType::End)
            .sidebar(&info.widget())
            .content(&tab_view)
            .min_sidebar_width(240.0)
            .max_sidebar_width(320.0)
            .sidebar_width_fraction(0.24)
            .show_sidebar(true)
            .build();
        info_split.add_css_class("info-split");

        let content_tv = adw::ToolbarView::new();
        content_tv.add_top_bar(&header);
        content_tv.add_top_bar(&tab_bar);
        content_tv.set_content(Some(&info_split));

        let outer = adw::OverlaySplitView::builder()
            .sidebar(&sidebar_tv)
            .content(&content_tv)
            .min_sidebar_width(200.0)
            .max_sidebar_width(280.0)
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
        win.setup_actions();
        win.setup_file_actions();
        win.wire();
        WINDOWS.with(|w| w.borrow_mut().push(win.clone()));
        win
    }

    pub(crate) fn downgrade(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    pub(crate) fn from_weak(w: &Weak<Inner>) -> Option<Window> {
        w.upgrade().map(Window)
    }

    /// The Strata window behind a GTK window (used by test hooks).
    pub fn find(w: &gtk::Window) -> Option<Window> {
        WINDOWS.with(|list| list.borrow().iter().find(|x| x.0.window.upcast_ref::<gtk::Window>() == w).cloned())
    }

    pub(crate) fn gtk_window(&self) -> adw::ApplicationWindow {
        self.0.window.clone()
    }

    pub(crate) fn jobs(&self) -> Jobs {
        self.0.jobs.clone()
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

    pub fn toast(&self, text: &str) {
        let t = adw::Toast::builder().title(glib::markup_escape_text(text)).timeout(5).build();
        self.0.toasts.add_toast(t);
    }

    // ---------------------------------------------------------------- tabs

    fn current_tab(&self) -> Option<Rc<Tab>> {
        let page = self.0.tab_view.selected_page()?;
        self.0.tabs.borrow().iter().find(|t| t.page == page).cloned()
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
        page.set_icon(Some(&gio::ThemedIcon::new("folder-symbolic")));
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
                    self.sync_info();
                }
            }
            PaneEvent::Settings => {
                if self.is_active(pane) {
                    self.sync_actions();
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
        self.0.window.set_title(Some(&format!("{} — Strata", folder_title(&loc))));
        self.0.back.set_sensitive(pane.can_go_back());
        self.0.forward.set_sensitive(pane.can_go_forward());
        self.0.up.set_sensitive(pane.can_go_up());
        let split = self.current_tab().is_some_and(|t| t.right.borrow().is_some());
        if let Some(a) = self.action("split") {
            a.set_state(&split.to_variant());
        }
        self.sync_actions();
        self.sync_info();
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
                p.navigate(gio::File::for_path(strata_core::paths::home()))
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
            let loc = w.active_pane().map(|p| p.location()).unwrap_or_else(|| gio::File::for_path(strata_core::paths::home()));
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
    }

    fn string_action(&self, name: &str, initial: &str, apply: fn(&mut strata_core::settings::ViewSettings, &str)) {
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

    fn bool_action(&self, name: &str, apply: fn(&mut strata_core::settings::ViewSettings, bool)) {
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
            w.save_session();
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
            .application_name("Strata")
            .application_icon("system-file-manager")
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
    let s4 = gio::Menu::new();
    s4.append(Some("Keyboard Shortcuts"), Some("app.shortcuts"));
    s4.append(Some("About Strata"), Some("win.about"));
    menu.append_section(None, &s4);
    menu
}
