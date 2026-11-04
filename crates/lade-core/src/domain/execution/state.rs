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
