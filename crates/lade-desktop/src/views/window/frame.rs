//! The window frame: the body and centre stacks, the notice bar, and the responsive layout.

use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::presentation::layout::{self, LayoutClass, Region};
use crate::presentation::state::model::{ConnectionState, NoticeLevel, NoticeScope};
use crate::presentation::state::store::MainView;

use crate::views::{App, dialogs, widgets};

impl App {
    pub(in crate::views) fn render_centre(&self) {
        let view = self.store.state().ui.view;
        self.centre.set_visible_child_name(match view {
            MainView::Workbench => "workbench",
            MainView::Activity => "activity",
            MainView::Terminal => "terminal",
        });
    }

    pub(in crate::views) fn render_body(self: &Rc<Self>) {
        let state = self.store.state();
        let has_workspace = state.active_workspace().is_some();
        let connection = state.connection.clone();
        drop(state);
        widgets::clear(&self.empty);
        if has_workspace {
            self.body.set_visible_child_name("main");
            return;
        }
        let (title, body, action): (&str, String, Option<(&str, bool)>) = match &connection {
            ConnectionState::Connecting => ("Connecting to the LADE service…", String::new(), None),
            ConnectionState::Unavailable { reason } => (
                "Service unavailable",
                format!("{reason} Your Objectives and drafts are kept by the service, not this window."),
                Some(("Retry", true)),
            ),
            ConnectionState::Disconnected { reason } => (
                "Disconnected from the service",
                reason.clone(),
                Some(("Retry", true)),
            ),
            ConnectionState::LeaseHeld => (
                "Another LADE window controls this service",
                "Only one window can control the LADE service at a time. Close the other window, then retry here. Nothing is lost.".to_owned(),
                Some(("Retry", true)),
            ),
            ConnectionState::Reconnecting { attempt } => (
                "Reconnecting…",
                format!("Attempt {attempt}. Nothing is lost while the window waits."),
                None,
            ),
            ConnectionState::Connected => (
                "Open a Git repository to start",
                "LADE works in one Git repository at a time. Each Objective gets its own branch and worktree; your checkout is not modified.".to_owned(),
                Some(("Open repository…", false)),
            ),
        };
        self.empty.append(&widgets::heading(title, "empty-title"));
        if !body.is_empty() {
            self.empty.append(&widgets::label(&body, &["secondary"]));
        }
        if let Some((text, retry)) = action {
            let button = widgets::button(text, &["primary"]);
            button.set_halign(gtk::Align::Start);
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(app) = weak.upgrade() {
                    if retry {
                        app.controller.retry_connection();
                    } else {
                        dialogs::open_workspace(app.window.upcast_ref(), &app.controller);
                    }
                }
            });
            self.empty.append(&button);
        }
        if matches!(connection, ConnectionState::Connected) {
            let help = widgets::button("Costs and data", &["quiet"]);
            help.set_halign(gtk::Align::Start);
            help.connect_clicked(dialogs::costs_and_data);
            self.empty.append(&help);
        }
        self.body.set_visible_child_name("empty");
    }

    pub(in crate::views) fn render_notice(self: &Rc<Self>) {
        widgets::clear(&self.notice);
        let state = self.store.state();
        let total = state.notices_received;
        let latest = state.notices.last().cloned();
        drop(state);
        let Some(notice) = latest.filter(|_| total > self.notices_seen.get()) else {
            self.notice.set_visible(false);
            return;
        };
        let sign = match notice.level {
            NoticeLevel::Info => "i",
            NoticeLevel::Warning => "⚠",
            NoticeLevel::Error => "✕",
        };
        self.notice.add_css_class("notice-bar");
        for class in ["notice-warning", "notice-error"] {
            self.notice.remove_css_class(class);
        }
        match notice.level {
            NoticeLevel::Warning => self.notice.add_css_class("notice-warning"),
            NoticeLevel::Error => self.notice.add_css_class("notice-error"),
            NoticeLevel::Info => {}
        }
        self.notice.append(&gtk::Label::new(Some(sign)));
        let text = widgets::label(&notice.message, &[]);
        text.set_hexpand(true);
        self.notice.append(&text);
        if notice.scope == NoticeScope::WorkspaceOpen {
            let retry = widgets::wrapping_button("Choose repository…", &[]);
            let weak = Rc::downgrade(self);
            retry.connect_clicked(move |_| {
                if let Some(app) = weak.upgrade() {
                    dialogs::open_workspace(app.window.upcast_ref(), &app.controller);
                }
            });
            self.notice.append(&retry);
        }
        let dismiss = widgets::button("Dismiss", &["quiet"]);
        let weak = Rc::downgrade(self);
        dismiss.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                app.notices_seen.set(total);
                app.render_notice();
            }
        });
        self.notice.append(&dismiss);
        self.notice.set_visible(true);
        widgets::announce(
            &self.window,
            &notice.message,
            gtk::AccessibleAnnouncementPriority::Low,
        );
    }

    pub(in crate::views) fn apply_width(&self, width: i32) {
        let sizes = layout::wide_sizes(width);
        if self.sizes.replace(sizes) != sizes {
            self.workbench.set_file_list_width(sizes.file_list);
            if self.store.state().ui.layout == LayoutClass::Wide {
                self.place_wide_panes();
            }
        }
        self.controller.set_layout_width(width);
        let narrow = width > 0 && width < 600;
        self.header.set_narrow(narrow);
        self.workbench.set_narrow(narrow);
        for (region, button) in &self.region_switches {
            button.set_label(if narrow && *region == Region::Inspector {
                "Review"
            } else {
                region.switch_label()
            });
        }
        if narrow {
            self.window.add_css_class("narrow");
        } else {
            self.window.remove_css_class("narrow");
        }
    }

    /// Without a selected Objective the wide inspector would be an empty column.
    fn inspector_has_content(&self) -> bool {
        let state = self.store.state();
        state.ui.layout == LayoutClass::Compact || state.ui.selected.is_some()
    }

    pub(in crate::views) fn sync_inspector(&self) {
        let (class, region, details) = {
            let state = self.store.state();
            (
                state.ui.layout,
                state.ui.compact_region,
                state.ui.compact_details,
            )
        };
        let shown =
            layout::composition(class, region, details).inspector && self.inspector_has_content();
        if shown == self.inspector.root.is_visible() {
            return;
        }
        self.show_inspector(shown);
        if shown && class != LayoutClass::Compact {
            self.place_wide_panes();
        }
    }

    fn show_inspector(&self, shown: bool) {
        self.inspector.root.set_visible(shown);
        if shown {
            self.centre_region.remove_css_class("alone");
        } else {
            self.centre_region.add_css_class("alone");
        }
    }

    fn place_wide_panes(&self) {
        let sizes = self.sizes.get();
        self.paned.set_position(sizes.rail);
        let right = self.right.clone();
        let inspector = sizes.inspector;
        glib::idle_add_local_once(move || {
            right.set_position(right.max_position() - inspector);
            right.queue_resize();
        });
    }

    pub(in crate::views) fn render_layout(&self) {
        let (class, region, details) = {
            let state = self.store.state();
            (
                state.ui.layout,
                state.ui.compact_region,
                state.ui.compact_details,
            )
        };
        let c = layout::composition(class, region, details);
        let focus_region = self.focused_region();
        self.rail.root.set_visible(c.rail);
        self.centre_region.set_visible(c.centre);
        self.show_inspector(c.inspector && self.inspector_has_content());
        self.right.set_visible(c.centre || c.inspector);
        self.region_bar.set_visible(c.region_switches);
        self.header.set_compact(c.compact_header);
        self.workbench
            .set_objective_details(c.objective_details, c.details_toggle);
        self.updating_layout.set(true);
        let shown = match region {
            Region::Header => Region::Main,
            other => other,
        };
        for (r, toggle) in &self.region_switches {
            toggle.set_active(*r == shown);
        }
        self.updating_layout.set(false);
        if class == LayoutClass::Compact {
            self.window.add_css_class("compact");
        } else {
            self.window.remove_css_class("compact");
            self.place_wide_panes();
        }
        if let Some(previous) = focus_region
            && previous != Region::Header
            && !self.region_widget(previous).is_visible()
        {
            self.focus_region(shown);
        }
    }
}
