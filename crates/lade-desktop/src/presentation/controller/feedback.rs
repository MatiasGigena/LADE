//! Review feedback drafts: anchoring, editing and saving them, and submitting a batch.

use super::intents::{DraftRevisionRef, Intent};
use crate::presentation::layout::{LayoutClass, Region};
use crate::presentation::state::drafts::{DraftEditor, Millis, SaveStatus};
use crate::presentation::state::model::{
    ActionAvailability, ActionName, DeliveryMode, DraftId, SessionInfo,
};
use crate::presentation::state::store::{Change, InspectorTab};
use crate::presentation::text::format;
use crate::presentation::text::map::SelectionError;

use super::Controller;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchItem {
    pub draft_id: DraftId,
    pub revision: u64,
    pub reference_label: String,
    pub body: String,
    pub stale: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedbackBatch {
    pub items: Vec<BatchItem>,
    pub deferred: usize,
    pub destination: Option<SessionInfo>,
    pub mode: DeliveryMode,
    pub blockers: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsavedDraft {
    pub draft_id: DraftId,
    pub objective: String,
    pub reference_label: String,
    pub status: String,
    pub pending: bool,
}

pub const MAX_FEEDBACK_BATCH: usize = 256;

impl Controller {
    pub fn add_feedback(&self, draft_id: DraftId) -> Result<DraftId, String> {
        let id = self.selected().ok_or("Select an Objective first")?;
        let (reference, session) = {
            let state = self.store.state();
            let reference = match state.selected_ui().and_then(|u| u.selection.clone()) {
                Some(Ok(reference)) => reference,
                Some(Err(reason)) => return Err(reason),
                None => return Err(SelectionError::Empty.message().to_owned()),
            };
            let session = state
                .selected_detail()
                .and_then(|d| d.session.clone())
                .ok_or("No Agent Session to address — start the agent first")?;
            (reference, session)
        };
        self.store.update(|s| {
            s.drafts.entry(id.clone()).or_default().insert(
                0,
                DraftEditor::new_local(draft_id.clone(), id.clone(), session.session_id, reference),
            );
            s.ui.inspector_tab = InspectorTab::Feedback;
            let mut changes = vec![Change::Drafts(id.clone()), Change::InspectorTab];
            if s.ui.layout == LayoutClass::Compact && s.ui.compact_region != Region::Inspector {
                s.ui.compact_region = Region::Inspector;
                changes.push(Change::Layout);
            }
            ((), changes)
        });
        Ok(draft_id)
    }

    pub fn edit_draft(&self, draft_id: &DraftId, body: &str, now: Millis) {
        self.store.update(|s| match s.draft_mut(draft_id) {
            Some(editor) => {
                editor.edit(body, now);
                ((), vec![Change::DraftStatus(draft_id.clone())])
            }
            None => ((), Vec::new()),
        });
    }

    pub fn poll_drafts(&self, now: Millis) -> Option<Millis> {
        let (requests, next) = self.store.update(|s| {
            let connected = s.connection.is_connected();
            let mut requests = Vec::new();
            let mut changes = Vec::new();
            let mut next: Option<Millis> = None;
            for editor in s.drafts.values_mut().flatten() {
                if let Some(request) = editor.poll(now, connected) {
                    changes.push(Change::DraftStatus(editor.draft_id.clone()));
                    requests.push(request);
                }
                if let Some(deadline) = editor.next_deadline() {
                    next = Some(next.map_or(deadline, |n| n.min(deadline)));
                }
            }
            ((requests, next), changes)
        });
        for r in requests {
            self.emit(Intent::SaveDraft {
                objective_id: r.objective_id,
                draft_id: r.draft_id,
                expected_revision: r.expected_revision,
                reference: r.reference,
                body: r.body,
            });
        }
        next
    }

    pub fn unsaved_drafts(&self) -> Vec<UnsavedDraft> {
        let state = self.store.state();
        let connected = state.connection.is_connected();
        let mut unsaved: Vec<UnsavedDraft> = state
            .drafts
            .values()
            .flatten()
            .filter(|d| d.has_unsaved_text())
            .map(|d| {
                let status = d.status(connected);
                UnsavedDraft {
                    draft_id: d.draft_id.clone(),
                    objective: state
                        .objective(&d.objective_id)
                        .map_or_else(|| d.objective_id.short().to_owned(), |o| o.title.clone()),
                    reference_label: format::reference_label(
                        &d.reference,
                        state.detail(&d.objective_id),
                    ),
                    pending: matches!(status, SaveStatus::Unsaved | SaveStatus::Saving),
                    status: status.label(),
                }
            })
            .collect();
        unsaved.sort_by(|a, b| a.draft_id.as_str().cmp(b.draft_id.as_str()));
        unsaved
    }

    pub fn flush_drafts(&self, now: Millis) -> Option<Millis> {
        self.store.update(|s| {
            for editor in s.drafts.values_mut().flatten() {
                editor.flush(now);
            }
            ((), Vec::new())
        });
        self.poll_drafts(now)
    }

    pub fn retry_draft(&self, draft_id: &DraftId, now: Millis) {
        self.store.update(|s| {
            if let Some(editor) = s.draft_mut(draft_id) {
                editor.retry(now);
            }
            ((), vec![Change::DraftStatus(draft_id.clone())])
        });
    }

    pub fn resolve_draft_conflict(&self, draft_id: &DraftId, keep_local: bool, now: Millis) {
        self.store.update(|s| {
            let changes = s
                .draft_mut(draft_id)
                .map(|editor| {
                    editor.resolve_conflict(keep_local, now);
                    Change::Drafts(editor.objective_id.clone())
                })
                .into_iter()
                .collect();
            ((), changes)
        });
    }

    pub fn delete_draft(&self, draft_id: &DraftId) -> Result<(), String> {
        let durable = self.store.update(|s| {
            let revision = match s
                .draft_mut(draft_id)
                .map(|e| e.removal_revision())
                .transpose()
            {
                Ok(revision) => revision,
                Err(reason) => return (Err(reason), Vec::new()),
            };
            match revision {
                Some(0) => (Ok(None), s.remove_draft(draft_id)),
                other => (Ok(other), Vec::new()),
            }
        })?;
        if let Some(expected_revision) = durable {
            self.emit(Intent::DeleteDraft {
                draft_id: draft_id.clone(),
                expected_revision,
            });
        }
        Ok(())
    }

    pub fn set_feedback_mode(&self, mode: DeliveryMode) {
        let Some(id) = self.selected() else { return };
        self.store.update(|s| {
            s.objective_ui_mut(&id).feedback_mode = mode;
            ((), vec![Change::Drafts(id.clone())])
        });
    }

    pub fn feedback_batch(&self) -> Option<FeedbackBatch> {
        let state = self.store.state();
        let id = state.ui.selected.as_ref()?;
        let detail = state.detail(id);
        let mode = state
            .ui
            .per_objective
            .get(id)
            .map_or(DeliveryMode::Steer, |u| u.feedback_mode);
        let connected = state.connection.is_connected();
        let mut items: Vec<BatchItem> = state
            .unsent_drafts(id)
            .filter(|d| d.durable_revision() > 0 || !d.body.is_empty())
            .map(|d| BatchItem {
                draft_id: d.draft_id.clone(),
                revision: d.durable_revision(),
                reference_label: format::reference_label(&d.reference, detail),
                body: d.body.clone(),
                stale: d.stale,
            })
            .collect();
        let deferred = items.len().saturating_sub(MAX_FEEDBACK_BATCH);
        items.truncate(MAX_FEEDBACK_BATCH);
        let destination = detail.and_then(|d| d.session.clone());
        let mut blockers = Vec::new();
        let explained = !connected || destination.is_none() || items.is_empty();
        if !connected {
            blockers.push("Not connected to the service".to_owned());
        }
        if destination.is_none() {
            blockers.push(
                "There is no Agent Session to send feedback to — start the agent in Activity first"
                    .to_owned(),
            );
        }
        if items.is_empty() {
            blockers.push("No drafts to submit".to_owned());
        }
        if !explained {
            match detail.map(|d| d.availability(ActionName::FeedbackSubmit)) {
                Some(ActionAvailability::Enabled) => {}
                Some(ActionAvailability::Disabled { reason }) => blockers.push(reason),
                Some(ActionAvailability::NotOffered) | None => blockers
                    .push("Submitting feedback is not offered for this Objective".to_owned()),
            }
        }
        let stale: Vec<&BatchItem> = items.iter().filter(|i| i.stale).collect();
        if !stale.is_empty() {
            let names: Vec<String> = stale
                .iter()
                .map(|i| format!("{} (draft {})", i.reference_label, i.draft_id.short()))
                .collect();
            blockers.push(format!(
                "{} stale item{} block{} the whole batch: {} — re-anchor or remove {}",
                stale.len(),
                if stale.len() == 1 { "" } else { "s" },
                if stale.len() == 1 { "s" } else { "" },
                names.join("; "),
                if stale.len() == 1 { "it" } else { "them" },
            ));
        }
        if state
            .unsent_drafts(id)
            .any(|d| !d.is_saved() && !d.body.is_empty())
        {
            blockers.push("Wait until every draft is saved".to_owned());
        }
        Some(FeedbackBatch {
            items,
            deferred,
            destination,
            mode,
            blockers,
        })
    }

    pub fn submit_feedback(&self, batch: &FeedbackBatch) -> Result<(), String> {
        if let Some(blocker) = batch.blockers.first() {
            return Err(blocker.clone());
        }
        let id = self.selected().ok_or("Select an Objective first")?;
        let destination = batch
            .destination
            .clone()
            .ok_or("No destination Agent Session")?;
        self.emit(Intent::SubmitFeedback {
            objective_id: id,
            destination_session_id: destination.session_id,
            association_revision: destination.association_revision,
            drafts: batch
                .items
                .iter()
                .map(|i| DraftRevisionRef {
                    draft_id: i.draft_id.clone(),
                    revision: i.revision,
                })
                .collect(),
            mode: batch.mode,
            note: None,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests;
