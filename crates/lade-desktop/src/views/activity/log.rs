//! The activity log and the accepted inputs list.

use gtk::prelude::*;
use gtk4 as gtk;

use crate::presentation::state::model::{
    ActivityItem, ActivityRole, DeliveryState, ExecutionState, Timestamp,
};
use crate::presentation::text::format;
use crate::views::widgets::{self, hbox, label, set_accessible_label, space, vbox};
use crate::views::workbench;

use super::{Activity, INPUTS_TITLE, fit};

impl Activity {
    pub(super) fn render_inputs(&self) {
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

    pub(super) fn sync_body(&self) {
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
        text.set_natural_wrap_mode(gtk::NaturalWrapMode::None);
        if item.role == ActivityRole::User {
            text.set_max_width_chars(64);
            content.add_css_class("bubble");
            content.set_halign(gtk::Align::End);
        } else if item.role != ActivityRole::Tool {
            text.set_max_width_chars(88);
            text.set_halign(gtk::Align::Start);
        }
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

    pub(super) fn render_items(&self) {
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

    pub(super) fn append_items(&self, from_index: usize) {
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

    pub(super) fn apply_delta(&self, index: usize) {
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
