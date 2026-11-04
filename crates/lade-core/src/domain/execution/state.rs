//! Execution generations, run states and the session association they are observed under.

use super::ModelRef;
use crate::domain::ids::{Revision, SessionId};
use crate::error::DomainError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExecutionGeneration(u64);

impl ExecutionGeneration {
    pub const NONE: Self = Self(0);

    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }

    pub(super) fn advance(self) -> Result<Self, DomainError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(DomainError::LimitReached(
                "The agent of this Objective can't be started again.",
            ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionState {
    None,
    Starting,
    Running,
    Idle,
    Stopping,
    Cancelled { interrupted: bool },
    Failed { reason: String },
}

impl ExecutionState {
    #[must_use]
    pub const fn is_live(&self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Running | Self::Idle | Self::Stopping
        )
    }

    #[must_use]
    pub const fn accepts_dispatch(&self) -> bool {
        matches!(self, Self::Running | Self::Idle)
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Idle => "idle",
            Self::Stopping => "stopping",
            Self::Cancelled { .. } => "cancelled",
            Self::Failed { .. } => "failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionAssociation {
    pub session_id: SessionId,
    pub association_revision: Revision,
    pub model: ModelRef,
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [ExecutionState; 7] = [
        ExecutionState::None,
        ExecutionState::Starting,
        ExecutionState::Running,
        ExecutionState::Idle,
        ExecutionState::Stopping,
        ExecutionState::Cancelled { interrupted: false },
        ExecutionState::Failed {
            reason: String::new(),
        },
    ];

    #[test]
    fn generations_advance_by_one_and_refuse_to_wrap() {
        assert_eq!(ExecutionGeneration::NONE.value(), 0);
        assert_eq!(
            ExecutionGeneration::NONE.advance(),
            Ok(ExecutionGeneration::new(1))
        );
        assert_eq!(
            ExecutionGeneration::new(41)
                .advance()
                .map(ExecutionGeneration::value),
            Ok(42)
        );
        assert!(matches!(
            ExecutionGeneration::new(u64::MAX).advance(),
            Err(DomainError::LimitReached(_))
        ));
        assert!(ExecutionGeneration::new(1) < ExecutionGeneration::new(2));
    }

    #[test]
    fn only_running_and_idle_runs_accept_dispatch_and_both_are_live() {
        for state in &ALL {
            let live = matches!(
                state,
                ExecutionState::Starting
                    | ExecutionState::Running
                    | ExecutionState::Idle
                    | ExecutionState::Stopping
            );
            assert_eq!(state.is_live(), live, "{state:?}");
            let dispatchable = matches!(state, ExecutionState::Running | ExecutionState::Idle);
            assert_eq!(state.accepts_dispatch(), dispatchable, "{state:?}");
            assert!(!state.accepts_dispatch() || state.is_live());
        }
        assert!(!ExecutionState::Cancelled { interrupted: true }.is_live());
    }

    #[test]
    fn state_codes_are_distinct_and_ignore_payloads() {
        let codes: Vec<&str> = ALL.iter().map(ExecutionState::code).collect();
        assert_eq!(
            codes,
            [
                "none",
                "starting",
                "running",
                "idle",
                "stopping",
                "cancelled",
                "failed"
            ]
        );
        assert_eq!(
            ExecutionState::Cancelled { interrupted: true }.code(),
            "cancelled"
        );
        assert_eq!(
            ExecutionState::Failed {
                reason: "boom".into()
            }
            .code(),
            "failed"
        );
    }
}
