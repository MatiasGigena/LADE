//! The Activity region: the agent's transcript and controls, its setup card and settings, and the
//! accepted-input ledger.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use super::session;
use super::widgets::{
    self, MessageKind, hbox, label, set_accessible_label, set_focus_key, space, vbox,
};
use crate::presentation::controller::Controller;
use crate::presentation::state::model::{DeliveryMode, ObjectiveId};
use crate::presentation::state::store::{ActivityChange, Change};
use crate::presentation::text::bound::TEXT_LIMIT;
use crate::presentation::text::format;

mod agent;
mod log;

const MODES: [DeliveryMode; 3] = [
    DeliveryMode::Prompt,
    DeliveryMode::Steer,
    DeliveryMode::FollowUp,
];

/// The delivery mode at a mode dropdown position. With no selection (GTK reports
/// `INVALID_LIST_POSITION`) a message is a plain prompt.
fn mode_at(position: u32) -> DeliveryMode {
    MODES
        .get(position as usize)
        .copied()
        .unwrap_or(DeliveryMode::Prompt)
}

/// The agent area: the setup shown before the agent starts and the settings shown after, as the
/// area, the setup, its body and resources, the settings disclosure and its panel.
fn agent_area() -> (gtk::Box, gtk::Box, gtk::Box, gtk::Box, gtk::Box, gtk::Box) {
    let agent_area = vbox(space::SECTION);
    let setup = vbox(space::SECTION);
    setup.add_css_class("agent-setup");
    let setup_body = vbox(space::GROUP);
    setup.append(&setup_body);
    let setup_resources = vbox(0);
    setup.append(&widgets::disclosure(
        session::RESOURCES_TITLE,
        false,
        &setup_resources,
        |_| {},
    ));
    agent_area.append(&setup);
    let agent_panel = vbox(space::PAGE);
    agent_panel.add_css_class("agent-panel");
    set_accessible_label(&agent_panel, AGENT_SETTINGS_TITLE);
    let settings = widgets::disclosure(AGENT_SETTINGS_TITLE, false, &agent_panel, |_| {});
    agent_area.append(&settings);
    (
        agent_area,
        setup,
        setup_body,
        setup_resources,
        settings,
        agent_panel,
    )
}

/// The Activity heading: its title and status beside the execution controls.
fn heading() -> (gtk::Box, gtk::Label, gtk::Box) {
    let head = hbox(space::SECTION);
    let titles = vbox(space::TIGHT);
    titles.set_hexpand(true);
    let title = widgets::heading("Activity", "objective-heading");
    set_focus_key(&title, "activity:title");
    title.set_focusable(true);
    titles.append(&title);
    let status = label("", &["secondary"]);
    titles.append(&status);
    head.append(&titles);
    let controls = hbox(space::ITEM);
    controls.set_hexpand(false);
    controls.set_valign(gtk::Align::Start);
    head.append(&controls);
    (head, status, controls)
}

/// The Agent Session transcript and the scroller around it.
fn transcript() -> (gtk::ListBox, gtk::ScrolledWindow) {
    let list = gtk::ListBox::new();
    list.add_css_class("transcript");
    list.set_selection_mode(gtk::SelectionMode::None);
    set_accessible_label(&list, "Agent Session transcript");
    let scroll = widgets::region_scroll(&list);
    scroll.add_css_class("transcript-scroll");
    scroll.set_vexpand(true);
    scroll.set_min_content_height(160);
    (list, scroll)
}

/// The row under the composer: the delivery mode, its explanation and the send area.
fn delivery_row() -> (gtk::Box, gtk::DropDown, gtk::Label, gtk::Box) {
    let row = hbox(space::GROUP);
    let mode = widgets::dropdown(&MODES.map(format::mode_label));
    mode.set_hexpand(false);
    set_accessible_label(&mode, "Delivery mode");
    mode.set_valign(gtk::Align::Center);
    row.append(&mode);
    let explain = label("", &["meta"]);
    explain.set_hexpand(true);
    row.append(&explain);
    let send_area = hbox(0);
    send_area.set_valign(gtk::Align::Center);
    row.append(&send_area);
    (row, mode, explain, send_area)
}

fn fit(container: &gtk::Box) {
    container.set_visible(container.first_child().is_some());
}

pub struct Activity {
    pub root: gtk::Box,
    controller: Rc<Controller>,
    status: gtk::Label,
    controls: gtk::Box,
    state_message: gtk::Box,
    control_error: gtk::Box,
    control_refusal: gtk::Box,
    agent_area: gtk::Box,
    setup: gtk::Box,
    setup_body: RefCell<gtk::Box>,
    setup_resources: gtk::Box,
    settings: gtk::Box,
    agent_panel: gtk::Box,
    agent_error: RefCell<Option<String>>,
    inputs_alert: gtk::Box,
    inputs: gtk::Box,
    inputs_list: gtk::Box,
    earlier: gtk::Button,
    list: gtk::ListBox,
    scroll: gtk::ScrolledWindow,
    empty: gtk::Box,
    text_labels: RefCell<Vec<gtk::Label>>,
    shown_for: RefCell<Option<ObjectiveId>>,
    setup_for: RefCell<Option<ObjectiveId>>,
    composer_box: gtk::Box,
    pub composer: gtk::TextView,
    composer_input: Rc<widgets::LimitedInput>,
    mode: gtk::DropDown,
    send_area: gtk::Box,
    composer_error: gtk::Box,
    send_refusal: gtk::Box,
    updating: Cell<bool>,
    follow: Cell<bool>,
    auto_scrolling: Cell<bool>,
    scroll_pending: Cell<bool>,
}

pub const AGENT_SETTINGS_TITLE: &str = "Agent settings";
pub const INPUTS_TITLE: &str = "Accepted inputs and delivery";
pub const SETUP_TITLE: &str = "Set up the agent";
const SETUP_NOTE: &str = "Choose the model the agent works with, then Start agent. Once it starts, \
                          you can message it here and follow its work.";
const SETTINGS_MODEL_NOTE: &str = "To switch the agent to another model or thinking level, choose them here and use Apply model \
     and thinking below.";
const SETTINGS_RESTART_NOTE: &str = "Start agent, above, starts the agent again with these.";

impl Activity {
    pub fn new(controller: &Rc<Controller>) -> Rc<Self> {
        let root = vbox(space::SECTION);
        root.add_css_class("activity");
        let (head, status, controls) = heading();
        root.append(&head);
        let state_message = vbox(0);
        root.append(&state_message);
        let control_error = vbox(0);
        root.append(&control_error);
        let control_refusal = vbox(space::TIGHT);
        root.append(&control_refusal);

        let (agent_area, setup, setup_body, setup_resources, settings, agent_panel) = agent_area();
        root.append(&agent_area);

        let inputs_alert = vbox(space::TIGHT);
        root.append(&inputs_alert);
        let inputs_list = vbox(space::TIGHT);
        set_accessible_label(&inputs_list, INPUTS_TITLE);
        let inputs = widgets::disclosure(INPUTS_TITLE, false, &inputs_list, |_| {});
        root.append(&inputs);

        let earlier = widgets::button("Load earlier activity", &["quiet"]);
        earlier.set_halign(gtk::Align::Start);
        root.append(&earlier);
        let (list, scroll) = transcript();
        root.append(&scroll);
        let empty = vbox(0);
        root.append(&empty);

        let composer_box = vbox(space::ITEM);
        composer_box.add_css_class("composer");
        let composer = gtk::TextView::new();
        set_accessible_label(&composer, "Message to the agent");
        set_focus_key(&composer, "activity:composer");
        let composer_input =
            widgets::LimitedInput::new(&composer, TEXT_LIMIT, "this message to the agent");
        composer_box.append(&composer_input.field(&widgets::placeholder_text(
            &composer,
            "Message the agent…",
            64,
            160,
        )));
        let (row, mode, explain, send_area) = delivery_row();
        composer_box.append(&row);
        let composer_error = vbox(0);
        composer_error.set_visible(false);
        composer_box.append(&composer_error);
        let send_refusal = vbox(0);
        send_refusal.set_visible(false);
        composer_box.append(&send_refusal);
        root.append(&composer_box);

        let activity = Rc::new(Self {
            root,
            controller: controller.clone(),
            status,
            controls,
            state_message,
            control_error,
            control_refusal,
            agent_area,
            setup,
            setup_body: RefCell::new(setup_body),
            setup_resources,
            settings,
            agent_panel,
            agent_error: RefCell::new(None),
            inputs_alert,
            inputs,
            inputs_list,
            earlier,
            list,
            scroll,
            empty,
            text_labels: RefCell::new(Vec::new()),
            shown_for: RefCell::new(None),
            setup_for: RefCell::new(None),
            composer_box,
            composer,
            composer_input,
            mode,
            send_area,
            composer_error,
            send_refusal,
            updating: Cell::new(false),
            follow: Cell::new(true),
            auto_scrolling: Cell::new(false),
            scroll_pending: Cell::new(false),
        });
        activity.connect_composer(explain);
        activity.connect_follow();
        let controller = controller.clone();
        activity
            .earlier
            .connect_clicked(move |_| controller.load_older_activity());
        activity.render_state();
        activity.render_items();
        activity.render_agent();
        activity.render_inputs();
        activity
    }

    /// Keeps the delivery mode's explanation current and stores the composer as it changes.
    fn connect_composer(self: &Rc<Self>, explain: gtk::Label) {
        let set_explain = move |d: &gtk::DropDown| {
            explain.set_text(format::mode_explanation(mode_at(d.selected())));
        };
        set_explain(&self.mode);
        let this = Rc::downgrade(self);
        self.mode.connect_selected_notify(move |d| {
            set_explain(d);
            if let Some(this) = this.upgrade()
                && !this.updating.get()
            {
                this.store_composer();
            }
        });
        let this = Rc::downgrade(self);
        self.composer_input.connect_commit(move |_| {
            if let Some(this) = this.upgrade() {
                this.store_composer();
            }
        });
    }

    /// Follows the transcript's tail while the reader is at the bottom of it.
    fn connect_follow(self: &Rc<Self>) {
        let adjustment = self.scroll.vadjustment();
        let this = Rc::downgrade(self);
        adjustment.connect_value_changed(move |adj| {
            if let Some(this) = this.upgrade()
                && !this.auto_scrolling.get()
            {
                this.follow
                    .set(adj.value() + adj.page_size() >= adj.upper() - 8.0);
            }
        });
        let follow_tail = {
            let this = Rc::downgrade(self);
            move |_: &gtk::Adjustment| {
                let Some(this) = this.upgrade() else { return };
                if !this.follow.get() || this.scroll_pending.replace(true) {
                    return;
                }
                let weak = Rc::downgrade(&this);
                gtk::glib::idle_add_local_once(move || {
                    if let Some(this) = weak.upgrade() {
                        this.scroll_pending.set(false);
                        let adj = this.scroll.vadjustment();
                        this.auto_scrolling.set(true);
                        adj.set_value(adj.upper() - adj.page_size());
                        this.auto_scrolling.set(false);
                    }
                });
            }
        };
        adjustment.connect_upper_notify(follow_tail.clone());
        adjustment.connect_page_size_notify(follow_tail);
    }

    fn composer_text(&self) -> String {
        self.composer_input.text()
    }

    fn store_composer(&self) {
        let mode = mode_at(self.mode.selected());
        self.controller.set_composer(&self.composer_text(), mode);
    }

    pub fn send(&self) {
        let mode = mode_at(self.mode.selected());
        widgets::clear(&self.composer_error);
        match self.controller.send_message(&self.composer_text(), mode) {
            Ok(()) => {
                self.composer_input.set_text("");
                self.store_composer();
            }
            Err(message) => self.composer_error.append(&widgets::inline_message(
                MessageKind::Error,
                &message,
                None,
            )),
        }
        fit(&self.composer_error);
    }

    pub fn composer_has_focus(&self) -> bool {
        self.composer.is_focus()
    }

    pub fn refresh(self: &Rc<Self>, changes: &[Change]) {
        let selected = self.controller.store.state().ui.selected.clone();
        let mut agent = false;
        let mut state = false;
        for change in changes {
            match change {
                Change::Selection => {
                    self.clear_errors();
                    self.restore_composer();
                    self.render_items();
                    self.render_inputs();
                    state = true;
                    agent = true;
                }
                Change::Inputs(id) if Some(id) == selected.as_ref() => self.render_inputs(),
                Change::Catalog => {
                    self.clear_errors();
                    state = true;
                    agent = true;
                }
                Change::Detail(id) if Some(id) == selected.as_ref() => {
                    self.clear_errors();
                    state = true;
                    agent = true;
                }
                Change::Session(id) if Some(id) == selected.as_ref() => agent = true,
                Change::Resources => agent = true,
                Change::Connection | Change::Objectives => state = true,
                Change::Activity(id, kind) if Some(id) == selected.as_ref() => {
                    match kind {
                        ActivityChange::Reset => self.render_items(),
                        ActivityChange::Appended { from_index } => {
                            self.append_items(*from_index);
                        }
                        ActivityChange::Delta { index } => self.apply_delta(*index),
                    }
                    self.sync_body();
                }
                _ => {}
            }
        }
        if state {
            self.render_state();
        }
        if agent {
            self.rebuild_agent();
        }
    }

    fn restore_composer(&self) {
        let (text, mode) = self
            .controller
            .store
            .state()
            .selected_ui()
            .map_or((String::new(), DeliveryMode::Prompt), |u| {
                (u.composer_text.clone(), u.composer_mode)
            });
        self.composer_input.set_text(&text);
        self.updating.set(true);
        if let Some(index) = MODES
            .iter()
            .position(|m| *m == mode)
            .and_then(|i| u32::try_from(i).ok())
        {
            self.mode.set_selected(index);
        }
        self.updating.set(false);
    }
}
