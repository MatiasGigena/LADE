//! Recovering commands whose reply was lost with the connection: the service is asked for the
//! operation's outcome, which is applied, rechecked later or reported as uncertain.

use lade_protocol::client::{
    OperationGetQuery, OperationState, OperationStatus, Outcome, ReplyData, ReplyResult, Request,
    RequestFrame,
};

use super::{Core, MAX_RECHECKS, Operation, Pending, Step, notice};
use crate::presentation::controller::intents::Intent;
use crate::presentation::state::model::NoticeLevel;
use crate::presentation::state::store::StoreUpdate;
use crate::transport::requests::{self, Context};

impl Core {
    pub(super) fn recover(&mut self, operation: Operation) -> RequestFrame {
        let request = Request::OperationGet(OperationGetQuery {
            operation_id: operation.id.clone(),
        });
        let frame = self.query_frame(request);
        self.pending
            .insert(frame.request_id.clone(), Pending::Recover(operation));
        frame
    }

    /// Applies a recovered outcome; [`requests::recovered_effects`] decides which are stale.
    fn settle_recovered(&mut self, context: &Context, data: ReplyData) -> Step {
        match requests::recovered_effects(context, &data) {
            Some(effects) => self.apply_effects(effects),
            None => self.settle(context, ReplyResult::Ok(Box::new(data))),
        }
    }

    pub(super) fn recovered(&mut self, operation: Operation, result: ReplyResult) -> Step {
        let label = operation.context.label();
        let objective = operation.context.objective_id().cloned();
        let (state, outcome, note) = match result {
            ReplyResult::Ok(data) => match *data {
                ReplyData::OperationGet(OperationStatus { state, outcome }) => {
                    (state, outcome, None)
                }
                _ => return Step::default(),
            },
            ReplyResult::Rejected { rejection, .. } => (
                OperationState::Unknown,
                None,
                Some(requests::rejection_message(
                    &format!("check the outcome of “{label}”"),
                    &rejection,
                )),
            ),
        };
        match outcome {
            Some(Outcome::Ok(data)) => {
                let mut step = self.settle_recovered(&operation.context, *data);
                step.updates.push(StoreUpdate::Notice(notice(
                    NoticeLevel::Info,
                    "operation_recovered",
                    format!("{label} had completed before the connection dropped."),
                )));
                return step;
            }
            Some(Outcome::Rejected { name, rejection }) => {
                return self.settle(
                    &operation.context,
                    ReplyResult::Rejected {
                        name: Some(name),
                        rejection,
                    },
                );
            }
            None => {}
        }
        let mut step = Step::default();
        let (level, code, message) = self.unresolved(operation, state, note, &mut step);
        let note = notice(level, code, message);
        if let Some(o) = &objective {
            step.updates.push(StoreUpdate::CommandStatus {
                objective_id: o.clone(),
                status: Some(note.clone()),
            });
            step.extend(self.submit(Intent::LoadObjective {
                objective_id: o.clone(),
            }));
        }
        if level != NoticeLevel::Info {
            step.updates.push(StoreUpdate::Notice(note));
        }
        step
    }

    /// The notice for an operation the service has not finished or cannot account for; an
    /// operation still in progress is queued for another check.
    fn unresolved(
        &mut self,
        mut operation: Operation,
        state: OperationState,
        note: Option<String>,
        step: &mut Step,
    ) -> (NoticeLevel, &'static str, String) {
        let label = operation.context.label();
        match state {
            OperationState::Accepted | OperationState::Pending
                if operation.checks < MAX_RECHECKS =>
            {
                operation.checks += 1;
                self.rechecks.push(operation);
                (
                    NoticeLevel::Info,
                    "query_operation",
                    format!("Checking outcome of “{label}”… the service is still working on it."),
                )
            }
            OperationState::Accepted | OperationState::Pending | OperationState::Uncertain => (
                NoticeLevel::Warning,
                "uncertain",
                format!("The outcome of “{label}” is uncertain. Refresh before repeating it."),
            ),
            OperationState::Blocked => (
                NoticeLevel::Warning,
                "blocked",
                format!("“{label}” is blocked by the service."),
            ),
            OperationState::Completed => (
                NoticeLevel::Info,
                "operation_recovered",
                format!("{label} had completed before the connection dropped."),
            ),
            OperationState::Unknown => {
                if let Context::Command {
                    saved_draft: Some(draft_id),
                    ..
                } = &operation.context
                {
                    step.updates.push(StoreUpdate::DraftSaveFailed {
                        draft_id: draft_id.clone(),
                        message: "the save did not reach the service".into(),
                    });
                }
                (
                    NoticeLevel::Warning,
                    "unknown_operation",
                    note.unwrap_or_else(|| {
                        format!(
                            "{label} did not reach the service before the connection dropped; nothing was changed. Try again."
                        )
                    }),
                )
            }
        }
    }
}

#[cfg(test)]
mod tests;
