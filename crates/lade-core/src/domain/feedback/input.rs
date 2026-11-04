//! Acceptance of a message or a feedback batch into one durable Accepted Input.

use super::delivery::{DeliveryState, initial_delivery};
use super::draft::Draft;
use crate::domain::ids::{
    DraftId, InputId, ObjectiveId, OperationId, Revision, SessionId, Timestamp,
};
use crate::domain::inspection::{CurrentContent, MAX_TEXT_BYTES, Reference, Side};
use crate::domain::objectives::{CLOSED, Lifecycle, Objective};
use crate::error::DomainError;

pub const MAX_BATCH_ITEMS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InputMode {
    Prompt,
    Steer,
    FollowUp,
}

impl InputMode {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Steer => "steer",
            Self::FollowUp => "follow_up",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedbackItem {
    pub draft_id: DraftId,
    pub draft_revision: Revision,
    pub reference: Reference,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedInput {
    pub id: InputId,
    pub operation_id: OperationId,
    pub objective_id: ObjectiveId,
    pub session_id: SessionId,
    pub association_revision: Revision,
    pub mode: InputMode,
    pub items: Vec<FeedbackItem>,
    pub note: Option<String>,
    pub rendered_text: String,
    pub accepted_at: Timestamp,
    pub delivery: DeliveryState,
}

#[derive(Clone, Copy, Debug)]
pub struct Acceptance {
    pub input_id: InputId,
    pub operation_id: OperationId,
    pub session_id: SessionId,
    pub association_revision: Revision,
    pub mode: InputMode,
}

#[derive(Debug)]
pub struct BatchEntry<'a> {
    pub draft: &'a mut Draft,
    pub revision: Revision,
    pub current: CurrentContent<'a>,
}

/// Why input (a message or feedback) is refused for an Objective in `lifecycle` (not active).
#[must_use]
pub const fn input_refusal(lifecycle: Lifecycle) -> &'static str {
    match lifecycle {
        Lifecycle::Defined | Lifecycle::Active => "Start the agent before sending input.",
        Lifecycle::Frozen | Lifecycle::Approved => {
            "Return the candidate for revision before sending more input."
        }
        _ => CLOSED,
    }
}

fn check_accepting(objective: &Objective, acceptance: &Acceptance) -> Result<(), DomainError> {
    if objective.lifecycle() != Lifecycle::Active {
        return Err(DomainError::InvalidLifecycle(input_refusal(
            objective.lifecycle(),
        )));
    }
    objective
        .execution()
        .check_association(acceptance.session_id, acceptance.association_revision)
}

pub fn accept_message(
    objective: &Objective,
    acceptance: Acceptance,
    text: String,
    now: Timestamp,
) -> Result<AcceptedInput, DomainError> {
    check_accepting(objective, &acceptance)?;
    if text.trim().is_empty() {
        return Err(DomainError::InvalidRequest("Type a message first."));
    }
    if text.len() > MAX_TEXT_BYTES {
        return Err(DomainError::LimitReached("The message is too long."));
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

pub fn accept_feedback(
    objective: &Objective,
    acceptance: Acceptance,
    batch: &mut [BatchEntry<'_>],
    note: Option<String>,
    now: Timestamp,
) -> Result<AcceptedInput, DomainError> {
    if batch.is_empty() {
        return Err(DomainError::InvalidRequest(
            "Select at least one draft to submit.",
        ));
    }
    if batch.len() > MAX_BATCH_ITEMS {
        return Err(DomainError::LimitReached(
            "Too many drafts for one submission. Submit fewer at once.",
        ));
    }
    if note
        .as_deref()
        .is_some_and(|text| text.len() > MAX_TEXT_BYTES)
    {
        return Err(DomainError::LimitReached("The note is too long."));
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
            return Err(DomainError::Conflict("A draft was already submitted."));
        }
        if record.revision != entry.revision {
            return Err(DomainError::Conflict(
                "A draft changed after you reviewed it. Refresh and try again.",
            ));
        }
        if record.body.trim().is_empty() {
            return Err(DomainError::InvalidRequest(
                "Every draft needs a comment before it can be submitted.",
            ));
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
        return Err(DomainError::LimitReached(
            "The feedback is too long to send at once. Submit fewer drafts.",
        ));
    }
    for entry in batch.iter_mut() {
        entry.draft.mark_submitted(acceptance.input_id, now);
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

#[must_use]
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
