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
pub mod notify;
mod rail;
mod session;
mod terminal;
pub mod trace;
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

use crate::presentation::attention::{self, AttentionWatcher, Delivery, Notifier};
use crate::presentation::controller::Controller;
use crate::presentation::drafts::CLOSE_WAIT_MS;
use crate::presentation::intents::{IntentSink, StopReason};
use crate::presentation::keys::{self, AppAction};
use crate::presentation::layout::{self, LayoutClass, Region, WideSizes};
use crate::presentation::model::{Attention, ConnectionState, DraftId, NoticeLevel, ServiceNotice};
use crate::presentation::store::{Change, MainView, Store, StoreUpdate, ViewerContent};

const CSS: &str = include_str!("../assets/style.css");

/// What happens once no draft text would be lost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseAfter {
    /// The window closes; the service keeps running.
    Window,
    /// Explicit Exit: the service is asked to stop.
    Exit,
}

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
    terminal: Rc<terminal::TerminalPane>,
    /// Where focus was before the terminal took it (restored by Leave terminal).
    before_terminal: RefCell<Option<glib::WeakRef<gtk::Widget>>>,
    watcher: RefCell<AttentionWatcher>,
    delivery: RefCell<Delivery>,
    /// Desktop notifications (live windows only; never for preview data).
    notifier: RefCell<Option<Rc<dyn Notifier>>>,
    body: gtk::Stack,
    centre: gtk::Stack,
    /// The work area's region viewport: scrolls vertically when the window is short and bounds
    /// the work area's minimum width so the window can always narrow into compact.
    centre_region: gtk::ScrolledWindow,
    region_bar: gtk::Box,
    region_switches: Vec<(Region, gtk::ToggleButton)>,
    paned: gtk::Paned,
    right: gtk::Paned,
    probe: gtk::DrawingArea,
    sizes: Cell<WideSizes>,
    empty: gtk::Box,
    notice: gtk::Box,
    preview_band: gtk::Label,
    notices_seen: Cell<usize>,
    draft_timer: RefCell<Option<glib::SourceId>>,
    scheduling: Cell<bool>,
    updating_layout: Cell<bool>,
    /// One window-level shortcut per action (Escape excepted: dialogs own it), retargeted in
    /// place when the keymap changes so there is exactly one dispatch path.
    shortcuts: RefCell<Vec<(AppAction, gtk::Shortcut)>>,
    keys_path: Option<PathBuf>,
    /// Safe close: the action waiting for flushed drafts to be acknowledged.
    close_pending: Cell<Option<CloseAfter>>,
    /// The developer chose to close (nothing unsaved, or unsaved text explicitly discarded).
    close_confirmed: Cell<bool>,
    close_timer: RefCell<Option<glib::SourceId>>,
    loss_dialog: RefCell<Option<gtk::Window>>,
    trace: Option<Rc<trace::Trace>>,
}

impl App {
    pub fn new(
        application: &gtk::Application,
        store: Rc<Store>,
        sink: Rc<dyn IntentSink>,
    ) -> Rc<Self> {
        let trace = trace::Trace::from_env();
        let sink: Rc<dyn IntentSink> = match &trace {
            Some(trace) => Rc::new(trace::TracingSink {
                inner: sink,
                trace: trace.clone(),
            }),
            None => sink,
        };
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
        let terminal = terminal::TerminalPane::new(&controller);

        let root = widgets::vbox(0);
        root.add_css_class("lade-root");
        root.append(&header.viewport);
        // Persistent statement in preview mode so fixture data is never mistaken for service state.
        let preview_band = widgets::label("", &["preview-band"]);
        preview_band.set_visible(false);
        root.append(&preview_band);
        let notice = widgets::hbox(12);
        notice.add_css_class("notice-bar");
        notice.set_visible(false);
        root.append(&notice);

        // Compact composition: explicit region switches instead of side-by-side rails.
        let region_bar = widgets::hbox(0);
        region_bar.add_css_class("region-bar");
        region_bar.set_visible(false);
        let switches = widgets::hbox(0);
        switches.add_css_class("segmented");
        widgets::set_accessible_label(&switches, "Region");
        let mut region_switches = Vec::new();
        for region in [Region::Objectives, Region::Main, Region::Inspector] {
            let toggle = gtk::ToggleButton::with_label(region.switch_label());
            toggle.set_tooltip_text(Some(region.switch_tooltip()));
            if let Some((_, first)) = region_switches.first() {
                toggle.set_group(Some(first));
            }
            switches.append(&toggle);
            region_switches.push((region, toggle));
        }
        region_bar.append(&switches);
        root.append(&region_bar);

        let centre = gtk::Stack::new();
        centre.add_named(&workbench.root, Some("workbench"));
        centre.add_named(&activity.root, Some("activity"));
        centre.add_named(&terminal.root, Some("terminal"));
        centre.set_hexpand(true);
        centre.set_vexpand(true);
        centre.add_css_class("centre-stack");
        let centre_region = widgets::region_scroll(&centre);
        centre_region.set_hscrollbar_policy(gtk::PolicyType::External);
        centre_region.set_hexpand(true);
        widgets::set_accessible_label(&centre_region, "Main work area");
        // Inspector: adjustable 280–360 px; it may shrink transiently so the window can narrow
        // into the compact composition, where it becomes a region of its own.
        inspector.root.set_hexpand(false);
        widgets::set_accessible_label(&inspector.root, "Inspector");
        let right = gtk::Paned::new(gtk::Orientation::Horizontal);
        right.set_start_child(Some(&centre_region));
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
            terminal,
            before_terminal: RefCell::new(None),
            watcher: RefCell::new(AttentionWatcher::default()),
            delivery: RefCell::new(Delivery::default()),
            notifier: RefCell::new(None),
            body,
            centre,
            centre_region,
            region_bar,
            region_switches,
            paned,
            right,
            probe,
            sizes: Cell::new(layout::wide_sizes(1440)),
            empty,
            notice,
            preview_band,
            notices_seen: Cell::new(0),
            draft_timer: RefCell::new(None),
            scheduling: Cell::new(false),
            updating_layout: Cell::new(false),
            shortcuts: RefCell::new(Vec::new()),
            keys_path: keys::config_path(|name| std::env::var(name).ok()),
            close_pending: Cell::new(None),
            close_confirmed: Cell::new(false),
            close_timer: RefCell::new(None),
            loss_dialog: RefCell::new(None),
            trace,
        });
        if let Some(trace) = &app.trace {
            trace.watch_paints(&app.window);
        }
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
        app.render_layout();
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
                if let Some(trace) = &app.trace {
                    trace.event("draft-edit", "");
                }
                app.schedule_drafts();
            }
        }));
        // Safe close: unsaved draft text is flushed and acknowledged, or explicitly discarded.
        let weak = Rc::downgrade(self);
        self.window
            .connect_close_request(move |_| match weak.upgrade() {
                Some(app) if !app.close_confirmed.get() && !app.store.state().is_preview() => {
                    if app.guard_unsaved(CloseAfter::Window) {
                        glib::Propagation::Proceed
                    } else {
                        glib::Propagation::Stop
                    }
                }
                _ => glib::Propagation::Proceed,
            });
        let weak = Rc::downgrade(self);
        self.probe.connect_resize(move |_, width, _| {
            if let Some(app) = weak.upgrade() {
                app.apply_width(width);
            }
        });
        // Keep the inspector width (not the centre width) stable when the window resizes, and
        // keep a dragged width within 280–360 px. Only the wide composition has a side inspector.
        let weak = Rc::downgrade(self);
        self.right.connect_max_position_notify(move |p| {
            if let Some(app) = weak.upgrade()
                && app.store.state().ui.layout == LayoutClass::Wide
            {
                p.set_position(p.max_position() - app.sizes.get().inspector);
            }
        });
        let weak = Rc::downgrade(self);
        self.right.connect_position_notify(move |p| {
            if let Some(app) = weak.upgrade()
                && app.store.state().ui.layout == LayoutClass::Wide
                && p.max_position() > 0
            {
                let width = p.max_position() - p.position();
                let clamped = width.clamp(280, 360);
                if clamped != width {
                    p.set_position(p.max_position() - clamped);
                }
            }
        });
        for (region, toggle) in &self.region_switches {
            let weak = Rc::downgrade(self);
            let region = *region;
            toggle.connect_toggled(move |t| {
                if let Some(app) = weak.upgrade()
                    && t.is_active()
                    && !app.updating_layout.get()
                {
                    app.controller.show_region(region);
                }
            });
        }
        // Opening an Objective (Enter, double-click or click) in the compact Objectives region
        // moves to its work area.
        let weak = Rc::downgrade(self);
        self.rail.set_on_open(Rc::new(move || {
            if let Some(app) = weak.upgrade()
                && app.store.state().ui.layout == LayoutClass::Compact
            {
                app.controller.show_region(Region::Main);
                app.focus_region(Region::Main);
            }
        }));
        let weak = Rc::downgrade(self);
        self.header.shortcuts_button.connect_clicked(move |b| {
            if let Some(app) = weak.upgrade() {
                dialogs::shortcuts(b, &app.controller, app.keys_path.clone());
            }
        });
        let weak = Rc::downgrade(self);
        self.header.exit_button.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                app.confirm_exit();
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
        self.terminal.set_shortcut_hints(&keymap);
    }

    /// Retargets every window shortcut to the current keymap at once.
    fn apply_keymap(&self) {
        let keymap = self.store.state().ui.keymap.clone();
        for (action, shortcut) in self.shortcuts.borrow().iter() {
            shortcut.set_trigger(gtk::ShortcutTrigger::parse_string(&keymap.gtk(*action)));
        }
        self.header.set_shortcut_hints(&keymap);
        self.terminal.set_shortcut_hints(&keymap);
    }

    fn run_action(self: &Rc<Self>, action: AppAction) -> glib::Propagation {
        let in_terminal = self.terminal.has_focus();
        // A focused terminal receives every key except its escape routes, clipboard and Exit.
        if in_terminal && !keys::reaches_window_from_terminal(action) {
            return glib::Propagation::Proceed;
        }
        match action {
            AppAction::ShowTerminal => self.enter_terminal(),
            AppAction::LeaveTerminal | AppAction::TerminalCopy | AppAction::TerminalPaste
                if !in_terminal =>
            {
                return glib::Propagation::Proceed;
            }
            AppAction::LeaveTerminal => self.leave_terminal(),
            AppAction::TerminalCopy => self.terminal.copy(),
            AppAction::TerminalPaste => self.terminal.paste(),
            AppAction::Exit => self.confirm_exit(),
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

    /// Focus into the terminal: remembers where focus was, shows the terminal and focuses it.
    fn enter_terminal(&self) {
        if !self.terminal.has_focus() {
            let previous = gtk::prelude::GtkWindowExt::focus(&self.window);
            *self.before_terminal.borrow_mut() = previous.map(|w| w.downgrade());
        }
        self.controller.set_view(MainView::Terminal);
        let pane = self.terminal.clone();
        glib::idle_add_local_once(move || pane.focus());
    }

    /// Focus out of the terminal: back to where it was, else the Objectives region. The terminal
    /// keeps running.
    fn leave_terminal(&self) {
        let previous = self
            .before_terminal
            .borrow_mut()
            .take()
            .and_then(|w| w.upgrade())
            .filter(|w| w.is_mapped() && w.is_focusable() && !w.is_ancestor(&self.terminal.root));
        match previous {
            Some(widget) => {
                widget.grab_focus();
            }
            None => self.focus_region(Region::Objectives),
        }
        if self.terminal.has_focus() {
            self.focus_region(Region::Header);
        }
    }

    /// Explicit Exit: confirms, then stops the service (agents and terminals) when this window
    /// controls it; the window closes once the service confirms. Closing the window instead
    /// keeps authorized background work running.
    fn confirm_exit(self: &Rc<Self>) {
        let controls = {
            let state = self.store.state();
            state.connection.is_connected() && !state.is_preview()
        };
        let body = if controls {
            "Exit stops the LADE service: running agents are stopped and recorded as interrupted, terminals end, and nothing resumes on its own when LADE starts again. To keep agents working in the background, close the window instead."
        } else {
            "This window does not control the LADE service, so Exit only closes the window. The service and its agents are not affected."
        };
        let weak = Rc::downgrade(self);
        dialogs::confirm(
            &self.header.exit_button,
            "Exit LADE?",
            body,
            "Exit",
            controls,
            move || {
                if let Some(app) = weak.upgrade()
                    && app.guard_unsaved(CloseAfter::Exit)
                {
                    app.finish_close(CloseAfter::Exit);
                }
                Ok(())
            },
        );
    }

    /// The desktop session is ending: stop the service per policy (no lingering, no resume).
    /// Pending draft saves are sent first; the service handles them before the stop request.
    pub fn logout(&self) {
        self.controller.flush_drafts(widgets::monotonic_ms());
        self.controller.exit(StopReason::Logout);
    }

    /// Returns `true` when closing now loses no draft text. Otherwise flushes pending saves and
    /// waits (up to [`CLOSE_WAIT_MS`]) for the service's acknowledgements, then finishes `after`
    /// on its own; a failed, conflicting or disconnected draft asks the developer instead.
    fn guard_unsaved(self: &Rc<Self>, after: CloseAfter) -> bool {
        if self.controller.unsaved_drafts().is_empty() {
            return true;
        }
        if self.close_pending.replace(Some(after)).is_some() {
            return false; // Already waiting or asking.
        }
        if self.store.state().connection.is_connected() {
            self.controller.flush_drafts(widgets::monotonic_ms());
            self.schedule_drafts();
            widgets::announce(
                &self.window,
                "Saving feedback drafts before closing",
                gtk::AccessibleAnnouncementPriority::Medium,
            );
            let weak = Rc::downgrade(self);
            let id =
                glib::timeout_add_local_once(Duration::from_millis(CLOSE_WAIT_MS), move || {
                    if let Some(app) = weak.upgrade() {
                        app.close_timer.borrow_mut().take();
                        app.confirm_loss();
                    }
                });
            if let Some(old) = self.close_timer.borrow_mut().replace(id) {
                old.remove();
            }
            self.check_pending_close();
        } else {
            self.confirm_loss();
        }
        false
    }

    /// Re-evaluates a waiting close after draft or connection changes.
    fn check_pending_close(self: &Rc<Self>) {
        let Some(after) = self.close_pending.get() else {
            return;
        };
        let unsaved = self.controller.unsaved_drafts();
        if unsaved.is_empty() {
            self.finish_close(after);
        } else if unsaved.iter().any(|d| !d.pending) && self.loss_dialog.borrow().is_none() {
            // Failed, conflicting or disconnected: waiting cannot help.
            self.confirm_loss();
        }
    }

    /// Asks whether to keep the window open or discard the unsaved text.
    fn confirm_loss(self: &Rc<Self>) {
        let Some(after) = self.close_pending.get() else {
            return;
        };
        if let Some(timer) = self.close_timer.borrow_mut().take() {
            timer.remove();
        }
        if self.loss_dialog.borrow().is_some() {
            return;
        }
        let unsaved = self.controller.unsaved_drafts();
        if unsaved.is_empty() {
            self.finish_close(after);
            return;
        }
        let connected = self.store.state().connection.is_connected();
        let weak = Rc::downgrade(self);
        let dialog = dialogs::unsaved_drafts(
            &self.window,
            after == CloseAfter::Exit,
            &unsaved,
            connected,
            move || {
                if let Some(app) = weak.upgrade() {
                    app.finish_close(after);
                }
            },
        );
        // Keep window open (or Escape): the close request is withdrawn.
        let weak = Rc::downgrade(self);
        dialog.connect_close_request(move |_| {
            if let Some(app) = weak.upgrade() {
                app.loss_dialog.borrow_mut().take();
                app.close_pending.set(None);
            }
            glib::Propagation::Proceed
        });
        *self.loss_dialog.borrow_mut() = Some(dialog);
    }

    /// Closes (or exits) now: nothing is unsaved, or the developer discarded it.
    fn finish_close(&self, after: CloseAfter) {
        self.close_pending.set(None);
        if let Some(timer) = self.close_timer.borrow_mut().take() {
            timer.remove();
        }
        if let Some(dialog) = self.loss_dialog.borrow_mut().take() {
            dialog.close();
        }
        match after {
            CloseAfter::Window => {
                self.close_confirmed.set(true);
                self.window.close();
            }
            CloseAfter::Exit => {
                if !self.controller.exit(StopReason::Exit) {
                    self.close_confirmed.set(true);
                    self.quit();
                }
            }
        }
    }

    /// Records the semantic state behind `changes` in the opt-in timing trace.
    fn trace_changes(&self, changes: &[Change]) {
        let Some(trace) = &self.trace else { return };
        let state = self.store.state();
        for change in changes {
            match change {
                Change::DraftStatus(id) => {
                    if let Some(editor) =
                        state.drafts.values().flatten().find(|d| &d.draft_id == id)
                    {
                        let status = editor.status(state.connection.is_connected());
                        trace.changed("draft", &format!("{} {}", id.short(), status.label()));
                    }
                }
                Change::Viewer => {
                    let detail = match &state.viewer.content {
                        ViewerContent::Empty => "empty".to_owned(),
                        ViewerContent::Loading => "loading".to_owned(),
                        ViewerContent::Failed(_) => "failed".to_owned(),
                        ViewerContent::File(f) => {
                            format!("file {} of {} bytes", f.loaded_bytes, f.total_bytes)
                        }
                        ViewerContent::Diff(d) => format!("diff {} hunks", d.hunks.len()),
                    };
                    trace.changed("viewer", &detail);
                }
                Change::Activity(id, _) => {
                    if let Some(log) = state.activity.get(id) {
                        let last = log
                            .items
                            .last()
                            .map_or_else(|| "none".to_owned(), |i| format!("{:?}", i.role));
                        trace.changed("activity", &format!("{} {last}", log.items.len()));
                    }
                }
                Change::Detail(id) => {
                    if let Some(d) = state.detail(id) {
                        trace.changed(
                            "detail",
                            &format!(
                                "{:?} {:?} candidates={} approved={}",
                                d.summary.lifecycle,
                                d.summary.execution.state,
                                d.candidates.len(),
                                d.approval.is_some()
                            ),
                        );
                    }
                }
                Change::Connection => {
                    trace.changed("connection", &format!("{:?}", state.connection));
                }
                Change::Objectives => {
                    trace.changed("objectives", &state.objectives.len().to_string());
                }
                _ => {}
            }
        }
    }

    fn quit(&self) {
        match self.window.application() {
            Some(application) => application.quit(),
            None => self.window.close(),
        }
    }

    /// Enables desktop notifications for this (live) window.
    pub fn set_notifier(&self, notifier: Rc<dyn Notifier>) {
        *self.notifier.borrow_mut() = Some(notifier);
    }

    /// A notification was activated: navigate to its Objective (never approve or send).
    pub fn open_from_notification(&self, target: &str) {
        let found = {
            let state = self.store.state();
            attention::activation_target(target, &state.objectives).map(|id| {
                let review = state
                    .objective(&id)
                    .is_some_and(|o| o.attention == Attention::ReadyForReview);
                (id, review)
            })
        };
        if let Some((id, review)) = found {
            self.controller.navigate_to(&id, review);
            self.focus_region(Region::Main);
        }
        self.window.present();
    }

    /// Raises notifications for Objectives that newly need the developer, outside the window.
    fn observe_attention(self: &Rc<Self>, changes: &[Change]) {
        if changes.contains(&Change::Connection) && !self.store.state().connection.is_connected() {
            self.watcher.borrow_mut().reset();
            return;
        }
        if !changes
            .iter()
            .any(|c| matches!(c, Change::Objectives | Change::Detail(_)))
        {
            return;
        }
        let Some(notifier) = self.notifier.borrow().clone() else {
            return;
        };
        let requests = {
            let state = self.store.state();
            if state.is_preview() {
                return;
            }
            self.watcher.borrow_mut().observe(&state.objectives)
        };
        if requests.is_empty() {
            return;
        }
        let notice = self.delivery.borrow_mut().deliver(
            &requests,
            self.window.is_active(),
            notifier.as_ref(),
        );
        if let Some(reason) = notice {
            let store = self.store.clone();
            glib::idle_add_local_once(move || {
                store.apply(StoreUpdate::NotificationsUnavailable { reason });
            });
        }
    }

    fn region_widget(&self, region: Region) -> gtk::Widget {
        match region {
            Region::Header => self.header.root.clone().upcast(),
            Region::Objectives => self.rail.root.clone().upcast(),
            Region::Main => self.centre_region.clone().upcast(),
            Region::Inspector => self.inspector.root.clone().upcast(),
        }
    }

    fn focused_region(&self) -> Option<Region> {
        let focus = gtk::prelude::GtkWindowExt::focus(&self.window)?;
        layout::REGION_ORDER.into_iter().find(|region| {
            let widget = self.region_widget(*region);
            focus == widget || focus.is_ancestor(&widget)
        })
    }

    fn focus_region(&self, region: Region) {
        let target = self.region_widget(region);
        if !target.child_focus(gtk::DirectionType::TabForward) {
            target.grab_focus();
        }
    }

    /// F6 / Shift+F6: header → Objectives → work area → inspector. The wide composition skips
    /// hidden regions; the compact one switches to the region first, so every region stays
    /// reachable and the escape route always works.
    fn cycle_regions(&self, forward: bool) {
        let compact = self.store.state().ui.layout == LayoutClass::Compact;
        let mut target = layout::next_region(self.focused_region(), forward);
        if compact {
            if target != Region::Header {
                self.controller.show_region(target);
            }
        } else {
            for _ in 0..layout::REGION_ORDER.len() {
                if self.region_widget(target).is_mapped() {
                    break;
                }
                target = layout::next_region(Some(target), forward);
            }
        }
        self.focus_region(target);
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
        self.trace_changes(changes);
        self.header.refresh(changes);
        self.rail.refresh(changes);
        self.workbench.refresh(changes);
        self.activity.refresh(changes);
        self.inspector.refresh(changes);
        self.terminal.refresh(changes);
        self.observe_attention(changes);
        if changes.contains(&Change::Lifetime) && self.store.state().service_stopped {
            self.quit();
            return;
        }
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
        if changes.contains(&Change::Layout) {
            self.render_layout();
        }
        if changes.iter().any(|c| {
            matches!(
                c,
                Change::DraftStatus(_) | Change::Drafts(_) | Change::Connection
            )
        }) {
            self.schedule_drafts();
            self.check_pending_close();
        }
    }

    fn render_centre(&self) {
        let view = self.store.state().ui.view;
        self.centre.set_visible_child_name(match view {
            MainView::Workbench => "workbench",
            MainView::Activity => "activity",
            MainView::Terminal => "terminal",
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

    /// Records the allocated width; the store decides the layout class (`presentation::layout`).
    /// Within the wide composition, side widths tighten below 1280 px.
    fn apply_width(&self, width: i32) {
        let sizes = layout::wide_sizes(width);
        if self.sizes.replace(sizes) != sizes {
            self.workbench.set_file_list_width(sizes.file_list);
            if self.store.state().ui.layout == LayoutClass::Wide {
                self.place_wide_panes();
            }
        }
        self.controller.set_layout_width(width);
    }

    fn place_wide_panes(&self) {
        let sizes = self.sizes.get();
        self.paned.set_position(sizes.rail);
        let right = self.right.clone();
        let inspector = sizes.inspector;
        // The max position is known only after the next allocation.
        glib::idle_add_local_once(move || {
            right.set_position(right.max_position() - inspector);
        });
    }

    /// Applies the composition decided in `presentation::layout`: visible regions, the region
    /// switches, the header and the Objective details toggle. Nothing is removed in compact; one
    /// region is shown at a time and the others stay one switch (or F6) away.
    fn render_layout(&self) {
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
        self.inspector.root.set_visible(c.inspector);
        // A paned with both children hidden still takes its share: hide it so the shown region
        // gets the whole width.
        self.right.set_visible(c.centre || c.inspector);
        self.region_bar.set_visible(c.region_switches);
        self.header.set_compact(class == LayoutClass::Compact);
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
        // Focus in a region that was just hidden would be lost; keep it in the shown region.
        if let Some(previous) = focus_region
            && previous != Region::Header
            && !self.region_widget(previous).is_visible()
        {
            self.focus_region(shown);
        }
    }
}
