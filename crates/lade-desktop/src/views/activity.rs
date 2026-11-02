use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use super::session;
use super::widgets::{
    self, MessageKind, hbox, label, set_accessible_label, set_focus_key, space, vbox,
};
use super::workbench;
use crate::presentation::controller::Controller;
use crate::presentation::format;
use crate::presentation::model::{
    ActionAvailability, ActionName, ActionSurface, ActivityItem, ActivityRole, DeliveryMode,
    DeliveryState, ExecutionState, ObjectiveDetail, ObjectiveId, Timestamp,
};
use crate::presentation::panel::{self, PanelAction, Tone};
use crate::presentation::store::{ActivityChange, Change};
use crate::presentation::text_bound::TEXT_LIMIT;

const MODES: [DeliveryMode; 3] = [
    DeliveryMode::Prompt,
    DeliveryMode::Steer,
    DeliveryMode::FollowUp,
];

fn fit(container: &gtk::Box) {
    container.set_visible(container.first_child().is_some());
}

pub struct Activity {
    pub root: gtk::Box,
    controller: Rc<Controller>,
    title: gtk::Label,
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
        root.append(&head);
        let state_message = vbox(0);
        root.append(&state_message);
        let control_error = vbox(0);
        root.append(&control_error);
        let control_refusal = vbox(space::TIGHT);
        root.append(&control_refusal);

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
        let list = gtk::ListBox::new();
        list.add_css_class("transcript");
        list.set_selection_mode(gtk::SelectionMode::None);
        set_accessible_label(&list, "Agent Session transcript");
        let scroll = widgets::region_scroll(&list);
        scroll.set_vexpand(true);
        scroll.set_min_content_height(160);
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
        let row = hbox(space::GROUP);
        let mode = widgets::dropdown(&["Prompt", "Steer", "Follow-up"]);
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
            title,
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
        let set_explain = move |d: &gtk::DropDown| {
            let mode = MODES
                .get(d.selected() as usize)
                .copied()
                .unwrap_or(DeliveryMode::Prompt);
            explain.set_text(format::mode_explanation(mode));
        };
        set_explain(&activity.mode);
        let this = Rc::downgrade(&activity);
        activity.mode.connect_selected_notify(move |d| {
            set_explain(d);
            if let Some(this) = this.upgrade()
                && !this.updating.get()
            {
                this.store_composer();
            }
        });
        let this = Rc::downgrade(&activity);
        activity.composer_input.connect_commit(move |_| {
            if let Some(this) = this.upgrade() {
                this.store_composer();
            }
        });
        let adjustment = activity.scroll.vadjustment();
        let this = Rc::downgrade(&activity);
        adjustment.connect_value_changed(move |adj| {
            if let Some(this) = this.upgrade()
                && !this.auto_scrolling.get()
            {
                this.follow
                    .set(adj.value() + adj.page_size() >= adj.upper() - 8.0);
            }
        });
        let follow_tail = {
            let this = Rc::downgrade(&activity);
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

    fn render_inputs(&self) {
        widgets::clear(&self.inputs_alert);
        widgets::clear(&self.inputs_list);
        let inputs = {
            let state = self.controller.store.state();
            state
                .ui
                .selected
                .as_ref()
                .and_then(|id| state.inputs.get(id).cloned())
                .unwrap_or_default()
        };
        self.inputs.set_visible(!inputs.is_empty());
        let troubled = inputs
            .iter()
            .filter(|i| {
                matches!(
                    i.delivery,
                    DeliveryState::Uncertain | DeliveryState::Blocked
                )
            })
            .count();
        if troubled > 0 {
            self.inputs_alert.append(&widgets::warning_line(&format!(
                "{troubled} accepted input{} with delivery uncertain or blocked — never resent automatically; see {INPUTS_TITLE}.",
                if troubled == 1 { "" } else { "s" }
            )));
        }
        fit(&self.inputs_alert);
        for input in inputs.iter().rev() {
            let row = vbox(space::TIGHT);
            row.add_css_class("evidence-row");
            let line = format::input_line(input);
            let state_label = label(&line, &["strong"]);
            if matches!(
                input.delivery,
                DeliveryState::Uncertain | DeliveryState::Blocked
            ) {
                state_label.add_css_class("warning-text");
            }
            row.append(&state_label);
            row.append(&label(&input.text_preview, &[]));
            row.append(&label(
                &format!(
                    "Accepted {} · input {}",
                    format::relative_time(Timestamp(widgets::now_ms()), input.accepted_at),
                    input.input_id.short()
                ),
                &["meta"],
            ));
            set_accessible_label(&row, &format!("{line}: {}", input.text_preview));
            self.inputs_list.append(&row);
        }
    }

    fn refusal(&self, surface: ActionSurface) -> Option<gtk::Box> {
        let state = self.controller.store.state();
        let refusal = state.last_refusal(state.ui.selected.as_ref()?, surface)?;
        Some(widgets::inline_message(
            MessageKind::Warning,
            &refusal.message,
            Some(&refusal.details),
        ))
    }

    fn show_control_result(&self, result: Result<(), String>) {
        widgets::clear(&self.control_error);
        if let Err(message) = result {
            self.control_error
                .append(&widgets::inline_message(MessageKind::Error, &message, None));
        }
        fit(&self.control_error);
    }

    fn clear_errors(&self) {
        widgets::clear(&self.control_error);
        fit(&self.control_error);
        self.agent_error.replace(None);
    }

    fn agent_button(self: &Rc<Self>, control: &PanelAction) -> gtk::Button {
        let availability = match (control.action, &control.availability) {
            (ActionName::ExecutionStart, ActionAvailability::Enabled) => {
                match self.controller.chosen_model() {
                    Ok(_) => ActionAvailability::Enabled,
                    Err(reason) => ActionAvailability::Disabled { reason },
                }
            }
            (_, availability) => availability.clone(),
        };
        let this = Rc::downgrade(self);
        let action = control.action;
        let button = widgets::compact_action(&control.label, &availability, move |_| {
            if let Some(this) = this.upgrade() {
                let controller = &this.controller;
                let result = match action {
                    ActionName::ExecutionStart => {
                        controller.chosen_model().and_then(|(model, thinking)| {
                            controller.start_execution(model, Some(thinking))
                        })
                    }
                    ActionName::ExecutionCancel => controller.cancel(),
                    _ => controller.resume(),
                };
                this.show_control_result(result);
            }
        });
        if control.tone == Tone::Primary && availability == ActionAvailability::Enabled {
            button.add_css_class("primary");
        }
        button
    }

    fn render_setup(self: &Rc<Self>, detail: &ObjectiveDetail) {
        let body = &vbox(space::GROUP);
        let heading = widgets::section_title(SETUP_TITLE);
        body.append(&heading);
        body.append(&label(SETUP_NOTE, &["secondary"]));
        if let Some(error) = self.agent_error.borrow().as_ref() {
            body.append(&widgets::inline_message(MessageKind::Error, error, None));
        }
        body.append(&session::model_picker(&self.controller));
        let start = panel::activity_controls(detail)
            .into_iter()
            .find(|c| c.action == ActionName::ExecutionStart);
        match start {
            Some(start) => {
                let row = vbox(space::TIGHT);
                let button = self.agent_button(&start);
                button.set_halign(gtk::Align::Start);
                row.append(&button);
                if let ActionAvailability::Disabled { reason } = &start.availability {
                    row.append(&label(reason, &["meta"]));
                }
                if let Some(refusal) = self.refusal(ActionSurface::Agent) {
                    row.append(&refusal);
                }
                body.append(&row);
            }
            None => body.append(&label(
                "Starting the agent is not offered for this Objective.",
                &["meta"],
            )),
        }
        let old = self.setup_body.replace(body.clone());
        self.setup.insert_child_after(body, Some(&old));
        self.setup.remove(&old);
        self.setup_resources
            .append(&session::resources_section(&self.controller));
    }

    fn render_settings(self: &Rc<Self>, detail: &ObjectiveDetail) {
        let panel = &self.agent_panel;
        if let Some(error) = self.agent_error.borrow().as_ref() {
            panel.append(&widgets::inline_message(MessageKind::Error, error, None));
        }
        if let Some(refusal) = self.refusal(ActionSurface::Session) {
            panel.append(&refusal);
        }
        let model = vbox(space::GROUP);
        model.append(&widgets::section_title("Model and thinking"));
        let restart =
            Self::relevant_control(detail).is_some_and(|c| c.action == ActionName::ExecutionStart);
        model.append(&label(
            if restart {
                SETTINGS_RESTART_NOTE
            } else {
                SETTINGS_MODEL_NOTE
            },
            &["meta"],
        ));
        model.append(&session::model_picker(&self.controller));
        panel.append(&model);
        let this = Rc::downgrade(self);
        let report: Rc<dyn Fn(Result<(), String>)> = Rc::new(move |result| {
            if let Some(this) = this.upgrade() {
                let changed = match result {
                    Ok(()) => this.agent_error.replace(None).is_some(),
                    Err(message) => {
                        this.agent_error.replace(Some(message));
                        true
                    }
                };
                if changed {
                    this.rebuild_agent();
                }
            }
        });
        panel.append(&session::session_section(&self.controller, detail, report));
        let resources = vbox(space::GROUP);
        resources.append(&widgets::section_title(session::RESOURCES_TITLE));
        resources.append(&session::resources_section(&self.controller));
        panel.append(&resources);
    }

    fn render_agent(self: &Rc<Self>) -> bool {
        widgets::clear(&self.setup_body.borrow());
        widgets::clear(&self.setup_resources);
        widgets::clear(&self.agent_panel);
        let detail = self.controller.store.state().selected_detail().cloned();
        let session = detail.as_ref().is_some_and(|d| d.session.is_some());
        self.setup.set_visible(detail.is_some() && !session);
        self.settings.set_visible(session);
        let id = detail.as_ref().map(|d| d.summary.objective_id.clone());
        let previous = self.setup_for.replace(id.clone().filter(|_| !session));
        match detail {
            Some(detail) if session => self.render_settings(&detail),
            Some(detail) => self.render_setup(&detail),
            None => {}
        }
        session && previous.is_some() && previous == id
    }

    fn rebuild_agent(self: &Rc<Self>) {
        let in_setup = self
            .root
            .root()
            .and_then(|r| r.focus())
            .is_some_and(|f| f.is_ancestor(&self.setup));
        let mut started = false;
        widgets::rebuild_preserving_focus(&self.agent_area, || started = self.render_agent());
        if !started {
            return;
        }
        let sentence = self
            .controller
            .agent_line()
            .map_or_else(|| "The agent started.".to_owned(), |l| l.sentence);
        let message = if in_setup && self.composer_box.is_visible() && self.composer.grab_focus() {
            format!("{sentence} Focus is on the message to the agent.")
        } else {
            sentence
        };
        widgets::announce(
            &self.root,
            &message,
            gtk::AccessibleAnnouncementPriority::Medium,
        );
    }

    fn composer_text(&self) -> String {
        self.composer_input.text()
    }

    fn store_composer(&self) {
        let mode = MODES[self.mode.selected() as usize % MODES.len()];
        self.controller.set_composer(&self.composer_text(), mode);
    }

    pub fn send(&self) {
        let mode = MODES[self.mode.selected() as usize % MODES.len()];
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
            .map(|u| (u.composer_text.clone(), u.composer_mode))
            .unwrap_or((String::new(), DeliveryMode::Prompt));
        self.composer_input.set_text(&text);
        self.updating.set(true);
        if let Some(index) = MODES.iter().position(|m| *m == mode) {
            self.mode.set_selected(index as u32);
        }
        self.updating.set(false);
    }

    fn relevant_control(detail: &ObjectiveDetail) -> Option<PanelAction> {
        let wanted: &[ActionName] = match detail.summary.execution.state {
            ExecutionState::Starting
            | ExecutionState::Running
            | ExecutionState::Idle
            | ExecutionState::Stopping => &[ActionName::ExecutionCancel],
            _ if detail.session.is_none() => &[],
            _ => &[ActionName::ExecutionResume, ActionName::ExecutionStart],
        };
        let offered: Vec<PanelAction> = panel::activity_controls(detail)
            .into_iter()
            .filter(|c| wanted.contains(&c.action))
            .collect();
        let rank = |c: &PanelAction| wanted.iter().position(|w| *w == c.action);
        offered
            .iter()
            .filter(|c| c.availability == ActionAvailability::Enabled)
            .min_by_key(|c| rank(c))
            .or_else(|| offered.iter().min_by_key(|c| rank(c)))
            .cloned()
    }

    fn render_state(self: &Rc<Self>) {
        let state = self.controller.store.state();
        let title = state.selected_summary().map_or_else(
            || "Activity".to_owned(),
            |s| format!("Activity — {}", s.title),
        );
        let detail = state.selected_detail().cloned();
        let selected = state.ui.selected.is_some();
        drop(state);
        self.title.set_text(&title);
        widgets::clear(&self.controls);
        widgets::clear(&self.state_message);
        fit(&self.state_message);
        widgets::clear(&self.control_refusal);
        widgets::clear(&self.send_refusal);
        widgets::clear(&self.send_area);
        let line = self.controller.agent_line();
        let (sentence, details) = match (&detail, line) {
            (Some(_), Some(line)) => (line.sentence, line.details),
            (Some(_), None) => ("Loading the agent's state…".to_owned(), None),
            (None, _) if selected => ("Loading the agent's state…".to_owned(), None),
            (None, _) => (String::new(), None),
        };
        self.status.set_visible(!sentence.is_empty());
        self.status.set_text(&sentence);
        self.status.set_tooltip_text(details.as_deref());
        widgets::set_accessible_description(&self.status, details.as_deref().unwrap_or(""));
        if let Some(detail) = &detail {
            let execution = &detail.summary.execution;
            if execution.state == ExecutionState::Failed
                && let Some(reason) = &execution.reason
            {
                self.state_message.append(&widgets::inline_message(
                    MessageKind::Error,
                    reason,
                    None,
                ));
            }
            fit(&self.state_message);
            if let Some(control) = Self::relevant_control(detail) {
                self.controls.append(&self.agent_button(&control));
            }
            let agent = detail
                .session
                .is_some()
                .then(|| self.refusal(ActionSurface::Agent))
                .flatten();
            for refusal in agent.into_iter().chain(self.refusal(ActionSurface::Dialog)) {
                self.control_refusal.append(&refusal);
            }
            if let Some(refusal) = self.refusal(ActionSurface::Send) {
                self.send_refusal.append(&refusal);
            }
            let availability = detail.availability(ActionName::MessageSend);
            let this = Rc::downgrade(self);
            let send = widgets::compact_action("Send", &availability, move |_| {
                if let Some(this) = this.upgrade() {
                    this.send();
                }
            });
            if availability == ActionAvailability::Enabled {
                send.add_css_class("primary");
            }
            self.send_area.append(&send);
        }
        fit(&self.control_refusal);
        fit(&self.send_refusal);
        self.sync_body();
    }

    fn sync_body(&self) {
        let state = self.controller.store.state();
        let detail = state.selected_detail();
        let session = detail.is_some_and(|d| d.session.is_some());
        let execution = detail.map(|d| d.summary.execution.state);
        let (loaded, items, more) = state
            .ui
            .selected
            .as_ref()
            .and_then(|id| state.activity.get(id))
            .map_or((false, false, false), |log| {
                (log.loaded, !log.items.is_empty(), log.has_more)
            });
        let has_detail = detail.is_some();
        let selected = state.ui.selected.is_some();
        let board_empty = workbench::board_is_empty(&state);
        drop(state);
        let transcript = has_detail && (items || (session && !loaded));
        self.scroll.set_visible(transcript);
        self.earlier.set_visible(transcript && more);
        self.composer_box.set_visible(session);
        widgets::clear(&self.empty);
        if !selected {
            self.empty.append(&if board_empty {
                workbench::first_objective_state(&self.controller)
            } else {
                widgets::empty_state(
                    "No Objective selected",
                    "Select an Objective to see its agent.",
                    None,
                    None,
                )
            });
            self.empty.set_visible(true);
            return;
        }
        let empty = match execution {
            None => None,
            Some(_) if transcript || !session => None,
            Some(ExecutionState::Idle) => {
                Some(("No messages yet", "Send a prompt to start a turn."))
            }
            Some(ExecutionState::Starting | ExecutionState::Running) => Some((
                "The agent is working",
                "Its messages appear here as they arrive.",
            )),
            Some(_) => Some((
                "No activity yet",
                "The agent's messages appear here once it runs.",
            )),
        };
        self.empty.set_visible(empty.is_some());
        if let Some((title, body)) = empty {
            self.empty
                .append(&widgets::empty_state(title, body, None, None));
        }
    }

    fn item_row(item: &ActivityItem) -> (gtk::ListBoxRow, gtk::Label) {
        let row = gtk::ListBoxRow::new();
        row.set_activatable(false);
        row.add_css_class("activity-item");
        row.add_css_class(match item.role {
            ActivityRole::User => "from-user",
            ActivityRole::Assistant => "from-agent",
            ActivityRole::Thinking => "from-thinking",
            ActivityRole::Tool => "from-tool",
            ActivityRole::System => "from-system",
        });
        let content = vbox(space::TIGHT);
        let meta = hbox(space::ITEM);
        let role = format::role_label(item.role, item.tool_name.as_deref());
        meta.append(&label(&role, &["strong", "role"]));
        if !item.title.is_empty() && item.title != role && item.role != ActivityRole::Assistant {
            meta.append(&widgets::line_label(
                &item.title,
                &["meta"],
                gtk::pango::EllipsizeMode::End,
            ));
        }
        if item.role == ActivityRole::Assistant {
            meta.append(&label("agent claim", &["meta"]));
        }
        if item.is_error {
            meta.append(&label("✕ Error", &["removed", "strong"]));
        }
        content.append(&meta);
        let mut classes = vec!["activity-text"];
        if item.role == ActivityRole::Tool {
            classes.push("mono");
        }
        if item.role == ActivityRole::Thinking {
            classes.push("secondary");
        }
        let text = label(&item.text, &classes);
        text.set_selectable(true);
        text.set_focusable(false);
        content.append(&text);
        if item.truncated {
            content.append(&widgets::warning_line(
                "Truncated — this item exceeded the retained size; the shown text is incomplete.",
            ));
        }
        row.set_child(Some(&content));
        let mut spoken = role;
        if item.is_error {
            spoken.push_str(", error");
        }
        if item.truncated {
            spoken.push_str(", truncated");
        }
        set_accessible_label(&row, &spoken);
        (row, text)
    }

    fn render_items(&self) {
        while let Some(row) = self.list.row_at_index(0) {
            self.list.remove(&row);
        }
        self.text_labels.borrow_mut().clear();
        let state = self.controller.store.state();
        let id = state.ui.selected.clone();
        *self.shown_for.borrow_mut() = id.clone();
        let log = id
            .and_then(|id| state.activity.get(&id).cloned())
            .unwrap_or_default();
        drop(state);
        if !log.loaded {
            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&label("Loading activity…", &["meta"])));
            self.list.append(&row);
        }
        for item in &log.items {
            let (row, text) = Self::item_row(item);
            self.list.append(&row);
            self.text_labels.borrow_mut().push(text);
        }
        self.follow.set(true);
        self.sync_body();
    }

    fn append_items(&self, from_index: usize) {
        let state = self.controller.store.state();
        let Some(id) = state.ui.selected.clone() else {
            return;
        };
        let Some(log) = state.activity.get(&id) else {
            return;
        };
        if from_index == 0 || self.text_labels.borrow().len() != from_index {
            drop(state);
            self.render_items();
            return;
        }
        for item in &log.items[from_index..] {
            let (row, text) = Self::item_row(item);
            self.list.append(&row);
            self.text_labels.borrow_mut().push(text);
        }
        drop(state);
    }

    fn apply_delta(&self, index: usize) {
        let state = self.controller.store.state();
        let Some(id) = state.ui.selected.as_ref() else {
            return;
        };
        let Some(item) = state.activity.get(id).and_then(|l| l.items.get(index)) else {
            return;
        };
        if let Some(text) = self.text_labels.borrow().get(index) {
            text.set_text(&item.text);
        }
        drop(state);
    }
}
