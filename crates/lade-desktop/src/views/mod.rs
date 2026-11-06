//! The GTK window: [`App`] builds the regions, routes store changes to them and owns the
//! window-wide state; each region and dialog lives in its own module.

mod activity;
mod dialogs;
mod header;
mod inspector;
pub mod platform;
mod rail;
mod session;
mod terminal;
mod viewer;
mod widgets;
mod window;
mod workbench;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::presentation::controller::Controller;
use crate::presentation::controller::intents::IntentSink;
use crate::presentation::keys::{self, AppAction};
use crate::presentation::layout::{self, LayoutClass, Region, WideSizes};
use crate::presentation::state::attention::{self, AttentionWatcher, Delivery, Notifier};
use crate::presentation::state::model::{Attention, DraftId, NoticeScope};
use crate::presentation::state::store::{Change, Store, StoreUpdate, ViewerContent};

use platform::trace;
use window::CloseAfter;

const CSS: &str = include_str!("../assets/style.css");

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
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_theme_name(Some("Adwaita"));
        settings.set_gtk_icon_theme_name(Some("Adwaita"));
        settings.set_gtk_application_prefer_dark_theme(true);
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
    before_terminal: RefCell<Option<glib::WeakRef<gtk::Widget>>>,
    watcher: RefCell<AttentionWatcher>,
    delivery: RefCell<Delivery>,
    notifier: RefCell<Option<Rc<dyn Notifier>>>,
    body: gtk::Stack,
    centre: gtk::Stack,
    centre_region: gtk::ScrolledWindow,
    region_bar: gtk::Box,
    region_switches: Vec<(Region, gtk::ToggleButton)>,
    paned: gtk::Paned,
    right: gtk::Paned,
    probe: gtk::DrawingArea,
    sizes: Cell<WideSizes>,
    empty: gtk::Box,
    notice: gtk::Box,
    notices_seen: Cell<usize>,
    draft_timer: RefCell<Option<glib::SourceId>>,
    scheduling: Cell<bool>,
    updating_layout: Cell<bool>,
    shortcuts: RefCell<Vec<(AppAction, gtk::Shortcut)>>,
    keys_path: Option<PathBuf>,
    close_pending: Cell<Option<CloseAfter>>,
    close_confirmed: Cell<bool>,
    close_timer: RefCell<Option<glib::SourceId>>,
    loss_dialog: RefCell<Option<gtk::Window>>,
    trace: Option<Rc<trace::Trace>>,
}

/// Process settings the window needs, read from the environment once by `main`.
#[derive(Clone, Debug, Default)]
pub struct Environment {
    /// Where the user's keybinding overrides live; `None` keeps the defaults only.
    pub keys_path: Option<PathBuf>,
    /// Where to append the interaction trace, when tracing is on.
    pub trace_path: Option<PathBuf>,
}

impl Environment {
    pub fn from_process() -> Self {
        let env = |name: &str| std::env::var(name).ok();
        Self {
            keys_path: keys::config_path(env),
            trace_path: trace::path_from_env(env),
        }
    }
}

impl App {
    pub fn new(
        application: &gtk::Application,
        store: Rc<Store>,
        sink: Rc<dyn IntentSink>,
        environment: Environment,
    ) -> Rc<Self> {
        let trace = trace::Trace::open(environment.trace_path.as_deref());
        let sink: Rc<dyn IntentSink> = match &trace {
            Some(trace) => Rc::new(trace::TracingSink {
                inner: sink,
                trace: trace.clone(),
            }),
            None => sink,
        };
        let controller = Controller::new(Rc::clone(&store), sink);
        let window = main_window(application);

        let header = header::Header::new(&controller);
        let rail = rail::Rail::new(&controller);
        let workbench = workbench::Workbench::new(&controller);
        let activity = activity::Activity::new(&controller);
        let inspector = inspector::Inspector::new(&controller);
        let terminal = terminal::TerminalPane::new(&controller);

        let root = widgets::vbox(0);
        root.add_css_class("lade-root");
        root.append(&header.viewport);
        let notice = widgets::hbox(widgets::space::GROUP);
        notice.add_css_class("notice-bar");
        notice.set_visible(false);
        root.append(&notice);

        let (region_bar, region_switches) = region_bar();
        root.append(&region_bar);

        let centre = centre_stack(&[
            (workbench.root.upcast_ref(), "workbench"),
            (activity.root.upcast_ref(), "activity"),
            (terminal.root.upcast_ref(), "terminal"),
        ]);
        let centre_region = centre_region(&centre);
        let (paned, right) = panes(
            rail.root.upcast_ref(),
            &centre_region,
            inspector.root.upcast_ref(),
        );

        let probe = width_probe();
        root.prepend(&probe);

        let empty = widgets::vbox(widgets::space::GROUP);
        empty.add_css_class("empty-state");
        let body = gtk::Stack::new();
        body.set_vexpand(true);
        body.add_named(&empty, Some("empty"));
        body.add_named(&paned, Some("main"));
        root.append(&body);
        window.set_child(Some(&root));

        let app = Rc::new(Self {
            window,
            store,
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
            notices_seen: Cell::new(0),
            draft_timer: RefCell::new(None),
            scheduling: Cell::new(false),
            updating_layout: Cell::new(false),
            shortcuts: RefCell::new(Vec::new()),
            keys_path: environment.keys_path,
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
        app.store.subscribe(move |changes| {
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

    pub fn load_initial(&self) {
        self.controller.reload_viewer();
    }

    pub fn present(&self) {
        self.window.present();
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
        let weak = Rc::downgrade(self);
        self.window
            .connect_close_request(move |_| match weak.upgrade() {
                Some(app) if !app.close_confirmed.get() => {
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
        self.header.set_on_shortcuts(Rc::new(move |opener| {
            if let Some(app) = weak.upgrade() {
                dialogs::shortcuts(opener, &app.controller, app.keys_path.clone());
            }
        }));
        let weak = Rc::downgrade(self);
        self.header.set_on_exit(Rc::new(move |opener| {
            if let Some(app) = weak.upgrade() {
                app.confirm_exit(opener);
            }
        }));
        self.install_shortcuts();
    }

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

    pub fn set_notifier(&self, notifier: Rc<dyn Notifier>) {
        *self.notifier.borrow_mut() = Some(notifier);
    }

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
        let requests = self
            .watcher
            .borrow_mut()
            .observe(&self.store.state().objectives);
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
        if changes.contains(&Change::WorkspaceOpened) {
            let state = self.store.state();
            if state
                .notices
                .last()
                .is_some_and(|n| n.scope == NoticeScope::WorkspaceOpen)
            {
                self.notices_seen.set(state.notices_received);
                drop(state);
                self.render_notice();
            }
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
        } else if changes.contains(&Change::Selection) {
            self.sync_inspector();
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
}

#[cfg(test)]
mod tests;

/// The bar of toggles that switches between regions when only one fits, hidden until then.
fn region_bar() -> (gtk::Box, Vec<(Region, gtk::ToggleButton)>) {
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
    (region_bar, region_switches)
}

/// The centre stack, holding one named page per centre view.
fn centre_stack(pages: &[(&gtk::Widget, &str)]) -> gtk::Stack {
    let centre = gtk::Stack::new();
    for (page, name) in pages {
        centre.add_named(*page, Some(name));
    }
    centre.set_hexpand(true);
    centre.set_vexpand(true);
    centre.set_vhomogeneous(false);
    centre.set_hhomogeneous(false);
    centre.add_css_class("centre-stack");
    centre
}

/// The scrolled sheet the centre stack sits in.
fn centre_region(centre: &gtk::Stack) -> gtk::ScrolledWindow {
    let centre_region = widgets::region_scroll(centre);
    centre_region.set_hscrollbar_policy(gtk::PolicyType::External);
    centre_region.set_hexpand(true);
    centre_region.add_css_class("centre-region");
    centre_region.add_css_class("sheet");
    centre_region.set_overflow(gtk::Overflow::Hidden);
    widgets::set_accessible_label(&centre_region, "Main work area");
    centre_region
}

/// The rail beside the centre and inspector, as the outer pane and the inner one on its right.
fn panes(
    rail: &gtk::Widget,
    centre_region: &gtk::ScrolledWindow,
    inspector: &gtk::Widget,
) -> (gtk::Paned, gtk::Paned) {
    inspector.set_hexpand(false);
    inspector.add_css_class("sheet");
    inspector.set_overflow(gtk::Overflow::Hidden);
    widgets::set_accessible_label(inspector, "Inspector");
    let right = gtk::Paned::new(gtk::Orientation::Horizontal);
    right.set_start_child(Some(centre_region));
    right.set_end_child(Some(inspector));
    right.set_resize_start_child(true);
    right.set_resize_end_child(false);
    right.set_shrink_start_child(false);
    right.set_shrink_end_child(true);
    right.set_wide_handle(false);
    let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    paned.set_start_child(Some(rail));
    paned.set_end_child(Some(&right));
    paned.set_resize_start_child(false);
    paned.set_shrink_start_child(true);
    paned.set_shrink_end_child(false);
    paned.set_position(264);
    paned.set_wide_handle(false);
    paned.connect_position_notify(|p| {
        let clamped = p.position().clamp(224, 280);
        if clamped != p.position() {
            p.set_position(clamped);
        }
    });
    (paned, right)
}

/// The application's main window, before it has any content.
fn main_window(application: &gtk::Application) -> gtk::ApplicationWindow {
    let window = gtk::ApplicationWindow::builder()
        .application(application)
        .title("LADE")
        .default_width(1440)
        .default_height(900)
        .build();
    window.add_css_class("lade-window");
    window
}

/// An invisible, full-width strip whose allocation reports the window's usable width.
fn width_probe() -> gtk::DrawingArea {
    let probe = gtk::DrawingArea::new();
    probe.set_hexpand(true);
    probe.set_content_height(0);
    probe.set_can_target(false);
    probe.set_focusable(false);
    probe
}
