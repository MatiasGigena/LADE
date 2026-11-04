//! Drafts: unsent feedback comments with optimistic revisions, bound to the session they will reach.

use crate::domain::ids::{DraftId, InputId, ObjectiveId, Revision, SessionId, Timestamp};
use crate::domain::inspection::{CurrentContent, MAX_TEXT_BYTES, Reference};
use crate::domain::objectives::{CLOSED, Objective};
use crate::error::DomainError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftState {
    Unsent,
    Submitted { input_id: InputId },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftRecord {
    pub id: DraftId,
    pub objective_id: ObjectiveId,
    pub destination_session_id: SessionId,
    pub reference: Reference,
    pub body: String,
    pub revision: Revision,
    pub state: DraftState,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    record: DraftRecord,
}

#[derive(Clone, Debug)]
pub struct SaveDraft {
    pub draft_id: DraftId,
    pub objective_id: ObjectiveId,
    pub expected_revision: Revision,
    pub reference: Reference,
    pub body: String,
}

fn check_body(body: &str) -> Result<(), DomainError> {
    if body.len() > MAX_TEXT_BYTES {
        return Err(DomainError::LimitReached("The draft is too long."));
    }
    Ok(())
}

/// Creates a draft (expected revision 0) or updates `existing` under optimistic concurrency.
///
/// # Errors
///
/// - `DomainError::InvalidRequest` when the request names another Objective.
/// - `DomainError::InvalidLifecycle` when creating on a closed Objective, or updating a submitted
///   draft.
/// - `DomainError::ExecutionUnavailable` when creating with no session to address.
/// - `DomainError::NotFound` when updating a draft that does not exist or belongs elsewhere.
/// - `DomainError::Conflict` when creating over an existing draft or the revision has moved on.
/// - `DomainError::LimitReached` when the body is too long or the revision is exhausted.
pub fn save_draft(
    existing: Option<Draft>,
    objective: &Objective,
    request: SaveDraft,
    now: Timestamp,
) -> Result<Draft, DomainError> {
    if request.objective_id != objective.id() {
        return Err(DomainError::InvalidRequest("draft objective mismatch"));
    }
    match existing {
        None if request.expected_revision == Revision::new(0) => {
            if objective.lifecycle().is_terminal() {
                return Err(DomainError::InvalidLifecycle(CLOSED));
            }
            let Some(session) = objective.execution().session() else {
                return Err(DomainError::ExecutionUnavailable);
            };
            check_body(&request.body)?;
            Ok(Draft {
                record: DraftRecord {
                    id: request.draft_id,
                    objective_id: request.objective_id,
                    destination_session_id: session.session_id,
                    reference: request.reference,
                    body: request.body,
                    revision: Revision::INITIAL,
                    state: DraftState::Unsent,
                    created_at: now,
                    updated_at: now,
                },
            })
        }
        None => Err(DomainError::NotFound("draft")),
        Some(draft)
            if draft.record.id != request.draft_id
                || draft.record.objective_id != request.objective_id =>
        {
            Err(DomainError::NotFound("draft"))
        }
        Some(_) if request.expected_revision == Revision::new(0) => {
            Err(DomainError::Conflict("This draft already exists."))
        }
        Some(mut draft) => {
            draft.update(
                request.expected_revision,
                request.reference,
                request.body,
                now,
            )?;
            Ok(draft)
        }
    }
}

impl Draft {
    /// Rebuilds a draft from its stored record.
    ///
    /// # Errors
    ///
    /// `DomainError::DurabilityFailure` when the record has revision 0 or an oversized body.
    pub fn restore(record: DraftRecord) -> Result<Self, DomainError> {
        if record.revision.value() == 0 || record.body.len() > MAX_TEXT_BYTES {
            return Err(DomainError::DurabilityFailure("inconsistent stored draft"));
        }
        Ok(Self { record })
    }

    #[must_use]
    pub const fn record(&self) -> &DraftRecord {
        &self.record
    }

    #[must_use]
    pub fn into_record(self) -> DraftRecord {
        self.record
    }

    #[must_use]
    pub const fn id(&self) -> DraftId {
        self.record.id
    }

    #[must_use]
    pub const fn revision(&self) -> Revision {
        self.record.revision
    }

    #[must_use]
    pub const fn is_unsent(&self) -> bool {
        matches!(self.record.state, DraftState::Unsent)
    }

    fn check_editable(&self, expected_revision: Revision) -> Result<(), DomainError> {
        if !self.is_unsent() {
            return Err(DomainError::InvalidLifecycle(
                "This draft was already submitted and can't be changed.",
            ));
        }
        if expected_revision != self.record.revision {
            return Err(DomainError::Conflict(
                "The draft was changed elsewhere. Refresh and try again.",
            ));
        }
        Ok(())
    }

    /// Replaces the reference and body of an unsent draft, returning its new revision.
    ///
    /// # Errors
    ///
    /// - `DomainError::InvalidLifecycle` when the draft was submitted.
    /// - `DomainError::Conflict` when `expected_revision` is not current.
    /// - `DomainError::LimitReached` when the body is too long or the revision is exhausted.
    pub fn update(
        &mut self,
        expected_revision: Revision,
        reference: Reference,
        body: String,
        now: Timestamp,
    ) -> Result<Revision, DomainError> {
        self.check_editable(expected_revision)?;
        check_body(&body)?;
        let revision = self.record.revision.advance()?;
        self.record.reference = reference;
        self.record.body = body;
        self.record.revision = revision;
        self.record.updated_at = now;
        Ok(revision)
    }

    /// Readdresses an unsent draft to `session_id`, returning its new revision.
    ///
    /// # Errors
    ///
    /// - `DomainError::InvalidLifecycle` when the draft was submitted.
    /// - `DomainError::Conflict` when `expected_revision` is not current.
    /// - `DomainError::LimitReached` when the revision is exhausted.
    pub fn retarget(
        &mut self,
        expected_revision: Revision,
        session_id: SessionId,
        now: Timestamp,
    ) -> Result<Revision, DomainError> {
        self.check_editable(expected_revision)?;
        let revision = self.record.revision.advance()?;
        self.record.destination_session_id = session_id;
        self.record.revision = revision;
        self.record.updated_at = now;
        Ok(revision)
    }

    pub(super) fn mark_submitted(&mut self, input_id: InputId, now: Timestamp) {
        self.record.state = DraftState::Submitted { input_id };
        self.record.updated_at = now;
    }

    /// Checks that the draft may be deleted at `expected_revision`.
    ///
    /// # Errors
    ///
    /// - `DomainError::InvalidLifecycle` when the draft was submitted.
    /// - `DomainError::Conflict` when `expected_revision` is not current.
    pub fn check_delete(&self, expected_revision: Revision) -> Result<(), DomainError> {
        self.check_editable(expected_revision)
    }

    #[must_use]
    pub fn is_stale(&self, current: CurrentContent<'_>) -> bool {
        self.record.reference.staleness(current).is_stale()
    }
}

#[cfg(test)]
mod tests;
