//! The agent panel: setup before the first start, its controls and settings, and its state line.

use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use crate::presentation::objective_actions::{self, PanelAction, Tone};
use crate::presentation::state::model::{
    ActionAvailability, ActionName, ActionSurface, ExecutionState, ObjectiveDetail,
};
use crate::views::session;
use crate::views::widgets::{self, MessageKind, label, space, vbox};

use super::{Activity, SETTINGS_MODEL_NOTE, SETTINGS_RESTART_NOTE, SETUP_NOTE, SETUP_TITLE, fit};
use crate::views::dialogs::costs_and_data;

impl Activity {
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

    pub(super) fn clear_errors(&self) {
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
        let help = widgets::button("Costs and data", &["quiet"]);
        help.set_halign(gtk::Align::Start);
        help.connect_clicked(costs_and_data);
        body.append(&help);
        if let Some(error) = self.agent_error.borrow().as_ref() {
            body.append(&widgets::inline_message(MessageKind::Error, error, None));
        }
        body.append(&session::model_picker(&self.controller));
        let start = objective_actions::activity_controls(detail)
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
        panel.append(&session::session_section(&self.controller, detail, &report));
        let resources = vbox(space::GROUP);
        resources.append(&widgets::section_title(session::RESOURCES_TITLE));
        resources.append(&session::resources_section(&self.controller));
        panel.append(&resources);
    }

    pub(super) fn render_agent(self: &Rc<Self>) -> bool {
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

    pub(super) fn rebuild_agent(self: &Rc<Self>) {
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

    fn relevant_control(detail: &ObjectiveDetail) -> Option<PanelAction> {
        let wanted: &[ActionName] = match detail.summary.execution.state {
            ExecutionState::Starting
            | ExecutionState::Running
            | ExecutionState::Idle
            | ExecutionState::Stopping => &[ActionName::ExecutionCancel],
            _ if detail.session.is_none() => &[],
            _ => &[ActionName::ExecutionResume, ActionName::ExecutionStart],
        };
        let offered: Vec<PanelAction> = objective_actions::activity_controls(detail)
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

    pub(super) fn render_state(self: &Rc<Self>) {
        let state = self.controller.store.state();
        let detail = state.selected_detail().cloned();
        let selected = state.ui.selected.is_some();
        drop(state);
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
}
