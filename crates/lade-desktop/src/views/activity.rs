use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use super::session;
use super::widgets::{
    self, action_row, hbox, label, set_accessible_label, set_focus_key, space, vbox,
};
use crate::presentation::controller::Controller;
use crate::presentation::format;
use crate::presentation::model::{
    ActionName, ActivityItem, ActivityRole, DeliveryMode, DeliveryState, ObjectiveId, Timestamp,
};
use crate::presentation::panel::{self, Tone};
use crate::presentation::store::{ActivityChange, Change};
use crate::presentation::text_bound::TEXT_LIMIT;

const MODES: [DeliveryMode; 3] = [
    DeliveryMode::Prompt,
    DeliveryMode::Steer,
    DeliveryMode::FollowUp,
];

pub struct Activity {
    pub root: gtk::Box,
    controller: Rc<Controller>,
    title: gtk::Label,
    status: gtk::Label,
    controls: gtk::Box,
    earlier: gtk::Button,
    list: gtk::ListBox,
    scroll: gtk::ScrolledWindow,
    text_labels: RefCell<Vec<gtk::Label>>,
    shown_for: RefCell<Option<ObjectiveId>>,
    pub composer: gtk::TextView,
    composer_input: Rc<widgets::LimitedInput>,
    mode: gtk::DropDown,
    send_area: gtk::Box,
    composer_error: gtk::Box,
    updating: Cell<bool>,
    follow: Cell<bool>,
    auto_scrolling: Cell<bool>,
    scroll_pending: Cell<bool>,
    agent_panel: gtk::Box,
    agent_error: RefCell<Option<String>>,
    control_error: gtk::Box,
    inputs_alert: gtk::Box,
    inputs_list: gtk::Box,
}

pub const AGENT_TOOLS_TITLE: &str = "Model, Agent Session and profile";
pub const INPUTS_TITLE: &str = "Accepted inputs and delivery";

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
        let status = label("", &["meta"]);
        titles.append(&status);
        let control_error = vbox(0);
        titles.append(&control_error);
        head.append(&titles);
        let controls = hbox(space::ITEM);
        controls.set_hexpand(false);
        controls.set_valign(gtk::Align::Start);
        head.append(&controls);
        root.append(&head);

        let agent_panel = vbox(space::PAGE);
        agent_panel.add_css_class("agent-panel");
        let agent_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(360)
            .overlay_scrolling(false)
            .child(&agent_panel)
            .build();
        set_accessible_label(&agent_scroll, AGENT_TOOLS_TITLE);
        root.append(&widgets::disclosure(
            AGENT_TOOLS_TITLE,
            false,
            &agent_scroll,
            |_| {},
        ));

        let inputs_alert = vbox(space::TIGHT);
        root.append(&inputs_alert);
        let inputs_list = vbox(space::TIGHT);
        let inputs_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(240)
            .overlay_scrolling(false)
            .child(&inputs_list)
            .build();
        set_accessible_label(&inputs_scroll, INPUTS_TITLE);
        root.append(&widgets::disclosure(
            INPUTS_TITLE,
            false,
            &inputs_scroll,
            |_| {},
        ));

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

        let composer_box = vbox(space::ITEM);
        composer_box.add_css_class("composer");
        let composer = gtk::TextView::new();
        set_accessible_label(&composer, "Message to the agent");
        set_focus_key(&composer, "activity:composer");
        let composer_input =
            widgets::LimitedInput::new(&composer, TEXT_LIMIT, "this message to the agent");
        composer_box.append(&composer_input.field(&widgets::bounded_text(&composer, 64, 160)));
        let row = hbox(space::GROUP);
        let mode = gtk::DropDown::from_strings(&["Prompt", "Steer", "Follow-up"]);
        set_accessible_label(&mode, "Delivery mode");
        mode.set_valign(gtk::Align::Start);
        row.append(&mode);
        let explain = label("", &["meta"]);
        explain.set_hexpand(true);
        row.append(&explain);
        let send_area = vbox(space::TIGHT);
        send_area.set_hexpand(false);
        send_area.set_size_request(160, -1);
        row.append(&send_area);
        composer_box.append(&row);
        let composer_error = vbox(0);
        composer_box.append(&composer_error);
        root.append(&composer_box);

        let activity = Rc::new(Self {
            root,
            controller: controller.clone(),
            title,
            status,
            controls,
            earlier,
            list,
            scroll,
            text_labels: RefCell::new(Vec::new()),
            shown_for: RefCell::new(None),
            composer,
            composer_input,
            mode,
            send_area,
            composer_error,
            updating: Cell::new(false),
            follow: Cell::new(true),
            auto_scrolling: Cell::new(false),
            scroll_pending: Cell::new(false),
            agent_panel,
            agent_error: RefCell::new(None),
            control_error,
            inputs_alert,
            inputs_list,
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
        activity.render_header();
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
            let Some(id) = state.ui.selected.as_ref() else {
                return;
            };
            state.inputs.get(id).cloned().unwrap_or_default()
        };
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
        if inputs.is_empty() {
            self.inputs_list
                .append(&label("Nothing accepted yet.", &["meta"]));
        }
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

    fn show_control_result(&self, result: Result<(), String>) {
        widgets::clear(&self.control_error);
        if let Err(message) = result {
            self.control_error.append(&widgets::error_line(&message));
        }
    }

    fn render_agent(self: &Rc<Self>) {
        widgets::clear(&self.agent_panel);
        let detail = self.controller.store.state().selected_detail().cloned();
        let Some(detail) = detail else {
            self.agent_panel
                .append(&label("Select an Objective to see its agent.", &["meta"]));
            return;
        };
        if let Some(error) = self.agent_error.borrow().as_ref() {
            self.agent_panel.append(&widgets::error_line(error));
        }
        let model = vbox(space::GROUP);
        model.append(&widgets::section_title("Model and thinking"));
        model.append(&label(
            "Used by Start agent and by Apply model and thinking.",
            &["meta"],
        ));
        model.append(&session::model_picker(&self.controller));
        self.agent_panel.append(&model);
        if detail.session.is_some() {
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
                        let panel = this.agent_panel.clone();
                        widgets::rebuild_preserving_focus(&panel, || this.render_agent());
                    }
                }
            });
            self.agent_panel
                .append(&session::session_section(&self.controller, &detail, report));
        }
        self.agent_panel
            .append(&session::resources_section(&self.controller));
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
            Err(message) => self.composer_error.append(&widgets::error_line(&message)),
        }
    }

    pub fn composer_has_focus(&self) -> bool {
        self.composer.is_focus()
    }

    pub fn refresh(self: &Rc<Self>, changes: &[Change]) {
        let selected = self.controller.store.state().ui.selected.clone();
        let mut agent = false;
        for change in changes {
            match change {
                Change::Selection => {
                    self.agent_error.replace(None);
                    widgets::clear(&self.control_error);
                    self.restore_composer();
                    self.render_header();
                    self.render_items();
                    self.render_inputs();
                    agent = true;
                }
                Change::Inputs(id) if Some(id) == selected.as_ref() => self.render_inputs(),
                Change::Catalog => {
                    self.render_header();
                    agent = true;
                }
                Change::Detail(id) if Some(id) == selected.as_ref() => {
                    self.render_header();
                    agent = true;
                }
                Change::Session(id) if Some(id) == selected.as_ref() => agent = true,
                Change::Resources => agent = true,
                Change::Connection => self.render_header(),
                Change::Activity(id, kind) if Some(id) == selected.as_ref() => match kind {
                    ActivityChange::Reset => self.render_items(),
                    ActivityChange::Appended { from_index } => self.append_items(*from_index),
                    ActivityChange::Delta { index } => self.apply_delta(*index),
                },
                _ => {}
            }
        }
        if agent {
            let panel = self.agent_panel.clone();
            widgets::rebuild_preserving_focus(&panel, || self.render_agent());
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

    fn render_header(self: &Rc<Self>) {
        let state = self.controller.store.state();
        let title = state.selected_summary().map_or_else(
            || "Activity".to_owned(),
            |s| format!("Activity — {}", s.title),
        );
        let detail = state.selected_detail().cloned();
        drop(state);
        self.title.set_text(&title);
        let (mut line, full) = self.controller.execution_line().unwrap_or_else(|| {
            let none = "Select an Objective to see its Agent Session.".to_owned();
            (none.clone(), none)
        });
        widgets::clear(&self.controls);
        widgets::clear(&self.send_area);
        let Some(detail) = detail else {
            self.status.set_text(&line);
            return;
        };
        let controls = panel::activity_controls(&detail);
        if controls
            .iter()
            .any(|p| p.action == ActionName::ExecutionStart && p.tone == Tone::Primary)
        {
            self.controller.picker_defaults();
            line.push_str(&match self.controller.chosen_model() {
                Ok((model, thinking)) => format!(
                    " · starts with {} · thinking {}",
                    model.label(),
                    thinking.label()
                ),
                Err(message) => format!(" · {message}"),
            });
        }
        self.status.set_text(&line);
        self.status.set_tooltip_text(Some(&full));
        widgets::set_accessible_description(&self.status, &full);
        for control in controls {
            let this = Rc::downgrade(self);
            let action = control.action;
            let button =
                widgets::compact_action(&control.label, &control.availability, move |_| {
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
            if control.tone == Tone::Primary {
                button.add_css_class("primary");
            }
            self.controls.append(&button);
        }
        let this = Rc::downgrade(self);
        self.send_area.append(&action_row(
            "Send",
            &detail.availability(ActionName::MessageSend),
            &["primary"],
            move |_| {
                if let Some(this) = this.upgrade() {
                    this.send();
                }
            },
        ));
    }

    fn item_row(item: &ActivityItem) -> (gtk::ListBoxRow, gtk::Label) {
        let row = gtk::ListBoxRow::new();
        row.set_activatable(false);
        row.add_css_class("activity-item");
        if item.role == ActivityRole::User {
            row.add_css_class("from-user");
        }
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
        let Some(id) = id else {
            self.earlier.set_visible(false);
            return;
        };
        let log = state.activity.get(&id).cloned().unwrap_or_default();
        drop(state);
        self.earlier.set_visible(log.has_more);
        if !log.loaded {
            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&label("Loading activity…", &["meta"])));
            self.list.append(&row);
            return;
        }
        if log.items.is_empty() {
            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&label("No activity yet.", &["meta"])));
            self.list.append(&row);
        }
        for item in &log.items {
            let (row, text) = Self::item_row(item);
            self.list.append(&row);
            self.text_labels.borrow_mut().push(text);
        }
        self.follow.set(true);
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
