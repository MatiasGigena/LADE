//! The Feedback tab: review drafts anchored to the Change Set, and submitting them.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use crate::presentation::state::drafts::SaveStatus;
use crate::presentation::state::model::{ActionAvailability, ActionSurface, DeliveryMode, DraftId};
use crate::presentation::state::store::MainView;
use crate::presentation::text::bound::TEXT_LIMIT;
use crate::presentation::text::format;
use crate::views::dialogs;
use crate::views::widgets::{
    self, MessageKind, action_row, field, hbox, inline_message, label, set_accessible_description,
    set_accessible_label, set_focus_key, space, vbox,
};

use super::{Inspector, section};

const FEEDBACK_MODES: [DeliveryMode; 2] = [DeliveryMode::Steer, DeliveryMode::FollowUp];

pub(super) struct DraftWidgets {
    root: gtk::Box,
    reference: gtk::Label,
    excerpt: gtk::Label,
    stale: gtk::Box,
    view: gtk::TextView,
    input: Rc<widgets::LimitedInput>,
    status: gtk::Label,
    actions: gtk::Box,
}

impl Inspector {
    pub(super) fn reconcile_drafts(self: &Rc<Self>) {
        let state = self.controller.store.state();
        let Some(id) = state.ui.selected.clone() else {
            drop(state);
            widgets::clear(&self.drafts_box);
            widgets::clear(&self.drafts_heading);
            self.drafts.borrow_mut().clear();
            return;
        };
        let detail = state.detail(&id).cloned();
        let editors: Vec<_> = state.unsent_drafts(&id).cloned().collect();
        drop(state);
        widgets::clear(&self.drafts_heading);
        self.drafts_heading.append(&widgets::section_title(&format!(
            "Drafts ({})",
            editors.len()
        )));
        self.drafts_heading.append(&label(
            "Saved to the service; sent only when you submit them together.",
            &["meta"],
        ));
        if editors.is_empty() {
            let none = vbox(space::TIGHT);
            none.append(&label(
                "No unsent drafts. Accepted feedback and its delivery are listed in Activity.",
                &["meta"],
            ));
            let open = widgets::button("Open Activity", &["quiet", "list-action-button"]);
            open.set_halign(gtk::Align::Start);
            set_accessible_description(&open, "Accepted inputs and their delivery state");
            set_focus_key(&open, "feedback:open-activity");
            let controller = self.controller.clone();
            open.connect_clicked(move |_| controller.set_view(MainView::Activity));
            none.append(&open);
            self.drafts_heading.append(&none);
        }
        let mut widgets_map = self.drafts.borrow_mut();
        widgets_map.retain(|draft_id, w| {
            let keep = editors.iter().any(|e| &e.draft_id == draft_id);
            if !keep {
                self.drafts_box.remove(&w.root);
            }
            keep
        });
        let mut previous: Option<gtk::Widget> = None;
        for editor in &editors {
            let w = widgets_map
                .entry(editor.draft_id.clone())
                .or_insert_with(|| self.draft_widgets(&editor.draft_id));
            if w.root.parent().is_none() {
                self.drafts_box.append(&w.root);
            }
            self.drafts_box
                .reorder_child_after(&w.root, previous.as_ref());
            previous = Some(w.root.clone().upcast());
            w.reference
                .set_text(&format::reference_label(&editor.reference, detail.as_ref()));
            let excerpt = editor
                .reference
                .selected_text
                .lines()
                .take(3)
                .collect::<Vec<_>>()
                .join("\n");
            w.excerpt.set_text(&excerpt);
            if w.input.text() != editor.body {
                w.input.set_text(&editor.body);
            }
            set_accessible_label(
                &w.view,
                &format!(
                    "Feedback on {}",
                    format::reference_label(&editor.reference, detail.as_ref())
                ),
            );
        }
        drop(widgets_map);
        for editor in &editors {
            self.render_draft_status(&editor.draft_id);
        }
    }

    fn draft_widgets(self: &Rc<Self>, draft_id: &DraftId) -> DraftWidgets {
        let root = vbox(space::ITEM);
        root.add_css_class("draft");
        let reference = label("", &["meta", "draft-reference"]);
        root.append(&reference);
        let excerpt = label("", &["mono", "excerpt"]);
        excerpt.set_wrap_mode(gtk::pango::WrapMode::Char);
        root.append(&excerpt);
        let stale = vbox(0);
        root.append(&stale);
        let view = gtk::TextView::new();
        set_focus_key(&view, &format!("draft:{}", draft_id.as_str()));
        let input = widgets::LimitedInput::new(&view, TEXT_LIMIT, "this feedback draft");
        root.append(&input.field(&widgets::bounded_text(&view, 88, 200)));
        let status_row = vbox(space::TIGHT);
        let status = label("", &["meta", "save-state"]);
        status.set_hexpand(true);
        status_row.append(&status);
        let actions = hbox(space::ITEM);
        status_row.append(&actions);
        root.append(&status_row);
        let controller = self.controller.clone();
        let id = draft_id.clone();
        let this = Rc::downgrade(self);
        input.connect_commit(move |text| {
            controller.edit_draft(&id, text, widgets::monotonic_ms());
            if let Some(this) = this.upgrade() {
                let handler = this.on_draft_edit.borrow().clone();
                if let Some(handler) = handler {
                    handler();
                }
            }
        });
        DraftWidgets {
            root,
            reference,
            excerpt,
            stale,
            view,
            input,
            status,
            actions,
        }
    }

    pub(super) fn render_draft_status(self: &Rc<Self>, draft_id: &DraftId) {
        let map = self.drafts.borrow();
        let Some(w) = map.get(draft_id) else { return };
        let state = self.controller.store.state();
        let Some(editor) = state
            .drafts
            .values()
            .flatten()
            .find(|d| &d.draft_id == draft_id)
        else {
            return;
        };
        let status = editor.status(state.connection.is_connected());
        let is_stale = editor.stale;
        drop(state);
        let shown = w.stale.first_child().is_some();
        if shown != is_stale {
            widgets::clear(&w.stale);
            if is_stale {
                w.stale.append(&inline_message(
                    MessageKind::Warning,
                    "Stale reference — the inspected bytes changed. Re-anchor or remove this item; it blocks the whole batch.",
                    None,
                ));
            }
        }
        w.status.set_text(&status.label());
        let details = match &status {
            SaveStatus::Failed { details, .. } if !details.is_empty() => Some(details.as_str()),
            _ => None,
        };
        w.status.set_tooltip_text(details);
        set_accessible_description(&w.status, details.unwrap_or(""));
        for class in ["save-failed", "save-warning", "save-ok"] {
            w.status.remove_css_class(class);
        }
        w.status.add_css_class(match status {
            SaveStatus::Failed { .. } => "save-failed",
            SaveStatus::Disconnected | SaveStatus::Conflict { .. } => "save-warning",
            SaveStatus::Saved { .. } => "save-ok",
            _ => "save-neutral",
        });
        set_accessible_label(&w.status, &format!("Draft save state: {}", status.label()));
        widgets::clear(&w.actions);
        let id = draft_id.clone();
        match status {
            SaveStatus::Failed { .. } => {
                let retry = widgets::button("Retry", &["quiet"]);
                set_focus_key(&retry, &format!("retry:{}", id.as_str()));
                let this = Rc::downgrade(self);
                let rid = id.clone();
                retry.connect_clicked(move |_| {
                    if let Some(this) = this.upgrade() {
                        this.controller.retry_draft(&rid, widgets::monotonic_ms());
                        let handler = this.on_draft_edit.borrow().clone();
                        if let Some(handler) = handler {
                            handler();
                        }
                    }
                });
                w.actions.append(&retry);
            }
            SaveStatus::Conflict { .. } => {
                for (text, keep) in [("Keep mine", true), ("Use saved", false)] {
                    let b = widgets::button(text, &["quiet"]);
                    let this = Rc::downgrade(self);
                    let cid = id.clone();
                    b.connect_clicked(move |_| {
                        if let Some(this) = this.upgrade() {
                            this.controller.resolve_draft_conflict(
                                &cid,
                                keep,
                                widgets::monotonic_ms(),
                            );
                            let handler = this.on_draft_edit.borrow().clone();
                            if let Some(handler) = handler {
                                handler();
                            }
                        }
                    });
                    w.actions.append(&b);
                }
            }
            _ => {}
        }
        let delete = widgets::button("Remove draft…", &["quiet", "destructive-quiet"]);
        delete.set_tooltip_text(Some("Delete this draft from the service"));
        set_focus_key(&delete, &format!("remove:{}", id.as_str()));
        let controller = self.controller.clone();
        let reference = w.reference.text().to_string();
        delete.connect_clicked(move |b| {
            let controller = controller.clone();
            let id = id.clone();
            dialogs::confirm(
                b,
                "Remove draft",
                &format!(
                    "The draft on {reference} and its text are deleted. This cannot be undone; nothing is sent to the agent."
                ),
                "Remove draft",
                true,
                move || controller.delete_draft(&id),
            );
        });
        w.actions.append(&delete);
    }

    pub(super) fn render_submit(self: &Rc<Self>) {
        widgets::clear(&self.submit_box);
        let refusal = self
            .controller
            .store
            .state()
            .ui
            .selected
            .clone()
            .and_then(|o| self.refusal(&o, ActionSurface::Feedback));
        let Some(batch) = self
            .controller
            .feedback_batch()
            .filter(|b| !b.items.is_empty())
        else {
            if let Some(refusal) = refusal {
                self.submit_box.append(&refusal);
            }
            return;
        };
        let s = section("Submit feedback");
        let mode = widgets::dropdown_or(
            &FEEDBACK_MODES.map(format::mode_label),
            "No delivery mode available",
        );
        set_accessible_label(&mode, "Delivery mode");
        set_focus_key(&mode, "submit:mode");
        if let Some(index) = FEEDBACK_MODES
            .iter()
            .position(|m| *m == batch.mode)
            .and_then(|i| u32::try_from(i).ok())
        {
            mode.set_selected(index);
        }
        let controller = self.controller.clone();
        mode.connect_selected_notify(move |d| {
            if let Some(m) = FEEDBACK_MODES.get(d.selected() as usize) {
                controller.set_feedback_mode(*m);
            }
        });
        let explanation = format::mode_explanation(batch.mode);
        mode.set_tooltip_text(Some(explanation));
        set_accessible_description(&mode, explanation);
        s.append(&field("Delivery", &mode));
        let destination = batch.destination.as_ref().map_or_else(
            || "No Agent Session".to_owned(),
            |d| {
                format!(
                    "Session {} · {} · gen {}",
                    d.session_id.short(),
                    d.model.label(),
                    d.execution_generation
                )
            },
        );
        s.append(&field("Destination", &label(&destination, &[])));
        for blocker in &batch.blockers {
            s.append(&inline_message(MessageKind::Warning, blocker, None));
        }
        let availability = if batch.blockers.is_empty() {
            ActionAvailability::Enabled
        } else {
            ActionAvailability::Disabled {
                reason: "Resolve the items above before submitting".to_owned(),
            }
        };
        let count = batch.items.len();
        let this = Rc::downgrade(self);
        s.append(&action_row(
            &format!(
                "Review and submit {count} item{}…",
                if count == 1 { "" } else { "s" }
            ),
            &availability,
            &["primary"],
            move |b| {
                if let Some(this) = this.upgrade()
                    && let Some(batch) = this.controller.feedback_batch()
                {
                    dialogs::submit_feedback(b, &this.controller, batch);
                }
            },
        ));
        if let Some(refusal) = refusal {
            s.append(&refusal);
        }
        self.submit_box.append(&s);
    }

    pub fn focus_draft(&self, draft_id: &DraftId) {
        self.root.set_current_page(Some(2));
        let Some((view, draft)) = self
            .drafts
            .borrow()
            .get(draft_id)
            .map(|w| (w.view.clone(), w.root.clone()))
        else {
            return;
        };
        view.grab_focus();
        let viewport = self
            .root
            .nth_page(Some(2))
            .and_downcast::<gtk::ScrolledWindow>()
            .and_then(|s| s.child())
            .and_downcast::<gtk::Viewport>();
        let Some(viewport) = viewport else { return };
        let allocated = Cell::new(false);
        view.add_tick_callback(move |view, _| {
            if !allocated.replace(true) {
                return gtk::glib::ControlFlow::Continue;
            }
            if view.has_focus() {
                viewport.scroll_to(&draft, None);
            }
            gtk::glib::ControlFlow::Break
        });
    }
}
