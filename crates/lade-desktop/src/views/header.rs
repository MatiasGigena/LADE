use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use super::widgets::{self, hbox, label, set_accessible_label, space, vbox};
use crate::presentation::controller::Controller;
use crate::presentation::format;
use crate::presentation::keys::{AppAction, Keymap};
use crate::presentation::model::{
    ConnectionState, Load, SearchResult, SearchResultKind, SearchScope,
};
use crate::presentation::store::{Change, MainView};

pub fn connection_text(connection: &ConnectionState) -> (String, String) {
    match connection {
        ConnectionState::Connecting => (
            "… Connecting".to_owned(),
            "Connecting to the service".to_owned(),
        ),
        ConnectionState::Connected => (
            "● Connected".to_owned(),
            "Connected to the service".to_owned(),
        ),
        ConnectionState::Reconnecting { attempt } => (
            "↻ Reconnecting".to_owned(),
            format!("Reconnecting to the service (attempt {attempt})"),
        ),
        ConnectionState::Disconnected { reason } => (
            "○ Disconnected".to_owned(),
            format!("Disconnected — {reason}"),
        ),
        ConnectionState::Unavailable { reason } => (
            "! Unavailable".to_owned(),
            format!("Service unavailable — {reason}"),
        ),
        ConnectionState::LeaseHeld => (
            "! Not controlling".to_owned(),
            "Another LADE window controls this service".to_owned(),
        ),
    }
}

fn notifications_note(reason: &str) -> String {
    format!(
        ". Desktop notifications are unavailable ({reason}); Objectives that need you are marked in the Objectives list."
    )
}

const SCOPES: [SearchScope; 3] = [
    SearchScope::Objectives,
    SearchScope::Files,
    SearchScope::Content,
];

fn search_placeholder(scope: SearchScope) -> &'static str {
    match scope {
        SearchScope::Objectives => "Search Objectives…",
        SearchScope::Files => "Search file names…",
        SearchScope::Content => "Search file content…",
    }
}

pub type OpenerHandler = Rc<dyn Fn(&gtk::Widget)>;

pub struct Header {
    pub root: gtk::Box,
    pub viewport: gtk::ScrolledWindow,
    row: gtk::Box,
    brand: gtk::Box,
    controller: Rc<Controller>,
    workbench_tab: gtk::ToggleButton,
    activity_tab: gtk::ToggleButton,
    terminal_tab: gtk::ToggleButton,
    status: gtk::Label,
    search: gtk::Box,
    search_slot: gtk::Box,
    search_holder: gtk::Box,
    search_button: gtk::MenuButton,
    search_entry: gtk::SearchEntry,
    scope: gtk::DropDown,
    popover: gtk::Popover,
    popover_heading: gtk::Label,
    results: gtk::ListBox,
    popover_status: gtk::Label,
    shown_results: RefCell<Vec<SearchResult>>,
    nav: gtk::Box,
    profile_name: gtk::Label,
    shortcuts_button: gtk::Button,
    exit_button: gtk::Button,
    menu_button: gtk::MenuButton,
    menu_shortcuts: (gtk::Button, gtk::Label),
    menu_exit: (gtk::Button, gtk::Label),
    on_shortcuts: RefCell<Option<OpenerHandler>>,
    on_exit: RefCell<Option<OpenerHandler>>,
    compact: Cell<bool>,
    narrow: Cell<bool>,
    updating: Cell<bool>,
}

fn draw_mark(area: &gtk::DrawingArea) {
    area.set_draw_func(|area, cr, width, height| {
        let (w, h) = (f64::from(width), f64::from(height));
        let ink = area.color();
        cr.set_source_rgb(ink.red().into(), ink.green().into(), ink.blue().into());
        cr.set_line_join(gtk::cairo::LineJoin::Miter);
        cr.set_line_width(2.4);
        cr.move_to(w / 2.0, 2.0);
        cr.line_to(w - 1.5, h - 1.5);
        cr.line_to(1.5, h - 1.5);
        cr.close_path();
        let _ = cr.stroke();
        cr.set_line_width(1.8);
        cr.move_to(w / 2.0, h * 0.45);
        cr.line_to(w * 0.70, h - 5.5);
        cr.line_to(w * 0.30, h - 5.5);
        cr.close_path();
        let _ = cr.stroke();
    });
}

fn icon_button(icon: &str, name: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.add_css_class("icon-button");
    button.set_valign(gtk::Align::Center);
    set_accessible_label(&button, name);
    button
}

fn icon_menu_button(icon: &str, name: &str, popover: &gtk::Popover) -> gtk::MenuButton {
    let button = gtk::MenuButton::builder()
        .icon_name(icon)
        .popover(popover)
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    button.add_css_class("icon-button");
    button.set_tooltip_text(Some(name));
    set_accessible_label(&button, name);
    button
}

fn menu_item(text: &str, destructive: bool) -> (gtk::Button, gtk::Label) {
    let row = hbox(space::GROUP);
    let name = label(text, &[]);
    name.set_wrap(false);
    name.set_hexpand(true);
    row.append(&name);
    let accel = label("", &["meta"]);
    accel.set_wrap(false);
    row.append(&accel);
    let button = gtk::Button::new();
    button.set_child(Some(&row));
    button.add_css_class("quiet");
    button.add_css_class("menu-item");
    if destructive {
        button.add_css_class("destructive-quiet");
    }
    set_accessible_label(&button, text);
    (button, accel)
}

impl Header {
    pub fn new(controller: &Rc<Controller>) -> Rc<Self> {
        let root = vbox(0);
        root.add_css_class("lade-header");
        set_accessible_label(&root, "Header");
        let row = hbox(space::SECTION);
        row.set_height_request(56);
        root.append(&row);

        let brand = hbox(space::ITEM);
        brand.add_css_class("brand");
        let mark = gtk::DrawingArea::new();
        mark.set_content_width(26);
        mark.set_content_height(23);
        mark.set_valign(gtk::Align::Center);
        draw_mark(&mark);
        set_accessible_label(&mark, "LADE mark");
        brand.append(&mark);
        let wordmark = gtk::Label::new(Some("LADE"));
        wordmark.add_css_class("wordmark");
        brand.append(&wordmark);
        row.append(&brand);

        let nav = hbox(0);
        nav.add_css_class("nav");
        nav.add_css_class("segmented");
        nav.set_valign(gtk::Align::Center);
        nav.set_margin_start(space::PAGE);
        let workbench_tab = gtk::ToggleButton::with_label("Workbench");
        let activity_tab = gtk::ToggleButton::with_label("Activity");
        activity_tab.set_group(Some(&workbench_tab));
        let terminal_tab = gtk::ToggleButton::with_label("Terminal");
        terminal_tab.set_group(Some(&workbench_tab));
        for tab in [&workbench_tab, &activity_tab, &terminal_tab] {
            tab.add_css_class("nav-tab");
            nav.append(tab);
        }
        workbench_tab.set_active(true);
        row.append(&nav);

        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        row.append(&spacer);

        let status = label("", &["connection", "meta"]);
        status.set_wrap(false);
        status.set_ellipsize(gtk::pango::EllipsizeMode::End);
        status.set_width_chars(14);
        status.set_valign(gtk::Align::Center);
        row.append(&status);

        let search = hbox(0);
        search.add_css_class("search");
        search.set_valign(gtk::Align::Center);
        let scope = gtk::DropDown::from_strings(&SCOPES.map(format::scope_label));
        scope.add_css_class("search-scope");
        scope.set_tooltip_text(Some("Search scope"));
        set_accessible_label(&scope, "Search scope");
        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text(search_placeholder(SCOPES[0]))
            .width_chars(12)
            .max_width_chars(24)
            .build();
        search_entry.set_hexpand(false);
        set_accessible_label(&search_entry, "Global search");
        search.append(&search_entry);
        search.append(&scope);
        let search_slot = hbox(0);
        search_slot.set_valign(gtk::Align::Center);
        search_slot.append(&search);
        row.append(&search_slot);

        let search_popover = gtk::Popover::new();
        search_popover.add_css_class("search-popover");
        let search_holder = hbox(0);
        search_popover.set_child(Some(&search_holder));
        let search_button = icon_menu_button("system-search-symbolic", "Search", &search_popover);
        row.append(&search_button);

        let popover = gtk::Popover::new();
        popover.add_css_class("search-results");
        popover.set_parent(&search);
        popover.set_position(gtk::PositionType::Bottom);
        popover.set_has_arrow(false);
        let popover_box = vbox(space::ITEM);
        popover_box.set_size_request(380, -1);
        let popover_heading = widgets::heading("", "popover-heading");
        popover_box.append(&popover_heading);
        let popover_status = label("", &["meta"]);
        popover_box.append(&popover_status);
        let results = gtk::ListBox::new();
        results.add_css_class("plain-list");
        results.set_selection_mode(gtk::SelectionMode::None);
        set_accessible_label(&results, "Search results");
        let results_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(420)
            .child(&results)
            .build();
        popover_box.append(&results_scroll);
        popover.set_child(Some(&popover_box));

        let shortcuts_button = icon_button("input-keyboard-symbolic", "Keyboard shortcuts");
        row.append(&shortcuts_button);
        let exit_button = icon_button("system-shutdown-symbolic", "Exit LADE and stop its agents");
        row.append(&exit_button);

        let menu = gtk::Popover::new();
        menu.add_css_class("header-menu");
        let menu_box = vbox(0);
        let menu_shortcuts = menu_item("Keyboard shortcuts", false);
        let menu_exit = menu_item("Exit LADE…", true);
        menu_box.append(&menu_shortcuts.0);
        menu_box.append(&menu_exit.0);
        menu.set_child(Some(&menu_box));
        let menu_button =
            icon_menu_button("open-menu-symbolic", "Keyboard shortcuts and Exit", &menu);
        row.append(&menu_button);

        let profile = hbox(space::ITEM);
        profile.add_css_class("profile");
        profile.set_valign(gtk::Align::Center);
        let real_name = gtk::glib::real_name().to_string_lossy().into_owned();
        let display_name = if real_name.trim().is_empty() || real_name == "Unknown" {
            gtk::glib::user_name().to_string_lossy().into_owned()
        } else {
            real_name
        };
        let initials: String = display_name
            .split_whitespace()
            .take(2)
            .filter_map(|word| word.chars().next())
            .flat_map(char::to_uppercase)
            .collect();
        let avatar = gtk::Label::new(Some(&initials));
        avatar.add_css_class("avatar");
        avatar.set_valign(gtk::Align::Center);
        let avatar_label = format!("Placeholder avatar for {display_name}");
        avatar.set_tooltip_text(Some(&avatar_label));
        set_accessible_label(&avatar, &avatar_label);
        profile.append(&avatar);
        let name = gtk::Label::new(Some(&display_name));
        name.add_css_class("profile-name");
        name.set_valign(gtk::Align::Center);
        profile.append(&name);
        row.append(&profile);

        let viewport = gtk::ScrolledWindow::builder()
            .child(&root)
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .build();
        let header = Rc::new(Self {
            root,
            viewport,
            row,
            brand,
            controller: controller.clone(),
            workbench_tab,
            activity_tab,
            terminal_tab,
            status,
            search,
            search_slot,
            search_holder,
            search_button,
            search_entry,
            scope,
            popover,
            popover_heading,
            results,
            popover_status,
            shown_results: RefCell::new(Vec::new()),
            nav,
            profile_name: name,
            shortcuts_button,
            exit_button,
            menu_button,
            menu_shortcuts,
            menu_exit,
            on_shortcuts: RefCell::new(None),
            on_exit: RefCell::new(None),
            compact: Cell::new(false),
            narrow: Cell::new(false),
            updating: Cell::new(false),
        });
        header.connect();
        header.refresh_connection();
        header
    }

    fn connect(self: &Rc<Self>) {
        for (tab, view) in [
            (&self.workbench_tab, MainView::Workbench),
            (&self.activity_tab, MainView::Activity),
            (&self.terminal_tab, MainView::Terminal),
        ] {
            let this = Rc::downgrade(self);
            tab.connect_toggled(move |tab| {
                if let Some(this) = this.upgrade()
                    && tab.is_active()
                    && !this.updating.get()
                {
                    this.controller.set_view(view);
                }
            });
        }
        let this = Rc::downgrade(self);
        self.search_entry.connect_activate(move |entry| {
            if let Some(this) = this.upgrade() {
                this.controller.search(this.selected_scope(), &entry.text());
                this.popover.popup();
            }
        });
        let this = Rc::downgrade(self);
        self.scope.connect_selected_notify(move |_| {
            if let Some(this) = this.upgrade() {
                this.search_entry
                    .set_placeholder_text(Some(search_placeholder(this.selected_scope())));
            }
        });
        let entry = self.search_entry.clone();
        self.popover.connect_closed(move |_| {
            entry.grab_focus();
        });
        let this = Rc::downgrade(self);
        self.results.connect_row_activated(move |_, row| {
            if let Some(this) = this.upgrade() {
                let index = usize::try_from(row.index()).unwrap_or(usize::MAX);
                let result = this.shown_results.borrow().get(index).cloned();
                if let Some(result) = result {
                    this.popover.popdown();
                    this.search_button.popdown();
                    this.controller.open_search_result(&result);
                }
            }
        });
        for (button, exit) in [(&self.shortcuts_button, false), (&self.exit_button, true)] {
            let this = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(this) = this.upgrade() {
                    this.run(exit);
                }
            });
        }
        for (button, exit) in [(&self.menu_shortcuts.0, false), (&self.menu_exit.0, true)] {
            let this = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(this) = this.upgrade() {
                    this.menu_button.popdown();
                    this.run(exit);
                }
            });
        }
    }

    fn selected_scope(&self) -> SearchScope {
        SCOPES[self.scope.selected() as usize % SCOPES.len()]
    }

    pub fn set_on_shortcuts(&self, handler: OpenerHandler) {
        *self.on_shortcuts.borrow_mut() = Some(handler);
    }

    pub fn set_on_exit(&self, handler: OpenerHandler) {
        *self.on_exit.borrow_mut() = Some(handler);
    }

    pub fn show_shortcuts(&self) {
        self.run(false);
    }

    pub fn request_exit(&self) {
        self.run(true);
    }

    fn run(&self, exit: bool) {
        let (handler, wide) = if exit {
            (self.on_exit.borrow().clone(), &self.exit_button)
        } else {
            (self.on_shortcuts.borrow().clone(), &self.shortcuts_button)
        };
        let opener: gtk::Widget = if self.compact.get() {
            self.menu_button.clone().upcast()
        } else {
            wide.clone().upcast()
        };
        if let Some(handler) = handler {
            handler(&opener);
        }
    }

    pub fn refresh(&self, changes: &[Change]) {
        for change in changes {
            match change {
                Change::Connection => self.refresh_connection(),
                Change::DesktopAttention => self.refresh_notifications(),
                Change::View => self.refresh_view(),
                Change::Search => self.refresh_search(),
                _ => {}
            }
        }
    }

    pub fn set_compact(&self, compact: bool) {
        if self.compact.replace(compact) == compact {
            return;
        }
        self.row
            .set_spacing(if compact { space::ITEM } else { space::SECTION });
        self.nav
            .set_margin_start(if compact { space::ITEM } else { space::PAGE });
        let (from, to) = if compact {
            (&self.search_slot, &self.search_holder)
        } else {
            (&self.search_holder, &self.search_slot)
        };
        self.popover.popdown();
        self.search_button.popdown();
        from.remove(&self.search);
        to.append(&self.search);
        self.search_slot.set_visible(!compact);
        self.search_button.set_visible(compact);
        self.shortcuts_button.set_visible(!compact);
        self.exit_button.set_visible(!compact);
        self.menu_button.set_visible(compact);
        self.profile_name.set_visible(!compact);
        self.status.set_width_chars(if compact { -1 } else { 14 });
        self.refresh_connection();
    }

    pub fn set_narrow(&self, narrow: bool) {
        if self.narrow.replace(narrow) == narrow {
            return;
        }
        if narrow {
            self.row.remove(&self.nav);
            self.root.append(&self.nav);
            self.nav.set_margin_start(0);
        } else {
            self.root.remove(&self.nav);
            self.row.insert_child_after(&self.nav, Some(&self.brand));
            self.nav.set_margin_start(if self.compact.get() {
                space::ITEM
            } else {
                2 * space::PAGE
            });
        }
        self.search_entry
            .set_width_chars(if narrow { 8 } else { 12 });
        self.search_entry
            .set_max_width_chars(if narrow { 18 } else { 34 });
        if let Some(child) = self.popover.child() {
            child.set_size_request(if narrow { 280 } else { 380 }, -1);
        }
    }

    pub fn set_shortcut_hints(&self, keymap: &Keymap) {
        let shortcuts = keymap.label(AppAction::ShowShortcuts);
        let search = keymap.label(AppAction::FocusSearch);
        let exit = keymap.label(AppAction::Exit);
        self.shortcuts_button
            .set_tooltip_text(Some(&format!("Keyboard shortcuts ({shortcuts})")));
        self.search_entry
            .set_tooltip_text(Some(&format!("Global search ({search})")));
        self.search_button
            .set_tooltip_text(Some(&format!("Search ({search})")));
        self.exit_button.set_tooltip_text(Some(&format!(
            "Exit: stop the service, its agents and terminals ({exit}). Closing the window keeps them running."
        )));
        self.menu_shortcuts.1.set_text(&shortcuts);
        self.menu_exit.1.set_text(&exit);
        self.menu_exit.0.set_tooltip_text(Some(
            "Stop the service, its agents and terminals. Closing the window keeps them running.",
        ));
        self.terminal_tab.set_tooltip_text(Some(&format!(
            "Terminal in the Objective sandbox ({})",
            keymap.label(AppAction::ShowTerminal)
        )));
    }

    pub fn focus_search(&self) {
        if self.compact.get() {
            self.search_button.popup();
        }
        self.search_entry.grab_focus();
    }

    fn refresh_view(&self) {
        let view = self.controller.store.state().ui.view;
        self.updating.set(true);
        match view {
            MainView::Workbench => self.workbench_tab.set_active(true),
            MainView::Activity => self.activity_tab.set_active(true),
            MainView::Terminal => self.terminal_tab.set_active(true),
        }
        self.updating.set(false);
    }

    fn refresh_connection(&self) {
        let state = self.controller.store.state();
        let (short, mut full) = connection_text(&state.connection);
        if let Some(reason) = &state.notifications_unavailable {
            full.push_str(&notifications_note(reason));
        }
        let shown = if self.compact.get() {
            short
                .split_once(' ')
                .map_or(short.as_str(), |(sign, _)| sign)
        } else {
            short.as_str()
        };
        self.status.set_text(shown);
        self.status.set_tooltip_text(Some(&full));
        set_accessible_label(&self.status, &format!("Connection status: {full}"));
    }

    fn refresh_notifications(&self) {
        self.refresh_connection();
        let reason = self
            .controller
            .store
            .state()
            .notifications_unavailable
            .clone();
        if let Some(reason) = reason {
            widgets::announce(
                &self.status,
                notifications_note(&reason).trim_start_matches(". "),
                gtk::AccessibleAnnouncementPriority::Low,
            );
        }
    }

    fn refresh_search(&self) {
        let state = self.controller.store.state();
        while let Some(row) = self.results.row_at_index(0) {
            self.results.remove(&row);
        }
        let mut shown = Vec::new();
        match &state.search {
            Load::NotLoaded => {
                self.popover_heading.set_text("Search");
                self.popover_status.set_text("");
            }
            Load::Loading => {
                self.popover_heading.set_text("Searching…");
                self.popover_status.set_text("");
            }
            Load::Failed(message) => {
                self.popover_heading.set_text("Search failed");
                self.popover_status.set_text(message);
            }
            Load::Loaded(results) => {
                self.popover_heading.set_text(&results.scope_label);
                let count = results.results.len();
                self.popover_status
                    .set_text(&match (count, results.truncated) {
                        (0, _) => format!("No results for “{}”", results.query),
                        (n, true) => format!(
                            "First {n} results for “{}” — refine to see more",
                            results.query
                        ),
                        (1, false) => format!("1 result for “{}”", results.query),
                        (n, false) => format!("{n} results for “{}”", results.query),
                    });
                for result in &results.results {
                    let row = gtk::ListBoxRow::new();
                    let content = vbox(space::TIGHT);
                    content.add_css_class("result-row");
                    let primary = match (&result.path, result.line) {
                        (Some(path), Some(line)) => format!("{}:{line}", path.display),
                        (Some(path), None) => path.display.clone(),
                        _ => result.preview.clone(),
                    };
                    content.append(&widgets::line_label(
                        &primary,
                        &[],
                        gtk::pango::EllipsizeMode::Middle,
                    ));
                    if result.path.is_some() && result.kind != SearchResultKind::File {
                        content.append(&widgets::line_label(
                            &result.preview,
                            &["meta", "mono"],
                            gtk::pango::EllipsizeMode::End,
                        ));
                    }
                    row.set_child(Some(&content));
                    set_accessible_label(
                        &row,
                        &format!("{primary}, {}", format::scope_label(results.scope)),
                    );
                    self.results.append(&row);
                    shown.push(result.clone());
                }
                let focus_in_popover = self
                    .popover
                    .root()
                    .and_then(|r| r.focus())
                    .is_some_and(|f| f.is_ancestor(&self.popover));
                if focus_in_popover && let Some(first) = self.results.row_at_index(0) {
                    first.grab_focus();
                }
            }
        }
        self.results.set_visible(!shown.is_empty());
        *self.shown_results.borrow_mut() = shown;
    }
}
