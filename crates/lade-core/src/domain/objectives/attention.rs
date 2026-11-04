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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{active_objective, defined_objective, stopped_objective, ts};

    #[test]
    fn attention_precedence_and_badges() {
        let defined = defined_objective();
        assert_eq!(
            derive_attention(&defined, &AttentionFacts::default()).attention,
            Attention::Idle
        );

        let (mut objective, generation) = active_objective();
        assert_eq!(
            derive_attention(&objective, &AttentionFacts::default()).attention,
            Attention::Idle
        );
        let changed = AttentionFacts {
            change_set_non_empty: true,
            ..AttentionFacts::default()
        };
        assert_eq!(
            derive_attention(&objective, &changed).attention,
            Attention::ReadyForReview
        );
        let uncertain = AttentionFacts {
            delivery_uncertain: true,
            ..changed
        };
        let view = derive_attention(&objective, &uncertain);
        assert_eq!(view.attention, Attention::NeedsAttention);
        assert_eq!(view.badges, vec![Badge::DeliveryUncertain]);

        objective
            .observe_turn(generation, true, ts(5))
            .expect("running");
        assert_eq!(
            derive_attention(&objective, &changed).attention,
            Attention::Working
        );
        objective.request_stop(ts(6)).expect("stop");
        assert_eq!(
            derive_attention(&objective, &changed).attention,
            Attention::Working
        );
        objective
            .observe_stopped(generation, ts(7))
            .expect("stopped");
        let view = derive_attention(&objective, &changed);
        assert_eq!(
            (view.attention, view.badges),
            (Attention::Idle, vec![Badge::Cancelled])
        );

        objective
            .transition(&[Lifecycle::Frozen], ts(8))
            .expect("freeze");
        assert_eq!(
            derive_attention(&objective, &AttentionFacts::default()).attention,
            Attention::ReadyForReview
        );
        let all = AttentionFacts {
            delivery_uncertain: true,
            stale_feedback: true,
            verification_failed: true,
            integration_conflict: true,
            target_moved: true,
            change_set_non_empty: true,
        };
        let view = derive_attention(&objective, &all);
        assert_eq!(view.attention, Attention::NeedsAttention);
        assert_eq!(
            view.badges,
            vec![
                Badge::DeliveryUncertain,
                Badge::StaleFeedback,
                Badge::VerificationFailed,
                Badge::IntegrationConflict,
                Badge::StaleTarget,
                Badge::Cancelled
            ]
        );

        objective
            .transition(&[Lifecycle::Discarded], ts(9))
            .expect("discard");
        assert_eq!(
            derive_attention(&objective, &all),
            AttentionView {
                attention: Attention::Archived,
                badges: vec![]
            }
        );
    }

    #[test]
    fn failed_execution_needs_attention() {
        let (mut objective, generation) = active_objective();
        objective
            .observe_failure(generation, "worker exited", ts(5))
            .expect("fail");
        let view = derive_attention(&objective, &AttentionFacts::default());
        assert_eq!(
            (view.attention, view.badges),
            (Attention::NeedsAttention, vec![Badge::Failed])
        );
    }

    #[test]
    fn board_order_sorts_by_precedence() {
        let mut board = vec![
            Attention::Archived,
            Attention::Idle,
            Attention::Working,
            Attention::ReadyForReview,
            Attention::NeedsAttention,
        ];
        board.sort();
        assert_eq!(
            board,
            vec![
                Attention::NeedsAttention,
                Attention::ReadyForReview,
                Attention::Working,
                Attention::Idle,
                Attention::Archived
            ]
        );
        assert_eq!(Attention::NeedsAttention.code(), "needs_attention");
    }

    #[test]
    fn closed_objectives_are_archived_whatever_the_facts_and_show_only_clean_up() {
        let all = AttentionFacts {
            delivery_uncertain: true,
            stale_feedback: true,
            verification_failed: true,
            integration_conflict: true,
            target_moved: true,
            change_set_non_empty: true,
        };
        let mut objective = defined_objective();
        objective
            .transition(&[Lifecycle::CompletedNoChanges], ts(2))
            .expect("complete");
        objective.cleanup(0, false, ts(3)).expect("clean up");
        assert_eq!(
            derive_attention(&objective, &all),
            AttentionView {
                attention: Attention::Archived,
                badges: vec![Badge::CleanedUp]
            }
        );
    }

    #[test]
    fn a_failure_or_interruption_comes_before_observed_facts() {
        let (mut objective, generation) = active_objective();
        objective.interrupt_after_restart(ts(3)).expect("interrupt");
        let facts = AttentionFacts {
            stale_feedback: true,
            ..AttentionFacts::default()
        };
        assert_eq!(
            derive_attention(&objective, &facts).badges,
            [Badge::Interrupted, Badge::StaleFeedback]
        );
        let next = objective.resume_execution(ts(4)).expect("resume");
        assert_ne!(next, generation);
        objective
            .observe_failure(next, "worker exited", ts(5))
            .expect("fail");
        assert_eq!(
            derive_attention(&objective, &facts).badges,
            [Badge::Failed, Badge::StaleFeedback]
        );
    }

    #[test]
    fn changes_are_ready_for_review_only_once_the_agent_is_idle() {
        let changed = AttentionFacts {
            change_set_non_empty: true,
            ..AttentionFacts::default()
        };
        let stopped = stopped_objective();
        let view = derive_attention(&stopped, &changed);
        assert_eq!(
            (view.attention, view.badges),
            (Attention::Idle, vec![Badge::Cancelled]),
            "a stopped agent with changes waits for freezing"
        );
        let mut starting = stopped;
        starting.resume_execution(ts(4)).expect("resume");
        assert_eq!(
            derive_attention(&starting, &changed).attention,
            Attention::Working
        );
    }

    #[test]
    fn attention_and_badge_codes_are_distinct() {
        let attention = [
            Attention::NeedsAttention,
            Attention::ReadyForReview,
            Attention::Working,
            Attention::Idle,
            Attention::Archived,
        ]
        .map(Attention::code);
        let badges = [
            Badge::Interrupted,
            Badge::Failed,
            Badge::DeliveryUncertain,
            Badge::StaleFeedback,
            Badge::VerificationFailed,
            Badge::IntegrationConflict,
            Badge::StaleTarget,
            Badge::Cancelled,
            Badge::CleanedUp,
        ]
        .map(Badge::code);
        for codes in [&attention[..], &badges[..]] {
            for (index, code) in codes.iter().enumerate() {
                assert!(!codes[..index].contains(code), "{code} repeated");
                assert!(code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'));
            }
        }
        assert_eq!(Badge::StaleTarget.code(), "stale_target");
    }
}
