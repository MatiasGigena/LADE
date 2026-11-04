//! Attention: how urgently an Objective needs the user, derived from its state and observed facts.

use super::{Lifecycle, Objective};
use crate::domain::execution::ExecutionState;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Attention {
    NeedsAttention,
    ReadyForReview,
    Working,
    Idle,
    Archived,
}

impl Attention {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NeedsAttention => "needs_attention",
            Self::ReadyForReview => "ready_for_review",
            Self::Working => "working",
            Self::Idle => "idle",
            Self::Archived => "archived",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Badge {
    Interrupted,
    Failed,
    DeliveryUncertain,
    StaleFeedback,
    VerificationFailed,
    IntegrationConflict,
    StaleTarget,
    Cancelled,
    CleanedUp,
}

impl Badge {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Interrupted => "interrupted",
            Self::Failed => "failed",
            Self::DeliveryUncertain => "delivery_uncertain",
            Self::StaleFeedback => "stale_feedback",
            Self::VerificationFailed => "verification_failed",
            Self::IntegrationConflict => "integration_conflict",
            Self::StaleTarget => "stale_target",
            Self::Cancelled => "cancelled",
            Self::CleanedUp => "cleaned_up",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AttentionFacts {
    pub delivery_uncertain: bool,
    pub stale_feedback: bool,
    pub verification_failed: bool,
    pub integration_conflict: bool,
    pub target_moved: bool,
    pub change_set_non_empty: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttentionView {
    pub attention: Attention,
    pub badges: Vec<Badge>,
}

#[must_use]
pub fn derive_attention(objective: &Objective, facts: &AttentionFacts) -> AttentionView {
    let record = objective.record();
    if record.lifecycle.is_terminal() {
        let badges = if record.cleaned_up {
            vec![Badge::CleanedUp]
        } else {
            Vec::new()
        };
        return AttentionView {
            attention: Attention::Archived,
            badges,
        };
    }
    let state = record.execution.state();
    let candidates = [
        (
            matches!(state, ExecutionState::Cancelled { interrupted: true }),
            Badge::Interrupted,
        ),
        (
            matches!(state, ExecutionState::Failed { .. }),
            Badge::Failed,
        ),
        (facts.delivery_uncertain, Badge::DeliveryUncertain),
        (facts.stale_feedback, Badge::StaleFeedback),
        (facts.verification_failed, Badge::VerificationFailed),
        (facts.integration_conflict, Badge::IntegrationConflict),
        (facts.target_moved, Badge::StaleTarget),
        (
            matches!(state, ExecutionState::Cancelled { interrupted: false }),
            Badge::Cancelled,
        ),
    ];
    let badges: Vec<Badge> = candidates
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, badge)| *badge)
        .collect();
    let attention = if badges.iter().any(|badge| *badge != Badge::Cancelled) {
        Attention::NeedsAttention
    } else if matches!(record.lifecycle, Lifecycle::Frozen | Lifecycle::Approved)
        || (*state == ExecutionState::Idle && facts.change_set_non_empty)
    {
        Attention::ReadyForReview
    } else if matches!(
        state,
        ExecutionState::Starting | ExecutionState::Running | ExecutionState::Stopping
    ) {
        Attention::Working
    } else {
        Attention::Idle
    };
    AttentionView { attention, badges }
}
