//! Header band: mark + LADE, Workbench/Activity/Terminal navigation, connection status, global
//! search, shortcuts, Exit and the profile (circular placeholder avatar + name).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use super::widgets::{self, hbox, label, set_accessible_label, space, vbox};
use crate::presentation::controller::Controller;
use crate::presentation::format;
use crate::presentation::keys::{AppAction, Keymap};
use crate::presentation::model::{ConnectionState, Load, SearchResult, SearchScope};
use crate::presentation::preview::PREVIEW_LABEL;
use crate::presentation::store::{Change, MainView};

/// Short header text and the full statement (tooltip, accessible name and preview band).
pub fn connection_text(connection: &ConnectionState, preview: bool) -> (String, String) {
    let (short, full) = match connection {
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
    };
    if !preview {
        return (short, full);
    }
    match connection {
        ConnectionState::Connected => ("◇ Preview data".to_owned(), PREVIEW_LABEL.to_owned()),
        _ => (
            "◇ Preview data".to_owned(),
            format!("Preview data — {}", full.to_lowercase()),
        ),
    }
}

const SCOPES: [SearchScope; 3] = [
    SearchScope::Objectives,
    SearchScope::Files,
    SearchScope::Content,
];

pub struct Header {
    pub root: gtk::Box,
    /// Bounds the header's minimum width (no visible scrollbar) so the window can always narrow
    /// into the compact composition; the compact header then fits without clipping.
    pub viewport: gtk::ScrolledWindow,
    controller: Rc<Controller>,
    workbench_tab: gtk::ToggleButton,
    activity_tab: gtk::ToggleButton,
    terminal_tab: gtk::ToggleButton,
    status: gtk::Label,
    pub search_entry: gtk::SearchEntry,
    scope: gtk::DropDown,
    popover: gtk::Popover,
    popover_heading: gtk::Label,
    results: gtk::ListBox,
    popover_status: gtk::Label,
    shown_results: RefCell<Vec<SearchResult>>,
    nav: gtk::Box,
    profile_name: gtk::Label,
    pub shortcuts_button: gtk::Button,
    /// Explicit Exit: stops the service and its agents (closing the window does not).
    pub exit_button: gtk::Button,
    updating: Cell<bool>,
}

fn draw_mark(area: &gtk::DrawingArea) {
    // The existing LADE triangle mark as a simple symbolic outline (outer and inner triangle).
    area.set_draw_func(|_, cr, width, height| {
        let (w, h) = (f64::from(width), f64::from(height));
        cr.set_source_rgb(0.0, 0.0, 0.0);
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

impl Header {
    pub fn new(controller: &Rc<Controller>) -> Rc<Self> {
        let root = hbox(space::SECTION);
        root.add_css_class("lade-header");
        set_accessible_label(&root, "Header");

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
        root.append(&brand);

        let nav = hbox(space::PAGE);
        nav.add_css_class("nav");
        nav.set_margin_start(2 * space::PAGE);
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
        root.append(&nav);

        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        root.append(&spacer);

        let status = label("", &["connection", "meta"]);
        status.set_wrap(false);
        status.set_ellipsize(gtk::pango::EllipsizeMode::End);
        // Keeps "Preview data" / "Disconnected" readable even when the header is tight.
        status.set_width_chars(14);
        status.set_valign(gtk::Align::Center);
        root.append(&status);

        let search = hbox(0);
        search.add_css_class("search");
        search.set_valign(gtk::Align::Center);
        let scope = gtk::DropDown::from_strings(&["Objectives", "Files", "Content"]);
        scope.add_css_class("search-scope");
        scope.set_tooltip_text(Some("Search scope"));
        set_accessible_label(&scope, "Search scope");
        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text("Search objectives, files, or content…")
            .width_chars(12)
            .max_width_chars(34)
            .build();
        search_entry.set_hexpand(false);
        set_accessible_label(&search_entry, "Global search");
        search.append(&search_entry);
        search.append(&scope);
        root.append(&search);

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

        let shortcuts_button = gtk::Button::from_icon_name("input-keyboard-symbolic");
        shortcuts_button.add_css_class("icon-button");
        shortcuts_button.set_valign(gtk::Align::Center);
        set_accessible_label(&shortcuts_button, "Keyboard shortcuts");
        root.append(&shortcuts_button);
        let exit_button = gtk::Button::from_icon_name("system-shutdown-symbolic");
        exit_button.add_css_class("icon-button");
        exit_button.set_valign(gtk::Align::Center);
        set_accessible_label(&exit_button, "Exit LADE and stop its agents");
        root.append(&exit_button);

        let profile = hbox(space::ITEM);
        profile.add_css_class("profile");
        profile.set_valign(gtk::Align::Center);
        let avatar = gtk::Label::new(Some("MG"));
        avatar.add_css_class("avatar");
        avatar.set_valign(gtk::Align::Center);
        avatar.set_tooltip_text(Some("Placeholder avatar for Matias Gigena"));
        set_accessible_label(&avatar, "Placeholder avatar for Matias Gigena");
        profile.append(&avatar);
        let name = gtk::Label::new(Some("Matias Gigena"));
        name.add_css_class("profile-name");
        name.set_valign(gtk::Align::Center);
        profile.append(&name);
        root.append(&profile);

        let viewport = gtk::ScrolledWindow::builder()
            .child(&root)
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .build();
        let header = Rc::new(Self {
            root,
            viewport,
            controller: controller.clone(),
            workbench_tab,
            activity_tab,
            terminal_tab,
            status,
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
                let scope = SCOPES[this.scope.selected() as usize % SCOPES.len()];
                this.controller.search(scope, &entry.text());
                this.popover.popup();
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
                    this.controller.open_search_result(&result);
                }
            }
        });
    }

    pub fn refresh(&self, changes: &[Change]) {
        for change in changes {
            match change {
                Change::Connection => self.refresh_connection(),
                Change::View => self.refresh_view(),
                Change::Search => self.refresh_search(),
                _ => {}
            }
        }
    }

    /// Tightens the header for the compact composition. Only the profile name is hidden; the
    /// avatar keeps its accessible name.
    pub fn set_compact(&self, compact: bool) {
        self.root
            .set_spacing(if compact { space::ITEM } else { space::SECTION });
        self.nav.set_margin_start(if compact {
            space::ITEM
        } else {
            2 * space::PAGE
        });
        self.nav
            .set_spacing(if compact { space::SECTION } else { space::PAGE });
        self.profile_name.set_visible(!compact);
    }

    /// Shows the current bindings where the header offers the action.
    pub fn set_shortcut_hints(&self, keymap: &Keymap) {
        self.shortcuts_button.set_tooltip_text(Some(&format!(
            "Keyboard shortcuts ({})",
            keymap.label(AppAction::ShowShortcuts)
        )));
        self.search_entry.set_tooltip_text(Some(&format!(
            "Global search ({})",
            keymap.label(AppAction::FocusSearch)
        )));
        self.exit_button.set_tooltip_text(Some(&format!(
            "Exit: stop the service, its agents and terminals ({}). Closing the window keeps them running.",
            keymap.label(AppAction::Exit)
        )));
        self.terminal_tab.set_tooltip_text(Some(&format!(
            "Terminal in the Objective sandbox ({})",
            keymap.label(AppAction::ShowTerminal)
        )));
    }

    pub fn focus_search(&self) {
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
        let (short, full) = connection_text(&state.connection, state.is_preview());
        self.status.set_text(&short);
        self.status.set_tooltip_text(Some(&full));
        set_accessible_label(&self.status, &format!("Connection status: {full}"));
        // In preview the band below the header is the one statement of the data source.
        self.status.set_visible(!state.is_preview());
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
                    if result.path.is_some()
                        && result.kind != crate::presentation::model::SearchResultKind::File
                    {
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
        *self.shown_results.borrow_mut() = shown;
    }
}
