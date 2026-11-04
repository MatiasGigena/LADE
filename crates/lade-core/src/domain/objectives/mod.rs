//! Objectives: the unit of delegated work, its defining values, its lifecycle and the attention it
//! asks of the user.

mod attention;
mod definition;
mod lifecycle;
#[cfg(test)]
mod tests;

pub use attention::{Attention, AttentionFacts, AttentionView, Badge, derive_attention};
pub use definition::{BranchName, Intent, MAX_BRANCH_BYTES, MAX_TITLE_CHARS, Title};
pub use lifecycle::Lifecycle;

use crate::domain::execution::{Execution, ExecutionGeneration, ExecutionState, ModelRef};
use crate::domain::ids::{GitId, ObjectiveId, Revision, SessionId, Timestamp, WorkspaceId};
use crate::error::DomainError;

pub const CLOSED: &str = "This Objective is closed.";
pub const INTENT_CHANGED: &str = "The intent was changed elsewhere. Refresh and try again.";
pub const UNSENT_DRAFTS_NEED_CONFIRMATION: &str =
    "This Objective has unsent drafts. Confirm that they may be deleted.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectiveRecord {
    pub id: ObjectiveId,
    pub workspace_id: WorkspaceId,
    pub title: Title,
    pub intent: Intent,
    pub intent_revision: Revision,
    pub integration_target: BranchName,
    pub start_commit: GitId,
    pub branch: BranchName,
    pub lifecycle: Lifecycle,
    pub execution: Execution,
    pub cleaned_up: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub revision: Revision,
}

#[derive(Clone, Debug)]
pub struct NewObjective {
    pub id: ObjectiveId,
    pub workspace_id: WorkspaceId,
    pub title: Title,
    pub intent: Intent,
    pub integration_target: BranchName,
    pub start_commit: GitId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Objective {
    record: ObjectiveRecord,
}

impl Objective {
    #[must_use]
    pub fn define(new: NewObjective, now: Timestamp) -> Self {
        Self {
            record: ObjectiveRecord {
                branch: BranchName::for_objective(new.id),
                id: new.id,
                workspace_id: new.workspace_id,
                title: new.title,
                intent: new.intent,
                intent_revision: Revision::INITIAL,
                integration_target: new.integration_target,
                start_commit: new.start_commit,
                lifecycle: Lifecycle::Defined,
                execution: Execution::new(),
                cleaned_up: false,
                created_at: now,
                updated_at: now,
                revision: Revision::INITIAL,
            },
        }
    }

    pub fn restore(record: ObjectiveRecord) -> Result<Self, DomainError> {
        let lifecycle = record.lifecycle;
        let state = record.execution.state();
        let valid = record.branch == BranchName::for_objective(record.id)
            && (!record.cleaned_up || lifecycle.is_terminal())
            && (lifecycle != Lifecycle::Defined || *state == ExecutionState::None)
            && (lifecycle == Lifecycle::Defined
                || lifecycle == Lifecycle::Active
                || !state.is_live())
            && record.revision.value() >= 1
            && record.intent_revision.value() >= 1;
        if !valid {
            return Err(DomainError::DurabilityFailure(
                "inconsistent stored objective",
            ));
        }
        Ok(Self { record })
    }

    #[must_use]
    pub const fn record(&self) -> &ObjectiveRecord {
        &self.record
    }

    #[must_use]
    pub fn into_record(self) -> ObjectiveRecord {
        self.record
    }

    #[must_use]
    pub const fn id(&self) -> ObjectiveId {
        self.record.id
    }

    #[must_use]
    pub const fn lifecycle(&self) -> Lifecycle {
        self.record.lifecycle
    }

    #[must_use]
    pub const fn execution(&self) -> &Execution {
        &self.record.execution
    }

    #[must_use]
    pub const fn revision(&self) -> Revision {
        self.record.revision
    }

    fn update<T>(
        &mut self,
        now: Timestamp,
        change: impl FnOnce(&mut ObjectiveRecord) -> Result<T, DomainError>,
    ) -> Result<T, DomainError> {
        let mut next = self.record.clone();
        let output = change(&mut next)?;
        next.revision = self.record.revision.advance()?;
        next.updated_at = now;
        self.record = next;
        Ok(output)
    }

    pub(crate) fn transition(
        &mut self,
        path: &[Lifecycle],
        now: Timestamp,
    ) -> Result<(), DomainError> {
        self.update(now, |record| {
            for &to in path {
                if !record.lifecycle.permits(to) {
                    return Err(DomainError::InvalidLifecycle(
                        "That isn't possible in the Objective's current state.",
                    ));
                }
                let excludes_writers = to != Lifecycle::Active;
                if excludes_writers && record.execution.state().is_live() {
                    return Err(DomainError::InvalidLifecycle("Stop the agent first."));
                }
                record.lifecycle = to;
            }
            Ok(())
        })
    }

    pub fn update_intent(
        &mut self,
        expected_intent_revision: Revision,
        intent: Intent,
        now: Timestamp,
    ) -> Result<Revision, DomainError> {
        self.update(now, |record| {
            if !matches!(
                record.lifecycle,
                Lifecycle::Defined | Lifecycle::Active | Lifecycle::Frozen
            ) {
                return Err(DomainError::InvalidLifecycle(
                    "The intent can't change after approval. Return the candidate for revision first.",
                ));
            }
            if record.intent_revision != expected_intent_revision {
                return Err(DomainError::Conflict(INTENT_CHANGED));
            }
            record.intent_revision = record.intent_revision.advance()?;
            record.intent = intent;
            Ok(record.intent_revision)
        })
    }

    pub fn start_execution(
        &mut self,
        session_id: SessionId,
        model: ModelRef,
        now: Timestamp,
    ) -> Result<ExecutionGeneration, DomainError> {
        self.update(now, |record| {
            if !matches!(record.lifecycle, Lifecycle::Defined | Lifecycle::Active) {
                return Err(DomainError::InvalidLifecycle(
                    "The agent can start only before review. Return the candidate for revision first.",
                ));
            }
            let generation = record.execution.start(session_id, model)?;
            record.lifecycle = Lifecycle::Active;
            Ok(generation)
        })
    }

    pub fn resume_execution(&mut self, now: Timestamp) -> Result<ExecutionGeneration, DomainError> {
        self.update(now, |record| {
            if record.lifecycle != Lifecycle::Active {
                return Err(DomainError::InvalidLifecycle(
                    "The agent can be resumed only while the Objective is in progress.",
                ));
            }
            record.execution.resume()
        })
    }

    pub fn request_stop(&mut self, now: Timestamp) -> Result<(), DomainError> {
        self.update(now, |record| record.execution.request_stop())
    }

    pub fn observe_worker_ready(
        &mut self,
        generation: ExecutionGeneration,
        now: Timestamp,
    ) -> Result<(), DomainError> {
        self.update(now, |record| record.execution.worker_ready(generation))
    }

    pub fn observe_turn(
        &mut self,
        generation: ExecutionGeneration,
        running: bool,
        now: Timestamp,
    ) -> Result<(), DomainError> {
        self.update(now, |record| {
            record.execution.observe_turn(generation, running)
        })
    }

    pub fn observe_stopped(
        &mut self,
        generation: ExecutionGeneration,
        now: Timestamp,
    ) -> Result<(), DomainError> {
        self.update(now, |record| record.execution.stopped(generation))
    }

    pub fn observe_failure(
        &mut self,
        generation: ExecutionGeneration,
        reason: &str,
        now: Timestamp,
    ) -> Result<(), DomainError> {
        self.update(now, |record| record.execution.fail(generation, reason))
    }

    pub fn interrupt_after_restart(&mut self, now: Timestamp) -> Result<bool, DomainError> {
        if !self.record.execution.state().is_live() {
            return Ok(false);
        }
        self.update(now, |record| Ok(record.execution.interrupt()))
    }

    pub fn rebind_live_session(
        &mut self,
        generation: ExecutionGeneration,
        session_id: SessionId,
        now: Timestamp,
    ) -> Result<Revision, DomainError> {
        self.update(now, |record| {
            if record.lifecycle != Lifecycle::Active {
                return Err(DomainError::InvalidLifecycle(
                    "The session can be replaced only while the Objective is in progress.",
                ));
            }
            record.execution.rebind_live_session(generation, session_id)
        })
    }

    pub fn select_model(&mut self, model: ModelRef, now: Timestamp) -> Result<(), DomainError> {
        self.update(now, |record| {
            if record.lifecycle.is_terminal() {
                return Err(DomainError::InvalidLifecycle(CLOSED));
            }
            record.execution.select_model(model)
        })
    }

    pub fn replace_session(
        &mut self,
        session_id: SessionId,
        model: ModelRef,
        now: Timestamp,
    ) -> Result<Revision, DomainError> {
        self.update(now, |record| {
            if record.lifecycle.is_terminal() {
                return Err(DomainError::InvalidLifecycle(CLOSED));
            }
            record.execution.replace_session(session_id, model)
        })
    }

    pub fn cleanup(
        &mut self,
        unsent_drafts: usize,
        confirm_data_loss: bool,
        now: Timestamp,
    ) -> Result<(), DomainError> {
        self.update(now, |record| {
            if !record.lifecycle.is_terminal() {
                return Err(DomainError::InvalidLifecycle(
                    "Clean up becomes available once the Objective is integrated, completed or discarded.",
                ));
            }
            if record.cleaned_up {
                return Err(DomainError::InvalidLifecycle(
                    "This Objective is already cleaned up.",
                ));
            }
            if unsent_drafts > 0 && !confirm_data_loss {
                return Err(DomainError::Conflict(UNSENT_DRAFTS_NEED_CONFIRMATION));
            }
            record.cleaned_up = true;
            Ok(())
        })
    }
}
