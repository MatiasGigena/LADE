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
mod tests {
    use super::*;
    use crate::domain::feedback::fixtures::{
        FILE, acceptance, present, reference_for, save_new, save_request,
    };
    use crate::domain::feedback::{BatchEntry, accept_feedback};
    use crate::domain::objectives::Lifecycle;
    use crate::testing::{
        active_objective, defined_objective, digest, draft_id, input_id, objective_id, session_id,
        stopped_objective, ts,
    };

    #[test]
    fn draft_create_binds_current_destination() {
        let (objective, _) = active_objective();
        let draft = save_new(&objective, 1, "\tlet é", "rename");
        assert_eq!(draft.revision(), Revision::INITIAL);
        assert_eq!(draft.record().destination_session_id, session_id(1));
        let without_session = defined_objective();
        assert_eq!(
            save_draft(
                None,
                &without_session,
                save_request(&without_session, 2, 0, ""),
                ts(3)
            ),
            Err(DomainError::ExecutionUnavailable)
        );
    }

    #[test]
    fn draft_cas_save_conflicts_and_never_wraps() {
        let (objective, _) = active_objective();
        let draft = save_new(&objective, 1, "\tlet é", "first");
        let update = |expected: u64, body: &str| save_request(&objective, 1, expected, body);
        let saved = save_draft(Some(draft.clone()), &objective, update(1, "second"), ts(4))
            .expect("cas ok");
        assert_eq!(saved.revision(), Revision::new(2));
        assert_eq!(saved.record().body, "second");
        assert_eq!(
            save_draft(
                Some(saved.clone()),
                &objective,
                update(1, "lost update"),
                ts(5)
            ),
            Err(DomainError::Conflict(
                "The draft was changed elsewhere. Refresh and try again."
            ))
        );
        assert_eq!(
            save_draft(Some(saved.clone()), &objective, update(0, "dup"), ts(5)),
            Err(DomainError::Conflict("This draft already exists."))
        );
        assert_eq!(
            save_draft(None, &objective, update(3, "missing"), ts(5)),
            Err(DomainError::NotFound("draft"))
        );

        let mut record = saved.into_record();
        record.revision = Revision::new(u64::MAX);
        let mut exhausted = Draft::restore(record).expect("restore");
        let before = exhausted.clone();
        assert!(matches!(
            exhausted.update(
                Revision::new(u64::MAX),
                reference_for("\tlet é"),
                "x".into(),
                ts(6)
            ),
            Err(DomainError::LimitReached(_))
        ));
        assert_eq!(exhausted, before);
    }

    #[test]
    fn submitted_drafts_cannot_be_edited_or_deleted() {
        let (objective, _) = active_objective();
        let mut draft = save_new(&objective, 1, "\tlet é", "fix");
        let current_digest = digest(1);
        let mut batch = [BatchEntry {
            draft: &mut draft,
            revision: Revision::INITIAL,
            current: present(&current_digest, b""),
        }];
        accept_feedback(&objective, acceptance(&objective), &mut batch, None, ts(5))
            .expect("accepted");
        assert_eq!(
            draft.record().state,
            DraftState::Submitted {
                input_id: input_id(1)
            }
        );
        assert!(matches!(
            draft.check_delete(Revision::INITIAL),
            Err(DomainError::InvalidLifecycle(_))
        ));
        assert!(
            draft
                .update(
                    Revision::INITIAL,
                    reference_for("\tlet é"),
                    "x".into(),
                    ts(6)
                )
                .is_err()
        );
    }

    #[test]
    fn saving_checks_the_objective_and_draft_identity_before_anything_else() {
        let (objective, _) = active_objective();
        let draft = save_new(&objective, 1, "\tlet é", "first");
        let mut foreign = save_request(&objective, 1, 1, "x");
        foreign.objective_id = objective_id(9);
        assert_eq!(
            save_draft(Some(draft.clone()), &objective, foreign, ts(4)),
            Err(DomainError::InvalidRequest("draft objective mismatch"))
        );
        assert_eq!(
            save_draft(
                Some(draft.clone()),
                &objective,
                save_request(&objective, 2, 1, "x"),
                ts(4)
            ),
            Err(DomainError::NotFound("draft")),
            "the stored draft must be the one named"
        );
        let mut record = draft.into_record();
        record.objective_id = objective_id(9);
        let elsewhere = Draft::restore(record).expect("restore");
        assert_eq!(
            save_draft(
                Some(elsewhere),
                &objective,
                save_request(&objective, 1, 1, "x"),
                ts(4)
            ),
            Err(DomainError::NotFound("draft"))
        );
    }

    #[test]
    fn new_drafts_need_an_open_objective_and_a_bounded_body() {
        let mut closed = defined_objective();
        closed
            .transition(&[Lifecycle::Discarded], ts(2))
            .expect("discard");
        assert!(matches!(
            save_draft(None, &closed, save_request(&closed, 1, 0, "x"), ts(3)),
            Err(DomainError::InvalidLifecycle(_))
        ));
        let stopped = stopped_objective();
        let at_limit = "x".repeat(MAX_TEXT_BYTES);
        let draft = save_draft(
            None,
            &stopped,
            save_request(&stopped, 1, 0, &at_limit),
            ts(3),
        )
        .expect("a stopped agent still has a session to address");
        assert_eq!(draft.record().body.len(), MAX_TEXT_BYTES);
        assert_eq!(draft.record().created_at, ts(3));
        let over = "x".repeat(MAX_TEXT_BYTES + 1);
        assert_eq!(
            save_draft(None, &stopped, save_request(&stopped, 2, 0, &over), ts(3)),
            Err(DomainError::LimitReached("The draft is too long."))
        );
        let mut draft = draft;
        let before = draft.clone();
        assert!(
            draft
                .update(Revision::INITIAL, reference_for("fn main()"), over, ts(4))
                .is_err()
        );
        assert_eq!(draft, before, "a refused update changes nothing");
    }

    #[test]
    fn updates_replace_the_anchor_and_body_and_stamp_the_time() {
        let (objective, _) = active_objective();
        let mut draft = save_new(&objective, 1, "\tlet é", "first");
        let anchor = reference_for("fn main()");
        assert_eq!(
            draft.update(Revision::INITIAL, anchor.clone(), "second".into(), ts(9)),
            Ok(Revision::new(2))
        );
        let record = draft.record();
        assert_eq!(
            (&record.reference, record.body.as_str()),
            (&anchor, "second")
        );
        assert_eq!((record.created_at, record.updated_at), (ts(3), ts(9)));
        assert_eq!(
            draft.check_delete(Revision::INITIAL),
            Err(DomainError::Conflict(
                "The draft was changed elsewhere. Refresh and try again."
            ))
        );
        assert_eq!(draft.check_delete(Revision::new(2)), Ok(()));
    }

    #[test]
    fn retargeting_readdresses_an_unsent_draft_under_its_revision() {
        let (objective, _) = active_objective();
        let mut draft = save_new(&objective, 1, "\tlet é", "fix");
        assert!(matches!(
            draft.retarget(Revision::new(5), session_id(2), ts(4)),
            Err(DomainError::Conflict(_))
        ));
        assert_eq!(
            draft.retarget(Revision::INITIAL, session_id(2), ts(4)),
            Ok(Revision::new(2))
        );
        assert_eq!(draft.record().destination_session_id, session_id(2));
        assert_eq!(draft.record().body, "fix", "only the destination moves");

        let current = digest(1);
        let mut submitted = save_new(&objective, 2, "\tlet é", "fix");
        let mut batch = [BatchEntry {
            draft: &mut submitted,
            revision: Revision::INITIAL,
            current: present(&current, b""),
        }];
        accept_feedback(&objective, acceptance(&objective), &mut batch, None, ts(5))
            .expect("accepted");
        assert!(matches!(
            submitted.retarget(Revision::INITIAL, session_id(2), ts(6)),
            Err(DomainError::InvalidLifecycle(_))
        ));
        assert!(!submitted.is_unsent());
        assert_eq!(submitted.record().updated_at, ts(5));
    }

    #[test]
    fn staleness_follows_the_anchored_bytes_not_the_digest_alone() {
        let (objective, _) = active_objective();
        let draft = save_new(&objective, 1, "\tlet é", "fix");
        let same = digest(1);
        let other = digest(2);
        assert!(!draft.is_stale(present(&same, b"")), "same artifact");
        assert!(
            !draft.is_stale(present(&other, FILE.as_bytes())),
            "unchanged bytes"
        );
        let edited = FILE.replacen('é', "e", 1);
        assert!(draft.is_stale(present(&other, edited.as_bytes())));
        assert!(draft.is_stale(CurrentContent::Missing));
    }

    #[test]
    fn restore_rejects_an_unsaved_revision_or_an_oversized_body() {
        let (objective, _) = active_objective();
        let record = save_new(&objective, 1, "\tlet é", "fix").into_record();
        assert_eq!(
            Draft::restore(record.clone()).map(|draft| draft.id()),
            Ok(draft_id(1))
        );
        let mut unsaved = record.clone();
        unsaved.revision = Revision::new(0);
        assert!(matches!(
            Draft::restore(unsaved),
            Err(DomainError::DurabilityFailure(_))
        ));
        let mut oversized = record;
        oversized.body = "x".repeat(MAX_TEXT_BYTES + 1);
        oversized.state = DraftState::Submitted {
            input_id: input_id(1),
        };
        assert!(Draft::restore(oversized).is_err());
    }
}
