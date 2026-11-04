//! The Overview tab: the objective summary, its next step and its other actions.

use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use crate::presentation::panel::{self, PanelAction, Tone};
use crate::presentation::state::model::{
    ActionAvailability, ActionName, ActionSurface, ObjectiveDetail, Timestamp,
};
use crate::presentation::state::store::MainView;
use crate::presentation::text::format;
use crate::views::dialogs;
use crate::views::widgets::{
    self, MessageKind, action_row, field, inline_message, label, set_accessible_description,
    set_focus_key, space, vbox,
};

use super::{Inspector, group_separator, section};

impl Inspector {
    pub(super) fn render_overview(self: &Rc<Self>) {
        widgets::clear(&self.overview);
        let selected = self.controller.store.state().ui.selected.is_some();
        self.root.set_show_tabs(selected);
        let Some(detail) = self.detail() else {
            if selected {
                self.overview
                    .append(&label("Loading Objective…", &["meta"]));
            }
            return;
        };
        let summary = &detail.summary;

        let next_area = vbox(space::ITEM);
        next_area.add_css_class("next-action");
        if let Some(approval) = &detail.approval {
            next_area.append(&label(format::approval_label(approval.state), &["meta"]));
        }
        let blocker =
            panel::start_blocker(self.controller.store.state().agent_session.catalog.as_ref());
        let pointer = panel::agent_pointer(&detail, blocker.as_deref());
        if let Some(pointer) = &pointer {
            let row = vbox(space::TIGHT);
            row.append(&label(pointer, &["meta"]));
            let open = widgets::button("Open Activity", &["quiet", "list-action-button"]);
            open.set_halign(gtk::Align::Start);
            set_accessible_description(&open, pointer);
            set_focus_key(&open, "overview:open-activity");
            let controller = self.controller.clone();
            open.connect_clicked(move |_| controller.set_view(MainView::Activity));
            row.append(&open);
            next_area.append(&row);
        }
        let next = panel::next_action(&detail);
        match &next {
            Some(next) => next_area.append(&self.action_widget(&detail, next, true)),
            None if !summary.lifecycle.is_archived() && pointer.is_none() => {
                next_area.append(&label("No step is available right now.", &["meta"]))
            }
            None => {}
        }
        if let Some(error) = self.last_error.borrow().as_ref() {
            next_area.append(&inline_message(MessageKind::Error, error, None));
        }
        for surface in [ActionSurface::Review, ActionSurface::Closing] {
            if let Some(refusal) = self.refusal(&summary.objective_id, surface) {
                next_area.append(&refusal);
            }
        }
        if let Some(note) = self.command_note(&summary.objective_id) {
            next_area.append(&note);
        }
        if next_area.first_child().is_some() {
            self.overview.append(&next_area);
        }

        let (regular, destructive) = panel::more_actions(&detail, next.as_ref());
        if !regular.is_empty() || !destructive.is_empty() || detail.history_deleted {
            let list = vbox(space::TIGHT);
            for action in &regular {
                list.append(&self.action_widget(&detail, action, false));
            }
            if !destructive.is_empty() || detail.history_deleted {
                if !regular.is_empty() {
                    list.append(&group_separator());
                }
                for action in &destructive {
                    list.append(&self.action_widget(&detail, action, false));
                }
                if detail.history_deleted {
                    list.append(&label(
                        "Git history deleted: retention references and branch were removed on request.",
                        &["meta"],
                    ));
                }
            }
            let this = Rc::downgrade(self);
            self.overview.append(&widgets::disclosure(
                panel::MORE_ACTIONS_TITLE,
                self.more_open.get(),
                &list,
                move |open| {
                    if let Some(this) = this.upgrade() {
                        this.more_open.set(open);
                    }
                },
            ));
        }

        let intent = section("Intent");
        for (key, value) in [
            ("Outcome", &detail.intent.outcome),
            ("Constraints", &detail.intent.constraints),
            ("Acceptance", &detail.intent.acceptance),
        ] {
            let block = vbox(space::TIGHT);
            block.append(&label(key, &["meta"]));
            block.append(&label(
                if value.is_empty() { "—" } else { value },
                &["prose"],
            ));
            intent.append(&block);
        }
        intent.append(&label(
            &format!("Intent revision {}", detail.intent_revision),
            &["meta"],
        ));
        let this = Rc::downgrade(self);
        intent.append(&widgets::list_action_row(
            "Edit intent",
            &detail.availability(ActionName::ObjectiveUpdateIntent),
            false,
            move |b| {
                if let Some(this) = this.upgrade() {
                    dialogs::edit_intent(b, &this.controller);
                }
            },
        ));
        if let Some(refusal) = self.refusal(&summary.objective_id, ActionSurface::Intent) {
            intent.append(&refusal);
        }
        self.overview.append(&intent);

        let rows = vbox(space::ITEM);
        for row in panel::details_rows(&detail, Timestamp(widgets::now_ms())) {
            let value = if row.mono {
                widgets::line_label(&row.value, &["mono"], gtk::pango::EllipsizeMode::Middle)
            } else {
                label(&row.value, &[])
            };
            if let Some(full) = &row.full {
                value.set_tooltip_text(Some(full));
                set_accessible_description(&value, full);
            }
            rows.append(&field(row.key, &value));
        }
        let this = Rc::downgrade(self);
        self.overview.append(&widgets::disclosure(
            panel::DETAILS_TITLE,
            self.details_open.get(),
            &rows,
            move |open| {
                if let Some(this) = this.upgrade() {
                    this.details_open.set(open);
                }
            },
        ));
    }

    fn action_widget(
        self: &Rc<Self>,
        detail: &ObjectiveDetail,
        action: &PanelAction,
        prominent: bool,
    ) -> gtk::Box {
        let this = Rc::downgrade(self);
        let name = action.action;
        let on_click = move |b: &gtk::Button| {
            if let Some(this) = this.upgrade() {
                this.activate(name, b);
            }
        };
        let destructive = action.tone == Tone::Destructive;
        let enabled = action.availability == ActionAvailability::Enabled;
        let note = match name {
            ActionName::ReviewApprove if enabled && self.controller.approval_needs_override() => {
                Some(action.note.clone().unwrap_or_else(|| {
                    "Checks missing or not all passed: approval needs a recorded override reason."
                        .to_owned()
                }))
            }
            _ => action.note.clone(),
        }
        .filter(|_| enabled);
        let row = if prominent {
            let classes: &[&str] = match action.tone {
                Tone::Primary if note.is_none() => &["primary"],
                Tone::Destructive => &["destructive"],
                Tone::Primary | Tone::Neutral => &[],
            };
            action_row(&action.label, &action.availability, classes, on_click)
        } else {
            widgets::list_action_row(&action.label, &action.availability, destructive, on_click)
        };
        if let Some(note) = note {
            row.append(&label(&note, &["meta", "list-action-reason"]));
        }
        if !prominent {
            return row;
        }
        match name {
            ActionName::ReviewIntegrate => row.append(&label(
                &format!(
                    "One squash commit on {} from the approved candidate and target commit — a separate, explicit step.",
                    detail.integration_target.branch
                ),
                &["meta"],
            )),
            ActionName::ObjectiveCleanup => row.append(&label(
                "Removes the Objective's branch and worktree; records and evidence stay.",
                &["meta"],
            )),
            _ => {}
        }
        row
    }

    fn activate(self: &Rc<Self>, action: ActionName, button: &gtk::Button) {
        let Some(detail) = self.detail() else { return };
        let controller = self.controller.clone();
        match action {
            ActionName::ReviewFreeze => self.report(controller.freeze()),
            ActionName::ReviewRefresh => self.report(controller.refresh_candidate()),
            ActionName::ReviewReturn => dialogs::confirm(
                button,
                "Return for revision",
                "The current candidate becomes historical and any approval or override is invalidated before the agent may change the work again.",
                "Return for revision",
                false,
                move || controller.return_for_revision(),
            ),
            ActionName::ReviewApprove => {
                if controller.approval_needs_override() {
                    dialogs::approve_with_override(button, &controller);
                } else {
                    self.report(controller.approve(None));
                }
            }
            ActionName::ReviewIntegrate => {
                let target = detail.integration_target.label();
                dialogs::confirm(
                    button,
                    "Integrate approved candidate",
                    &format!(
                        "Create a squash commit on {target} from the approved candidate. The target must be unchanged and its checkout clean."
                    ),
                    "Integrate",
                    false,
                    move || controller.integrate(),
                );
            }
            ActionName::ObjectiveComplete => dialogs::confirm(
                button,
                "Complete without changes",
                "The Objective is completed with no repository change.",
                "Complete",
                false,
                move || controller.complete(),
            ),
            ActionName::ObjectiveDiscard => {
                let title = detail.summary.title.clone();
                dialogs::confirm(
                    button,
                    "Discard Objective",
                    &format!(
                        "“{title}” will be abandoned without integration and its agent stopped. Branch and worktree stay until you clean up."
                    ),
                    "Discard",
                    true,
                    move || controller.discard(),
                );
            }
            ActionName::ObjectiveCleanup => {
                dialogs::cleanup(button, &controller, &detail.branch, detail.unsent_drafts);
            }
            ActionName::ObjectiveDeleteHistory => {
                dialogs::delete_history(button, &controller, &detail.summary.title);
            }
            ActionName::ObjectiveUpdateIntent => dialogs::edit_intent(button, &controller),
            ActionName::ExecutionStart
            | ActionName::ExecutionCancel
            | ActionName::ExecutionResume
            | ActionName::MessageSend
            | ActionName::FeedbackSubmit
            | ActionName::ReviewVerify => {}
        }
    }
}
