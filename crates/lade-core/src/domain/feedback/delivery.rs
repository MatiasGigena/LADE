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
