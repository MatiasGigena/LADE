//! Native GTK4 workbench views. Views render the presentation store and emit intents through the
//! controller; they never own domain state or decide authority.
//!
//! Composition for WP-G:
//! ```ignore
//! let store = Store::new(PresentationState::new(ConnectionState::Connecting));
//! let app = views::App::new(&application, store.clone(), sink);
//! let updates = app.update_sender(); // Send + Clone: hand to the network thread
//! app.present();
//! ```

mod activity;
pub mod bridge;
mod dialogs;
mod header;
mod inspector;
mod rail;
mod viewer;
mod widgets;
mod workbench;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::presentation::controller::Controller;
use crate::presentation::intents::IntentSink;
use crate::presentation::keys::{self, AppAction};
use crate::presentation::model::{ConnectionState, DraftId, NoticeLevel, ServiceNotice};
use crate::presentation::store::{Change, MainView, Store, StoreUpdate};

const CSS: &str = include_str!("../assets/style.css");

/// Window widths (logical px) for the compact compositions.
const COMPACT_WIDTH: i32 = 1100;
const NARROW_WIDTH: i32 = 800;
const INSPECTOR_WIDTH: i32 = 320;

/// Loads the single application stylesheet. Call once after GTK is initialised.
pub fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    // Focusing a selectable label (titles, ids) must not select its whole text.
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_label_select_on_focus(false);
    }
}

pub struct App {
    pub window: gtk::ApplicationWindow,
    store: Rc<Store>,
    controller: Rc<Controller>,
    header: Rc<header::Header>,
    rail: Rc<rail::Rail>,
    workbench: Rc<workbench::Workbench>,
    activity: Rc<activity::Activity>,
    inspector: Rc<inspector::Inspector>,
    body: gtk::Stack,
    centre: gtk::Stack,
    paned: gtk::Paned,
    right: gtk::Paned,
    probe: gtk::DrawingArea,
    inspector_width: Cell<i32>,
    empty: gtk::Box,
    notice: gtk::Box,
    preview_band: gtk::Label,
    notices_seen: Cell<usize>,
    draft_timer: RefCell<Option<glib::SourceId>>,
    scheduling: Cell<bool>,
    width_class: Cell<u8>,
    /// One window-level shortcut per action (Escape excepted: dialogs own it), retargeted in
    /// place when the keymap changes so there is exactly one dispatch path.
    shortcuts: RefCell<Vec<(AppAction, gtk::Shortcut)>>,
    keys_path: Option<PathBuf>,
}

impl App {
    pub fn new(
        application: &gtk::Application,
        store: Rc<Store>,
        sink: Rc<dyn IntentSink>,
    ) -> Rc<Self> {
        let controller = Controller::new(store.clone(), sink);
        let window = gtk::ApplicationWindow::builder()
            .application(application)
            .title("LADE")
            .default_width(1440)
            .default_height(900)
            .build();
        window.add_css_class("lade-window");

        let header = header::Header::new(&controller);
        let rail = rail::Rail::new(&controller);
        let workbench = workbench::Workbench::new(&controller);
        let activity = activity::Activity::new(&controller);
        let inspector = inspector::Inspector::new(&controller);

        let root = widgets::vbox(0);
        root.add_css_class("lade-root");
        root.append(&header.root);
        // Persistent statement in preview mode so fixture data is never mistaken for service state.
        let preview_band = widgets::label("", &["preview-band"]);
        preview_band.set_visible(false);
        root.append(&preview_band);
        let notice = widgets::hbox(12);
        notice.add_css_class("notice-bar");
        notice.set_visible(false);
        root.append(&notice);

        let centre = gtk::Stack::new();
        centre.add_named(&workbench.root, Some("workbench"));
        centre.add_named(&activity.root, Some("activity"));
        centre.set_hexpand(true);
        centre.add_css_class("centre-stack");
        widgets::set_accessible_label(&centre, "Main work area");
        // Inspector: adjustable 280–360 px; it may shrink transiently so the window can narrow
        // into the compact composition, where it becomes an on-demand panel.
        inspector.root.set_hexpand(false);
        widgets::set_accessible_label(&inspector.root, "Inspector");
        let right = gtk::Paned::new(gtk::Orientation::Horizontal);
        right.set_start_child(Some(&centre));
        right.set_end_child(Some(&inspector.root));
        right.set_resize_start_child(true);
        right.set_resize_end_child(false);
        right.set_shrink_start_child(false);
        right.set_shrink_end_child(true);
        right.set_wide_handle(false);
        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        paned.set_start_child(Some(&rail.root));
        paned.set_end_child(Some(&right));
        paned.set_resize_start_child(false);
        paned.set_shrink_start_child(true);
        paned.set_shrink_end_child(false);
        paned.set_position(248);
        paned.set_wide_handle(false);
        paned.connect_position_notify(|p| {
            let clamped = p.position().clamp(224, 280);
            if clamped != p.position() {
                p.set_position(clamped);
            }
        });

        // A zero-height probe reports the real allocated width for the compact compositions.
        let probe = gtk::DrawingArea::new();
        probe.set_hexpand(true);
        probe.set_content_height(0);
        probe.set_can_target(false);
        probe.set_focusable(false);
        root.prepend(&probe);

        let empty = widgets::vbox(12);
        empty.add_css_class("empty-state");
        let body = gtk::Stack::new();
        body.set_vexpand(true);
        body.add_named(&empty, Some("empty"));
        body.add_named(&paned, Some("main"));
        root.append(&body);
        window.set_child(Some(&root));

        let app = Rc::new(Self {
            window,
            store: store.clone(),
            controller,
            header,
            rail,
            workbench,
            activity,
            inspector,
            body,
            centre,
            paned,
            right,
            probe,
            inspector_width: Cell::new(INSPECTOR_WIDTH),
            empty,
            notice,
            preview_band,
            notices_seen: Cell::new(0),
            draft_timer: RefCell::new(None),
            scheduling: Cell::new(false),
            width_class: Cell::new(u8::MAX),
            shortcuts: RefCell::new(Vec::new()),
            keys_path: keys::config_path(|name| std::env::var(name).ok()),
        });
        app.connect();
        let weak = Rc::downgrade(&app);
        store.subscribe(move |changes| {
            if let Some(app) = weak.upgrade() {
                app.refresh(changes);
            }
        });
        app.load_keymap();
        app.render_body();
        app.render_centre();
        app
    }

    /// Requests the viewer content for a restored selection (e.g. the preview fixture).
    pub fn load_initial(&self) {
        self.controller.reload_viewer();
    }

    pub fn present(&self) {
        self.window.present();
    }

    /// Sender for delivering store updates from another thread (WP-G transport).
    pub fn update_sender(&self) -> bridge::UpdateSender {
        bridge::connect(&self.store, 1024)
    }

    fn connect(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let new_objective: Rc<dyn Fn(&gtk::Button)> = Rc::new(move |b| {
            if let Some(app) = weak.upgrade() {
                dialogs::new_objective(b, &app.controller);
            }
        });
        self.rail.set_on_new(new_objective);
        let weak = Rc::downgrade(self);
        self.workbench.viewer.set_on_add_feedback(Rc::new(move || {
            if let Some(app) = weak.upgrade() {
                app.add_feedback();
            }
        }));
        let weak = Rc::downgrade(self);
        self.inspector.set_on_draft_edit(Rc::new(move || {
            if let Some(app) = weak.upgrade() {
                app.schedule_drafts();
            }
        }));
        let weak = Rc::downgrade(self);
        self.probe.connect_resize(move |_, width, _| {
            if let Some(app) = weak.upgrade() {
                app.apply_width(width);
            }
        });
        // Keep the inspector width (not the centre width) stable when the window resizes, and
        // keep a dragged width within 280–360 px.
        let weak = Rc::downgrade(self);
        self.right.connect_max_position_notify(move |p| {
            if let Some(app) = weak.upgrade() {
                p.set_position(p.max_position() - app.inspector_width.get());
            }
        });
        let weak = Rc::downgrade(self);
        self.right.connect_position_notify(move |p| {
            if let Some(app) = weak.upgrade()
                && app.inspector.root.is_visible()
                && p.max_position() > 0
            {
                let width = p.max_position() - p.position();
                let clamped = width.clamp(280, 360);
                if app.width_class.get() == 0 {
                    app.inspector_width.set(clamped);
                }
                if clamped != width {
                    p.set_position(p.max_position() - clamped);
                }
            }
        });
        for toggle in [&self.header.details_toggle, &self.header.rail_toggle] {
            let weak = Rc::downgrade(self);
            toggle.connect_toggled(move |_| {
                if let Some(app) = weak.upgrade() {
                    let width = app.window.default_width();
                    app.width_class.set(u8::MAX);
                    app.apply_width(width);
                }
            });
        }
        let weak = Rc::downgrade(self);
        self.header.shortcuts_button.connect_clicked(move |b| {
            if let Some(app) = weak.upgrade() {
                dialogs::shortcuts(b, &app.controller, app.keys_path.clone());
            }
        });
        self.install_shortcuts();
    }

    /// Reads the client keybinding preference; an invalid file is ignored with a visible notice.
    fn load_keymap(&self) {
        let Some(path) = &self.keys_path else { return };
        let (keymap, problem) = keys::load(path);
        self.controller.set_keymap(keymap);
        if let Some(message) = problem {
            self.store.apply(StoreUpdate::Notice(ServiceNotice {
                level: NoticeLevel::Warning,
                code: "keybindings_ignored".to_owned(),
                message,
            }));
        }
    }

    /// Installs one capture-phase shortcut per window action on the main window only. Dialogs
    /// are separate windows, so a remapped window shortcut never captures their field input;
    /// Escape is installed by each dialog itself.
    fn install_shortcuts(self: &Rc<Self>) {
        let controller = gtk::ShortcutController::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let keymap = self.store.state().ui.keymap.clone();
        let mut shortcuts = Vec::new();
        for binding in keys::DEFAULT_APP_KEYBINDINGS {
            if binding.action == AppAction::CloseDialog {
                continue;
            }
            let weak = Rc::downgrade(self);
            let action = binding.action;
            let callback = gtk::CallbackAction::new(move |_, _| match weak.upgrade() {
                Some(app) => app.run_action(action),
                None => glib::Propagation::Proceed,
            });
            let shortcut = gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(&keymap.gtk(action)),
                Some(callback),
            );
            controller.add_shortcut(shortcut.clone());
            shortcuts.push((action, shortcut));
        }
        self.window.add_controller(controller);
        *self.shortcuts.borrow_mut() = shortcuts;
        self.header.set_shortcut_hints(&keymap);
    }

    /// Retargets every window shortcut to the current keymap at once.
    fn apply_keymap(&self) {
        let keymap = self.store.state().ui.keymap.clone();
        for (action, shortcut) in self.shortcuts.borrow().iter() {
            shortcut.set_trigger(gtk::ShortcutTrigger::parse_string(&keymap.gtk(*action)));
        }
        self.header.set_shortcut_hints(&keymap);
    }

    fn run_action(self: &Rc<Self>, action: AppAction) -> glib::Propagation {
        match action {
            AppAction::NextRegion => self.cycle_regions(true),
            AppAction::PreviousRegion => self.cycle_regions(false),
            AppAction::ShowWorkbench => self.controller.set_view(MainView::Workbench),
            AppAction::ShowActivity => self.controller.set_view(MainView::Activity),
            AppAction::FocusSearch => self.header.focus_search(),
            AppAction::AddFeedback => self.add_feedback(),
            AppAction::NewObjective => {
                if self.store.state().active_workspace.is_some() {
                    dialogs::new_objective(&self.rail.new_button, &self.controller);
                }
            }
            AppAction::OpenWorkspace => {
                dialogs::open_workspace(self.window.upcast_ref(), &self.controller);
            }
            AppAction::ToggleViewerMode => self.controller.toggle_viewer_mode(),
            AppAction::ShowShortcuts => dialogs::shortcuts(
                &self.header.shortcuts_button,
                &self.controller,
                self.keys_path.clone(),
            ),
            AppAction::SendMessage => {
                if !self.activity.composer_has_focus() {
                    return glib::Propagation::Proceed;
                }
                self.activity.send();
            }
            AppAction::CloseDialog => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    }

    fn regions(&self) -> Vec<gtk::Widget> {
        vec![
            self.header.root.clone().upcast(),
            self.rail.root.clone().upcast(),
            self.centre.clone().upcast(),
            self.inspector.root.clone().upcast(),
        ]
    }

    /// F6 / Shift+F6: header → Objectives → centre → inspector, skipping hidden regions.
    fn cycle_regions(&self, forward: bool) {
        let regions: Vec<gtk::Widget> = self
            .regions()
            .into_iter()
            .filter(|r| r.is_mapped())
            .collect();
        if regions.is_empty() {
            return;
        }
        let focus = gtk::prelude::GtkWindowExt::focus(&self.window);
        let current = focus.and_then(|f| regions.iter().position(|r| f.is_ancestor(r) || &f == r));
        let n = regions.len();
        let next = match (current, forward) {
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
            (None, true) => 0,
            (None, false) => n - 1,
        };
        let target = &regions[next];
        if !target.child_focus(gtk::DirectionType::TabForward) {
            target.grab_focus();
        }
    }

    fn add_feedback(&self) {
        let id = DraftId::new(glib::uuid_string_random().as_str());
        match self.controller.add_feedback(id) {
            Ok(id) => self.inspector.focus_draft(&id),
            Err(message) => widgets::announce(
                &self.window,
                &message,
                gtk::AccessibleAnnouncementPriority::Medium,
            ),
        }
    }

    /// Runs the draft autosave scheduler and arms a timer for the next due save.
    fn schedule_drafts(self: &Rc<Self>) {
        if self.scheduling.replace(true) {
            return;
        }
        let now = widgets::monotonic_ms();
        let next = self.controller.poll_drafts(now);
        if let Some(old) = self.draft_timer.borrow_mut().take() {
            old.remove();
        }
        if let Some(deadline) = next {
            let weak = Rc::downgrade(self);
            let delay = Duration::from_millis(deadline.saturating_sub(now).max(1));
            let id = glib::timeout_add_local_once(delay, move || {
                if let Some(app) = weak.upgrade() {
                    // The source fired and is gone; forget its id without removing it.
                    app.draft_timer.borrow_mut().take();
                    app.schedule_drafts();
                }
            });
            *self.draft_timer.borrow_mut() = Some(id);
        }
        self.scheduling.set(false);
    }

    fn refresh(self: &Rc<Self>, changes: &[Change]) {
        self.header.refresh(changes);
        self.rail.refresh(changes);
        self.workbench.refresh(changes);
        self.activity.refresh(changes);
        self.inspector.refresh(changes);
        if changes
            .iter()
            .any(|c| matches!(c, Change::Connection | Change::Workspaces))
        {
            self.render_body();
        }
        if changes.contains(&Change::View) {
            self.render_centre();
        }
        if changes.contains(&Change::Notices) {
            self.render_notice();
        }
        if changes.contains(&Change::Keymap) {
            self.apply_keymap();
        }
        if changes.iter().any(|c| {
            matches!(
                c,
                Change::DraftStatus(_) | Change::Drafts(_) | Change::Connection
            )
        }) {
            self.schedule_drafts();
        }
    }

    fn render_centre(&self) {
        let view = self.store.state().ui.view;
        self.centre.set_visible_child_name(match view {
            MainView::Workbench => "workbench",
            MainView::Activity => "activity",
        });
    }

    fn render_body(self: &Rc<Self>) {
        let state = self.store.state();
        let (_, full) = header::connection_text(&state.connection, state.is_preview());
        self.preview_band.set_text(&format!(
            "{full}. Every Objective, file, check and draft shown here is fixture data; actions are not sent anywhere."
        ));
        self.preview_band.set_visible(state.is_preview());
        let has_workspace = state.active_workspace().is_some();
        let connection = state.connection.clone();
        let preview = state.is_preview();
        drop(state);
        widgets::clear(&self.empty);
        if has_workspace || preview {
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
        self.body.set_visible_child_name("empty");
    }

    fn render_notice(self: &Rc<Self>) {
        widgets::clear(&self.notice);
        let state = self.store.state();
        let total = state.notices.len();
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

    /// Compact compositions: narrower rail and an on-demand inspector below ~1100 px; on-demand
    /// Objectives list below ~800 px. Nothing is removed, only placed behind a visible toggle.
    fn apply_width(&self, width: i32) {
        let class = if width < NARROW_WIDTH {
            2
        } else if width < COMPACT_WIDTH {
            1
        } else {
            0
        };
        let toggles_changed = self.width_class.get() == u8::MAX;
        if self.width_class.replace(class) == class && !toggles_changed {
            return;
        }
        let inspector = if class == 0 { INSPECTOR_WIDTH } else { 300 };
        self.inspector_width.set(inspector);
        let details = self.header.details_toggle.is_active();
        let show_rail = self.header.rail_toggle.is_active();
        self.header.set_compact(class);
        self.header.details_toggle.set_visible(class >= 1);
        self.header.rail_toggle.set_visible(class >= 2);
        self.inspector.root.set_visible(class == 0 || details);
        let right = self.right.clone();
        glib::idle_add_local_once(move || {
            right.set_position(right.max_position() - inspector);
        });
        // Compact: the inspector opens in place of the Objectives list, keeping the centre usable.
        self.rail.root.set_visible(match class {
            0 => true,
            1 => !details,
            _ => show_rail && !details,
        });
        self.paned.set_position(if class == 0 { 248 } else { 224 });
        if class >= 1 {
            self.window.add_css_class("compact");
        } else {
            self.window.remove_css_class("compact");
        }
    }
}
