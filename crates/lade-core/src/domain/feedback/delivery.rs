//! Delivery of an Accepted Input to the agent: dispatch decisions, receipt and blocking.

use super::input::AcceptedInput;
use crate::domain::execution::{ExecutionGeneration, ExecutionState};
use crate::domain::objectives::{Lifecycle, Objective};
use crate::error::DomainError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockedReason {
    ExecutionUnavailable,
    StaleAssociation,
    StaleReference,
    InvalidLifecycle,
}

impl BlockedReason {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::ExecutionUnavailable => "execution_unavailable",
            Self::StaleAssociation => "stale_association",
            Self::StaleReference => "stale_reference",
            Self::InvalidLifecycle => "invalid_lifecycle",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryState {
    Queued,
    Dispatched { generation: ExecutionGeneration },
    Received { generation: ExecutionGeneration },
    Uncertain { generation: ExecutionGeneration },
    Blocked { reason: BlockedReason },
}

impl DeliveryState {
    #[must_use]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchDecision {
    Dispatch { generation: ExecutionGeneration },
    Wait,
    Blocked { reason: BlockedReason },
}

pub(super) fn initial_delivery(state: &ExecutionState) -> DeliveryState {
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

impl AcceptedInput {
    /// Decides whether queued input can go to the agent now; a blocking reason is recorded on the
    /// input.
    ///
    /// # Errors
    ///
    /// - `DomainError::Conflict` when the input is not queued.
    /// - `DomainError::InvalidRequest` when `objective` is not the input's Objective.
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

    /// Records the agent's receipt of input sent in `generation`.
    ///
    /// # Errors
    ///
    /// - `DomainError::StaleExecution` when it was sent in another generation.
    /// - `DomainError::Conflict` when it was never dispatched.
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

    pub fn mark_worker_lost(&mut self) -> bool {
        if let DeliveryState::Dispatched { generation } = self.delivery {
            self.delivery = DeliveryState::Uncertain { generation };
            return true;
        }
        false
    }

    /// Queues blocked input again after the user explicitly continues.
    ///
    /// # Errors
    ///
    /// `DomainError::Conflict` unless the input is blocked.
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
    use crate::domain::feedback::accept_message;
    use crate::domain::feedback::fixtures::acceptance;
    use crate::domain::objectives::BranchName;
    use crate::testing::{active_objective, defined_objective, objective_id, session_id, ts};

    fn queued(objective: &Objective) -> AcceptedInput {
        accept_message(objective, acceptance(objective), "go".into(), ts(5)).expect("accepted")
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

    #[test]
    fn new_input_is_queued_only_while_the_agent_can_take_it() {
        for (state, queued) in [
            (ExecutionState::None, false),
            (ExecutionState::Starting, true),
            (ExecutionState::Running, true),
            (ExecutionState::Idle, true),
            (ExecutionState::Stopping, false),
            (ExecutionState::Cancelled { interrupted: true }, false),
            (
                ExecutionState::Failed {
                    reason: String::new(),
                },
                false,
            ),
        ] {
            let expected = if queued {
                DeliveryState::Queued
            } else {
                DeliveryState::Blocked {
                    reason: BlockedReason::ExecutionUnavailable,
                }
            };
            assert_eq!(initial_delivery(&state), expected, "{state:?}");
        }
    }

    #[test]
    fn dispatch_refuses_another_objective_and_leaves_the_input_queued() {
        let (objective, _) = active_objective();
        let mut input = queued(&objective);
        let mut record = defined_objective().into_record();
        record.id = objective_id(9);
        record.branch = BranchName::for_objective(record.id);
        let other = Objective::restore(record).expect("restore");
        assert_eq!(
            input.dispatch(&other, false),
            Err(DomainError::InvalidRequest("input objective mismatch"))
        );
        assert_eq!(input.delivery, DeliveryState::Queued);
    }

    #[test]
    fn stale_association_is_reported_before_lifecycle_and_references() {
        let (mut objective, generation) = active_objective();
        let mut input = queued(&objective);
        objective.request_stop(ts(6)).expect("stop");
        objective
            .observe_stopped(generation, ts(6))
            .expect("stopped");
        objective
            .replace_session(
                session_id(2),
                ModelRef::new("p", "m").expect("model"),
                ts(7),
            )
            .expect("replace");
        assert_eq!(
            input.dispatch(&objective, true),
            Ok(DispatchDecision::Blocked {
                reason: BlockedReason::StaleAssociation
            })
        );
    }

    #[test]
    fn receipt_needs_a_dispatch_and_repeats_are_idempotent() {
        let (objective, generation) = active_objective();
        let mut input = queued(&objective);
        assert_eq!(
            input.mark_received(generation),
            Err(DomainError::Conflict("input was not dispatched"))
        );
        assert!(!input.mark_worker_lost(), "nothing was in flight");
        input.dispatch(&objective, false).expect("dispatch");
        input.mark_received(generation).expect("received");
        assert_eq!(input.mark_received(generation), Ok(()));
        assert!(!input.mark_worker_lost(), "received input is not in doubt");
        assert_eq!(input.delivery, DeliveryState::Received { generation });
        assert!(input.requeue().is_err());
    }

    #[test]
    fn codes_are_stable_wire_names() {
        let generation = ExecutionGeneration::new(1);
        let reason = BlockedReason::StaleReference;
        assert_eq!(
            [
                DeliveryState::Queued,
                DeliveryState::Dispatched { generation },
                DeliveryState::Received { generation },
                DeliveryState::Uncertain { generation },
                DeliveryState::Blocked { reason },
            ]
            .map(DeliveryState::code),
            ["queued", "dispatched", "received", "uncertain", "blocked"]
        );
        assert_eq!(
            [
                BlockedReason::ExecutionUnavailable,
                BlockedReason::StaleAssociation,
                BlockedReason::StaleReference,
                BlockedReason::InvalidLifecycle,
            ]
            .map(BlockedReason::code),
            [
                "execution_unavailable",
                "stale_association",
                "stale_reference",
                "invalid_lifecycle"
            ]
        );
    }
}
