//! Contextual Feedback: drafts (CAS-saved), batch submission into immutable accepted input, and delivery state.

use crate::domain::execution::{ExecutionGeneration, ExecutionState};
use crate::domain::ids::{
    DraftId, InputId, ObjectiveId, OperationId, Revision, SessionId, Timestamp,
};
use crate::domain::inspection::{CurrentContent, MAX_TEXT_BYTES, Reference, Side};
use crate::domain::objectives::{Lifecycle, Objective};
use crate::error::DomainError;

/// Maximum number of drafts in one submitted batch; equals the wire bound of `feedback.submit`
/// (`FeedbackSubmitCommand.drafts`, `maxItems` 256), so every batch the protocol admits is submittable.
pub const MAX_BATCH_ITEMS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InputMode {
    Prompt,
    Steer,
    FollowUp,
}

impl InputMode {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Steer => "steer",
            Self::FollowUp => "follow_up",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftState {
    Unsent,
    Submitted { input_id: InputId },
}

/// Plain stored form of a draft. Use [`Draft::restore`] to obtain a validated entity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftRecord {
    pub id: DraftId,
    pub objective_id: ObjectiveId,
    /// Original destination; never silently redirected.
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

/// `draft.save` request; `expected_revision` 0 means create.
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
        return Err(DomainError::LimitReached("draft body too large"));
    }
    Ok(())
}

/// CAS save. Creating (`expected_revision == 0`) binds the destination to the Objective's current session.
/// Updating succeeds only when `expected_revision` equals the stored revision; returns the saved draft.
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
                return Err(DomainError::InvalidLifecycle("objective is closed"));
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
            Err(DomainError::Conflict("draft already exists"))
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
    pub fn restore(record: DraftRecord) -> Result<Self, DomainError> {
        if record.revision.value() == 0 || record.body.len() > MAX_TEXT_BYTES {
            return Err(DomainError::DurabilityFailure("inconsistent stored draft"));
        }
        Ok(Self { record })
    }

    pub const fn record(&self) -> &DraftRecord {
        &self.record
    }

    pub fn into_record(self) -> DraftRecord {
        self.record
    }

    pub const fn id(&self) -> DraftId {
        self.record.id
    }

    pub const fn revision(&self) -> Revision {
        self.record.revision
    }

    pub const fn is_unsent(&self) -> bool {
        matches!(self.record.state, DraftState::Unsent)
    }

    fn check_editable(&self, expected_revision: Revision) -> Result<(), DomainError> {
        if !self.is_unsent() {
            return Err(DomainError::InvalidLifecycle(
                "submitted drafts are history",
            ));
        }
        if expected_revision != self.record.revision {
            return Err(DomainError::Conflict("draft revision mismatch"));
        }
        Ok(())
    }

    /// CAS update of reference and body; returns the new revision.
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

    /// Explicit re-targeting to a confirmed new destination (records a new revision).
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

    /// Validates `draft.delete`; the caller removes the record.
    pub fn check_delete(&self, expected_revision: Revision) -> Result<(), DomainError> {
        self.check_editable(expected_revision)
    }

    pub fn is_stale(&self, current: CurrentContent<'_>) -> bool {
        self.record.reference.staleness(current).is_stale()
    }
}

/// Frozen copy of one submitted draft revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedbackItem {
    pub draft_id: DraftId,
    pub draft_revision: Revision,
    pub reference: Reference,
    pub body: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockedReason {
    ExecutionUnavailable,
    StaleAssociation,
    StaleReference,
    InvalidLifecycle,
}

impl BlockedReason {
    pub const fn code(self) -> &'static str {
        match self {
            Self::ExecutionUnavailable => "execution_unavailable",
            Self::StaleAssociation => "stale_association",
            Self::StaleReference => "stale_reference",
            Self::InvalidLifecycle => "invalid_lifecycle",
        }
    }
}

/// Delivery is tracked separately from acceptance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryState {
    Queued,
    Dispatched {
        generation: ExecutionGeneration,
    },
    Received {
        generation: ExecutionGeneration,
    },
    /// The worker was lost after dispatch; never redispatched automatically.
    Uncertain {
        generation: ExecutionGeneration,
    },
    Blocked {
        reason: BlockedReason,
    },
}

impl DeliveryState {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Dispatched { .. } => "dispatched",
            Self::Received { .. } => "received",
            Self::Uncertain { .. } => "uncertain",
            Self::Blocked { .. } => "blocked",
        }
    }
}

/// Outcome of a dispatch attempt for a queued input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchDecision {
    /// Hand the input to the worker of this generation (state is now `Dispatched`).
    Dispatch { generation: ExecutionGeneration },
    /// The worker is still starting; stay queued.
    Wait,
    /// Preconditions failed; state is now `Blocked` until an explicit continuation re-queues it.
    Blocked { reason: BlockedReason },
}

/// Durably accepted input. Content is immutable history; only `delivery` evolves, through its methods.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedInput {
    pub id: InputId,
    pub operation_id: OperationId,
    pub objective_id: ObjectiveId,
    pub session_id: SessionId,
    pub association_revision: Revision,
    pub mode: InputMode,
    /// Empty for a direct message.
    pub items: Vec<FeedbackItem>,
    pub note: Option<String>,
    pub rendered_text: String,
    pub accepted_at: Timestamp,
    pub delivery: DeliveryState,
}

/// Identity and routing of an acceptance request.
#[derive(Clone, Copy, Debug)]
pub struct Acceptance {
    pub input_id: InputId,
    pub operation_id: OperationId,
    pub session_id: SessionId,
    pub association_revision: Revision,
    pub mode: InputMode,
}

/// One draft of a submitted batch with the revision the developer reviewed and its current content.
#[derive(Debug)]
pub struct BatchEntry<'a> {
    pub draft: &'a mut Draft,
    pub revision: Revision,
    pub current: CurrentContent<'a>,
}

fn check_accepting(objective: &Objective, acceptance: &Acceptance) -> Result<(), DomainError> {
    if objective.lifecycle() != Lifecycle::Active {
        return Err(DomainError::InvalidLifecycle(
            "input requires an active objective; return a frozen candidate for revision first",
        ));
    }
    objective
        .execution()
        .check_association(acceptance.session_id, acceptance.association_revision)
}

fn initial_delivery(state: &ExecutionState) -> DeliveryState {
    if matches!(
        state,
        ExecutionState::Starting | ExecutionState::Running | ExecutionState::Idle
    ) {
        DeliveryState::Queued
    } else {
        DeliveryState::Blocked {
            reason: BlockedReason::ExecutionUnavailable,
        }
    }
}

/// Direct instruction (`message.send`).
pub fn accept_message(
    objective: &Objective,
    acceptance: Acceptance,
    text: String,
    now: Timestamp,
) -> Result<AcceptedInput, DomainError> {
    check_accepting(objective, &acceptance)?;
    if text.trim().is_empty() {
        return Err(DomainError::InvalidRequest("message text is required"));
    }
    if text.len() > MAX_TEXT_BYTES {
        return Err(DomainError::LimitReached("message too large"));
    }
    Ok(AcceptedInput {
        id: acceptance.input_id,
        operation_id: acceptance.operation_id,
        objective_id: objective.id(),
        session_id: acceptance.session_id,
        association_revision: acceptance.association_revision,
        mode: acceptance.mode,
        items: Vec::new(),
        note: None,
        rendered_text: text,
        accepted_at: now,
        delivery: initial_delivery(objective.execution().state()),
    })
}

/// Validates a whole feedback batch and, only if every check passes, creates the immutable accepted input and
/// marks each draft `Submitted`. Any stale item blocks the entire batch (`StaleReference` lists every stale
/// draft) and nothing is mutated. The caller persists the input and drafts in one transaction.
pub fn accept_feedback(
    objective: &Objective,
    acceptance: Acceptance,
    batch: &mut [BatchEntry<'_>],
    note: Option<String>,
    now: Timestamp,
) -> Result<AcceptedInput, DomainError> {
    if batch.is_empty() {
        return Err(DomainError::InvalidRequest("feedback batch is empty"));
    }
    if batch.len() > MAX_BATCH_ITEMS {
        return Err(DomainError::LimitReached("feedback batch too large"));
    }
    if note
        .as_deref()
        .is_some_and(|text| text.len() > MAX_TEXT_BYTES)
    {
        return Err(DomainError::LimitReached("note too large"));
    }
    for (index, entry) in batch.iter().enumerate() {
        if batch[..index]
            .iter()
            .any(|earlier| earlier.draft.id() == entry.draft.id())
        {
            return Err(DomainError::InvalidRequest("draft listed twice"));
        }
        let record = entry.draft.record();
        if record.objective_id != objective.id() {
            return Err(DomainError::NotFound("draft"));
        }
        if !entry.draft.is_unsent() {
            return Err(DomainError::Conflict("draft already submitted"));
        }
        if record.revision != entry.revision {
            return Err(DomainError::Conflict("draft revision mismatch"));
        }
        if record.body.trim().is_empty() {
            return Err(DomainError::InvalidRequest("feedback comment is required"));
        }
    }
    check_accepting(objective, &acceptance)?;
    if batch
        .iter()
        .any(|entry| entry.draft.record().destination_session_id != acceptance.session_id)
    {
        return Err(DomainError::StaleAssociation);
    }
    let stale: Vec<DraftId> = batch
        .iter()
        .filter(|entry| entry.draft.is_stale(entry.current))
        .map(|entry| entry.draft.id())
        .collect();
    if !stale.is_empty() {
        return Err(DomainError::StaleReference { drafts: stale });
    }

    let items: Vec<FeedbackItem> = batch
        .iter()
        .map(|entry| {
            let record = entry.draft.record();
            FeedbackItem {
                draft_id: record.id,
                draft_revision: record.revision,
                reference: record.reference.clone(),
                body: record.body.clone(),
            }
        })
        .collect();
    let rendered_text = render_feedback(&items, note.as_deref());
    if rendered_text.len() > MAX_TEXT_BYTES {
        return Err(DomainError::LimitReached("rendered feedback too large"));
    }
    for entry in batch.iter_mut() {
        entry.draft.record.state = DraftState::Submitted {
            input_id: acceptance.input_id,
        };
        entry.draft.record.updated_at = now;
    }
    Ok(AcceptedInput {
        id: acceptance.input_id,
        operation_id: acceptance.operation_id,
        objective_id: objective.id(),
        session_id: acceptance.session_id,
        association_revision: acceptance.association_revision,
        mode: acceptance.mode,
        items,
        note,
        rendered_text,
        accepted_at: now,
        delivery: initial_delivery(objective.execution().state()),
    })
}

/// Agent-facing text: optional note, then one block per item:
/// `file <path> (<version label>[, old side], bytes a–b, line ~N):`, the quoted selection, the comment.
pub fn render_feedback(items: &[FeedbackItem], note: Option<&str>) -> String {
    let mut blocks: Vec<String> = Vec::with_capacity(items.len() + 1);
    if let Some(note) = note.filter(|text| !text.trim().is_empty()) {
        blocks.push(note.to_string());
    }
    for item in items {
        let reference = &item.reference;
        let range = reference.byte_range();
        let side = if reference.side() == Side::Old {
            ", old side"
        } else {
            ""
        };
        let mut block = format!(
            "file {} ({}{side}, bytes {}\u{2013}{}, line ~{}):\n",
            reference.path().display(),
            reference.version().label(),
            range.start(),
            range.end(),
            reference.line_hint(),
        );
        for line in reference.selected_text().lines() {
            block.push_str("> ");
            block.push_str(line);
            block.push('\n');
        }
        block.push_str(&item.body);
        blocks.push(block);
    }
    blocks.join("\n\n")
}

impl AcceptedInput {
    /// Revalidates routing, lifecycle, context and execution authority before handing input to the worker.
    /// `references_stale` is the caller's re-evaluation of every item against the current basis.
    pub fn dispatch(
        &mut self,
        objective: &Objective,
        references_stale: bool,
    ) -> Result<DispatchDecision, DomainError> {
        if self.delivery != DeliveryState::Queued {
            return Err(DomainError::Conflict("input is not queued"));
        }
        if objective.id() != self.objective_id {
            return Err(DomainError::InvalidRequest("input objective mismatch"));
        }
        let execution = objective.execution();
        let blocked = if execution
            .check_association(self.session_id, self.association_revision)
            .is_err()
        {
            Some(BlockedReason::StaleAssociation)
        } else if objective.lifecycle() != Lifecycle::Active {
            Some(BlockedReason::InvalidLifecycle)
        } else if references_stale {
            Some(BlockedReason::StaleReference)
        } else if *execution.state() == ExecutionState::Starting {
            return Ok(DispatchDecision::Wait);
        } else if !execution.state().accepts_dispatch() {
            Some(BlockedReason::ExecutionUnavailable)
        } else {
            None
        };
        if let Some(reason) = blocked {
            self.delivery = DeliveryState::Blocked { reason };
            return Ok(DispatchDecision::Blocked { reason });
        }
        let generation = execution.generation();
        self.delivery = DeliveryState::Dispatched { generation };
        Ok(DispatchDecision::Dispatch { generation })
    }

    /// Worker acknowledged receipt (also accepted as reconciliation evidence for an uncertain delivery).
    pub fn mark_received(&mut self, generation: ExecutionGeneration) -> Result<(), DomainError> {
        match self.delivery {
            DeliveryState::Dispatched { generation: sent }
            | DeliveryState::Uncertain { generation: sent }
            | DeliveryState::Received { generation: sent } => {
                if sent != generation {
                    return Err(DomainError::StaleExecution);
                }
                self.delivery = DeliveryState::Received { generation };
                Ok(())
            }
            _ => Err(DomainError::Conflict("input was not dispatched")),
        }
    }

    /// The worker of the dispatch generation was lost: a dispatched input becomes uncertain. Returns whether
    /// anything changed.
    pub fn mark_worker_lost(&mut self) -> bool {
        if let DeliveryState::Dispatched { generation } = self.delivery {
            self.delivery = DeliveryState::Uncertain { generation };
            return true;
        }
        false
    }

    /// Explicit continuation: a blocked input is queued again (and revalidated at the next dispatch).
    pub fn requeue(&mut self) -> Result<(), DomainError> {
        if !matches!(self.delivery, DeliveryState::Blocked { .. }) {
            return Err(DomainError::Conflict("only blocked input can be re-queued"));
        }
        self.delivery = DeliveryState::Queued;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::execution::ModelRef;
    use crate::domain::inspection::ArtifactDigest;
    use crate::testing::{
        active_objective, defined_objective, digest, draft_id, input_id, objective_id,
        operation_id, reference_at, session_id, ts,
    };

    const FILE: &str = "fn main() {\r\n\tlet é = 1;\r\n\tprintln!(\"{é}\");\r\n}\r\n";

    fn reference_for(selection: &str) -> Reference {
        let start = FILE.find(selection).expect("selection in file");
        reference_at(1, start, start + selection.len(), selection).expect("reference")
    }

    fn save_new(objective: &Objective, id: u128, selection: &str, body: &str) -> Draft {
        let request = SaveDraft {
            draft_id: draft_id(id),
            objective_id: objective.id(),
            expected_revision: Revision::new(0),
            reference: reference_for(selection),
            body: body.to_string(),
        };
        save_draft(None, objective, request, ts(3)).expect("create draft")
    }

    fn acceptance(objective: &Objective) -> Acceptance {
        let session = objective.execution().session().expect("session");
        Acceptance {
            input_id: input_id(1),
            operation_id: operation_id(1),
            session_id: session.session_id,
            association_revision: session.association_revision,
            mode: InputMode::Steer,
        }
    }

    fn present<'a>(digest: &'a ArtifactDigest, bytes: &'a [u8]) -> CurrentContent<'a> {
        CurrentContent::Present { digest, bytes }
    }

    #[test]
    fn draft_create_binds_current_destination() {
        let (objective, _) = active_objective();
        let draft = save_new(&objective, 1, "\tlet é", "rename");
        assert_eq!(draft.revision(), Revision::INITIAL);
        assert_eq!(draft.record().destination_session_id, session_id(1));
        let without_session = defined_objective();
        let request = SaveDraft {
            draft_id: draft_id(2),
            objective_id: without_session.id(),
            expected_revision: Revision::new(0),
            reference: reference_for("\tlet é"),
            body: String::new(),
        };
        assert_eq!(
            save_draft(None, &without_session, request, ts(3)),
            Err(DomainError::ExecutionUnavailable)
        );
    }

    #[test]
    fn draft_cas_save_conflicts_and_never_wraps() {
        let (objective, _) = active_objective();
        let draft = save_new(&objective, 1, "\tlet é", "first");
        let update = |expected: u64, body: &str| SaveDraft {
            draft_id: draft_id(1),
            objective_id: objective.id(),
            expected_revision: Revision::new(expected),
            reference: reference_for("\tlet é"),
            body: body.to_string(),
        };
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
            Err(DomainError::Conflict("draft revision mismatch"))
        );
        assert_eq!(
            save_draft(Some(saved.clone()), &objective, update(0, "dup"), ts(5)),
            Err(DomainError::Conflict("draft already exists"))
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
    fn accepted_feedback_renders_byte_exact_references() {
        let (objective, _) = active_objective();
        let mut first = save_new(&objective, 1, "\tlet é = 1;", "Use a descriptive name.");
        let mut second = save_new(
            &objective,
            2,
            "\tprintln!(\"{é}\");\r\n}",
            "Drop the debug print.",
        );
        let changed = digest(9);
        let edited = format!("{FILE}// appended later\n");
        let mut batch = [
            BatchEntry {
                draft: &mut first,
                revision: Revision::INITIAL,
                current: present(&changed, edited.as_bytes()),
            },
            BatchEntry {
                draft: &mut second,
                revision: Revision::INITIAL,
                current: present(&changed, edited.as_bytes()),
            },
        ];
        let input = accept_feedback(
            &objective,
            acceptance(&objective),
            &mut batch,
            Some("Please revise:".into()),
            ts(5),
        )
        .expect("accepted");
        assert_eq!(input.items.len(), 2);
        assert_eq!(input.delivery, DeliveryState::Queued);
        let label = input.items[0].reference.version().label();
        let start = FILE.find("\tlet").expect("offset");
        let expected_first = format!(
            "file src/main.rs ({label}, bytes {start}\u{2013}{}, line ~1):\n> \tlet é = 1;\nUse a descriptive name.",
            start + "\tlet é = 1;".len()
        );
        assert!(input.rendered_text.starts_with("Please revise:\n\n"));
        assert!(
            input.rendered_text.contains(&expected_first),
            "{}",
            input.rendered_text
        );
        assert!(
            input
                .rendered_text
                .contains("> \tprintln!(\"{é}\");\n> }\nDrop the debug print.")
        );
        assert!(input.rendered_text.contains(" bytes "));
    }

    #[test]
    fn any_stale_item_blocks_the_whole_batch() {
        let (objective, _) = active_objective();
        let mut d1 = save_new(&objective, 1, "fn main()", "a");
        let mut d2 = save_new(&objective, 2, "\tlet é", "b");
        let mut d3 = save_new(&objective, 3, "}\r\n", "c");
        let other = digest(5);
        // Replacing "é" (2 bytes) with "e" (1 byte) shifts every later offset by one byte.
        let shifted = FILE.replacen("é", "e", 1);
        let before = (d1.clone(), d2.clone(), d3.clone());
        let mut batch = [
            BatchEntry {
                draft: &mut d1,
                revision: Revision::INITIAL,
                current: present(&other, shifted.as_bytes()),
            },
            BatchEntry {
                draft: &mut d2,
                revision: Revision::INITIAL,
                current: present(&other, shifted.as_bytes()),
            },
            BatchEntry {
                draft: &mut d3,
                revision: Revision::INITIAL,
                current: CurrentContent::Missing,
            },
        ];
        let result = accept_feedback(&objective, acceptance(&objective), &mut batch, None, ts(5));
        assert_eq!(
            result,
            Err(DomainError::StaleReference {
                drafts: vec![draft_id(2), draft_id(3)]
            })
        );
        assert_eq!((d1, d2, d3), before, "no draft is marked submitted");
    }

    #[test]
    fn batch_rejects_mismatched_revisions_duplicates_and_foreign_drafts() {
        let (objective, _) = active_objective();
        let mut d1 = save_new(&objective, 1, "fn main()", "a");
        let same = digest(1);
        let mut batch = [BatchEntry {
            draft: &mut d1,
            revision: Revision::new(2),
            current: present(&same, b""),
        }];
        assert_eq!(
            accept_feedback(&objective, acceptance(&objective), &mut batch, None, ts(5)),
            Err(DomainError::Conflict("draft revision mismatch"))
        );
        assert_eq!(
            accept_feedback(&objective, acceptance(&objective), &mut [], None, ts(5)),
            Err(DomainError::InvalidRequest("feedback batch is empty"))
        );
        let mut empty_body = save_new(&objective, 2, "fn main()", "  ");
        let mut batch = [BatchEntry {
            draft: &mut empty_body,
            revision: Revision::INITIAL,
            current: present(&same, b""),
        }];
        assert!(matches!(
            accept_feedback(&objective, acceptance(&objective), &mut batch, None, ts(5)),
            Err(DomainError::InvalidRequest(_))
        ));
        let mut record = save_new(&objective, 3, "fn main()", "x").into_record();
        record.objective_id = objective_id(99);
        let mut foreign = Draft::restore(record).expect("restore");
        let mut batch = [BatchEntry {
            draft: &mut foreign,
            revision: Revision::INITIAL,
            current: present(&same, b""),
        }];
        assert_eq!(
            accept_feedback(&objective, acceptance(&objective), &mut batch, None, ts(5)),
            Err(DomainError::NotFound("draft"))
        );
    }

    #[test]
    fn batch_size_matches_the_wire_contract() {
        // `FeedbackSubmitCommand.drafts` admits 1..=256 drafts; every one of them must be submittable.
        fn entries<'a>(drafts: &'a mut [Draft], same: &'a ArtifactDigest) -> Vec<BatchEntry<'a>> {
            drafts
                .iter_mut()
                .map(|draft| BatchEntry {
                    draft,
                    revision: Revision::INITIAL,
                    current: present(same, b""),
                })
                .collect()
        }
        let (objective, _) = active_objective();
        let same = digest(1);
        let mut drafts: Vec<Draft> = (0..=256)
            .map(|n| save_new(&objective, 1000 + n, "fn main()", "fix"))
            .collect();
        assert_eq!(
            accept_feedback(
                &objective,
                acceptance(&objective),
                &mut entries(&mut drafts, &same),
                None,
                ts(5)
            ),
            Err(DomainError::LimitReached("feedback batch too large"))
        );
        let mut full = entries(&mut drafts[..256], &same);
        let input = accept_feedback(&objective, acceptance(&objective), &mut full, None, ts(5))
            .expect("a full wire batch is accepted");
        assert_eq!(input.items.len(), 256);
    }

    #[test]
    fn destination_must_match_current_association() {
        let (mut objective, generation) = active_objective();
        let mut draft = save_new(&objective, 1, "fn main()", "a");
        let old = acceptance(&objective);
        objective.request_stop(ts(4)).expect("stop");
        objective
            .observe_stopped(generation, ts(4))
            .expect("stopped");
        objective
            .replace_session(
                session_id(2),
                ModelRef::new("p", "m").expect("model"),
                ts(4),
            )
            .expect("replace");
        let same = digest(1);
        let mut batch = [BatchEntry {
            draft: &mut draft,
            revision: Revision::INITIAL,
            current: present(&same, b""),
        }];
        assert_eq!(
            accept_feedback(&objective, old, &mut batch, None, ts(5)),
            Err(DomainError::StaleAssociation)
        );
        // Addressing the new association still fails: the draft's original destination is preserved.
        let new = acceptance(&objective);
        assert_eq!(
            accept_feedback(&objective, new, &mut batch, None, ts(5)),
            Err(DomainError::StaleAssociation)
        );
        batch[0]
            .draft
            .retarget(Revision::INITIAL, session_id(2), ts(6))
            .expect("explicit retarget");
        batch[0].revision = Revision::new(2);
        let input = accept_feedback(&objective, new, &mut batch, None, ts(7)).expect("accepted");
        assert_eq!(
            input.delivery,
            DeliveryState::Blocked {
                reason: BlockedReason::ExecutionUnavailable
            }
        );
    }

    #[test]
    fn frozen_objective_rejects_input_until_returned() {
        let (mut objective, generation) = active_objective();
        objective.request_stop(ts(4)).expect("stop");
        objective
            .observe_stopped(generation, ts(4))
            .expect("stopped");
        objective
            .transition(&[Lifecycle::Frozen], ts(5))
            .expect("freeze");
        assert!(matches!(
            accept_message(&objective, acceptance(&objective), "do more".into(), ts(6)),
            Err(DomainError::InvalidLifecycle(_))
        ));
        objective
            .transition(&[Lifecycle::Active], ts(7))
            .expect("return");
        let input = accept_message(&objective, acceptance(&objective), "do more".into(), ts(8))
            .expect("accepted");
        assert_eq!(
            input.delivery,
            DeliveryState::Blocked {
                reason: BlockedReason::ExecutionUnavailable
            }
        );
        assert!(input.items.is_empty());
        assert_eq!(input.rendered_text, "do more");
    }

    #[test]
    fn delivery_transitions() {
        let (mut objective, generation) = active_objective();
        let mut input = accept_message(&objective, acceptance(&objective), "go".into(), ts(5))
            .expect("accepted");
        assert_eq!(
            input.dispatch(&objective, false),
            Ok(DispatchDecision::Dispatch { generation })
        );
        assert!(
            input.dispatch(&objective, false).is_err(),
            "already dispatched"
        );
        assert_eq!(input.mark_received(generation), Ok(()));
        assert_eq!(input.delivery, DeliveryState::Received { generation });

        let mut lost = accept_message(&objective, acceptance(&objective), "go".into(), ts(5))
            .expect("accepted");
        lost.dispatch(&objective, false).expect("dispatch");
        assert!(lost.mark_worker_lost());
        assert_eq!(lost.delivery, DeliveryState::Uncertain { generation });
        assert!(
            lost.dispatch(&objective, false).is_err(),
            "never redispatched automatically"
        );
        assert!(lost.requeue().is_err(), "uncertain is not blocked");
        objective.interrupt_after_restart(ts(6)).expect("restart");
        let next = objective.resume_execution(ts(7)).expect("resume");
        assert_eq!(lost.mark_received(next), Err(DomainError::StaleExecution));
        assert_eq!(
            lost.mark_received(generation),
            Ok(()),
            "reconciled evidence"
        );
    }

    #[test]
    fn dispatch_revalidates_and_blocks() {
        let (mut objective, generation) = active_objective();
        let mut stale = accept_message(&objective, acceptance(&objective), "go".into(), ts(5))
            .expect("accepted");
        assert_eq!(
            stale.dispatch(&objective, true),
            Ok(DispatchDecision::Blocked {
                reason: BlockedReason::StaleReference
            })
        );
        let mut queued = accept_message(&objective, acceptance(&objective), "go".into(), ts(5))
            .expect("accepted");
        objective.request_stop(ts(6)).expect("stop");
        assert_eq!(
            queued.dispatch(&objective, false),
            Ok(DispatchDecision::Blocked {
                reason: BlockedReason::ExecutionUnavailable
            })
        );
        objective
            .observe_stopped(generation, ts(6))
            .expect("stopped");
        let next = objective.resume_execution(ts(7)).expect("resume");
        queued.requeue().expect("explicit continuation");
        assert_eq!(
            queued.dispatch(&objective, false),
            Ok(DispatchDecision::Wait)
        );
        assert_eq!(queued.delivery, DeliveryState::Queued);
        objective.observe_worker_ready(next, ts(8)).expect("ready");
        assert_eq!(
            queued.dispatch(&objective, false),
            Ok(DispatchDecision::Dispatch { generation: next })
        );
    }

    #[test]
    fn dispatch_blocks_on_replaced_association() {
        let (mut objective, generation) = active_objective();
        let mut input = accept_message(&objective, acceptance(&objective), "go".into(), ts(5))
            .expect("accepted");
        objective.request_stop(ts(6)).expect("stop");
        objective
            .observe_stopped(generation, ts(6))
            .expect("stopped");
        objective
            .replace_session(
                session_id(3),
                ModelRef::new("p", "m").expect("model"),
                ts(7),
            )
            .expect("replace");
        assert_eq!(
            input.dispatch(&objective, false),
            Ok(DispatchDecision::Blocked {
                reason: BlockedReason::StaleAssociation
            })
        );
    }
}
