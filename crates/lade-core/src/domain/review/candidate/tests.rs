use super::*;
use crate::domain::review::fixtures::{approve_cmd, approved, frozen, new_candidate};
use crate::domain::review::{ApprovalState, Evidence};
use crate::testing::{
    active_objective, candidate_id, defined_objective, git, stopped_objective, ts,
};

#[test]
fn freeze_requires_active_stopped_and_non_empty() {
    let (mut live, _) = active_objective();
    let mut review = Review::new(live.id());
    assert!(matches!(
        review.freeze(&mut live, new_candidate(1, &git(1)), ts(5)),
        Err(DomainError::InvalidLifecycle(_))
    ));
    let mut defined = defined_objective();
    let mut review = Review::new(defined.id());
    assert!(
        review
            .freeze(&mut defined, new_candidate(1, &git(1)), ts(5))
            .is_err()
    );
    let mut objective = stopped_objective();
    let mut review = Review::new(objective.id());
    let mut empty = new_candidate(1, &git(1));
    empty.changes.files = 0;
    assert!(matches!(
        review.freeze(&mut objective, empty, ts(5)),
        Err(DomainError::InvalidRequest(_))
    ));
    assert_eq!(objective.lifecycle(), Lifecycle::Active);
    let id = review
        .freeze(&mut objective, new_candidate(1, &git(1)), ts(5))
        .expect("freeze");
    assert_eq!(objective.lifecycle(), Lifecycle::Frozen);
    assert_eq!(
        review.current_candidate().map(|c| (c.id, c.ordinal)),
        Some((id, 1))
    );
}

#[test]
fn return_for_revision_invalidates_approval_and_keeps_history() {
    let (mut objective, mut review, _) = approved();
    assert_eq!(
        review.return_for_revision(&mut objective, candidate_id(9), ts(20)),
        Err(DomainError::NotFound("candidate"))
    );
    review
        .return_for_revision(&mut objective, candidate_id(1), ts(20))
        .expect("return");
    assert_eq!(objective.lifecycle(), Lifecycle::Active);
    assert_eq!(
        review.approvals()[0].state,
        ApprovalState::Invalidated {
            reason: InvalidationReason::ReturnedForRevision
        }
    );
    assert_eq!(review.candidates()[0].status, CandidateStatus::Historical);
    assert!(review.current_candidate().is_none());
    assert!(
        review
            .return_for_revision(&mut objective, candidate_id(1), ts(21))
            .is_err(),
        "not frozen"
    );
    review
        .freeze(&mut objective, new_candidate(2, &git(1)), ts(22))
        .expect("refreeze");
    assert_eq!(review.current_candidate().map(|c| c.ordinal), Some(2));
    assert_eq!(
        review.return_for_revision(&mut objective, candidate_id(1), ts(23)),
        Err(DomainError::StaleCandidate)
    );
}

#[test]
fn target_move_invalidates_and_refresh_builds_new_candidate() {
    let (mut objective, mut review, target) = approved();
    assert_eq!(
        review.observe_target(&mut objective, &target, ts(20)),
        Ok(false)
    );
    assert_eq!(objective.lifecycle(), Lifecycle::Approved);
    let moved = git(2);
    assert_eq!(
        review.observe_target(&mut objective, &moved, ts(21)),
        Ok(true)
    );
    assert_eq!(objective.lifecycle(), Lifecycle::Frozen);
    assert_eq!(
        review.approvals()[0].state,
        ApprovalState::Invalidated {
            reason: InvalidationReason::TargetMoved
        }
    );
    assert!(review.target_stale(&moved));
    assert_eq!(
        review.refresh(
            &mut objective,
            candidate_id(1),
            &target,
            RefreshOutcome::Rebuilt(new_candidate(2, &target)),
            ts(22)
        ),
        Err(DomainError::Conflict(
            "The Integration Target hasn't moved, so there is nothing to refresh."
        ))
    );
    assert_eq!(
        review.refresh(
            &mut objective,
            candidate_id(1),
            &moved,
            RefreshOutcome::Rebuilt(new_candidate(2, &target)),
            ts(22)
        ),
        Err(DomainError::StaleTarget)
    );
    let result = review
        .refresh(
            &mut objective,
            candidate_id(1),
            &moved,
            RefreshOutcome::Rebuilt(new_candidate(2, &moved)),
            ts(23),
        )
        .expect("refresh");
    assert_eq!(
        result,
        RefreshResult::Refreshed {
            candidate_id: candidate_id(2)
        }
    );
    assert_eq!(objective.lifecycle(), Lifecycle::Frozen);
    assert_eq!(review.candidates()[0].status, CandidateStatus::Historical);
    assert_eq!(
        review.evidence(candidate_id(2)),
        Evidence::Missing,
        "refreshed candidate needs checks again"
    );
    assert!(
        review
            .approve(
                &mut objective,
                approve_cmd(2, 2, &moved, None),
                &moved,
                ts(24)
            )
            .is_err()
    );
    assert_eq!(
        review.approve(
            &mut objective,
            approve_cmd(2, 1, &moved, Some("x")),
            &moved,
            ts(24)
        ),
        Err(DomainError::StaleCandidate)
    );
}

#[test]
fn refresh_from_approved_and_conflict_returns_for_revision() {
    let (mut objective, mut review, _) = approved();
    let moved = git(2);
    let result = review
        .refresh(
            &mut objective,
            candidate_id(1),
            &moved,
            RefreshOutcome::Conflict,
            ts(20),
        )
        .expect("refresh");
    assert_eq!(result, RefreshResult::Conflicted);
    assert_eq!(objective.lifecycle(), Lifecycle::Active);
    assert!(review.valid_approval().is_none());
    assert!(review.current_candidate().is_none());
    assert!(matches!(
        review.refresh(
            &mut objective,
            candidate_id(1),
            &moved,
            RefreshOutcome::Conflict,
            ts(21)
        ),
        Err(DomainError::InvalidLifecycle(_))
    ));
}

#[test]
fn unexpected_change_invalidates_candidate() {
    let (mut objective, mut review, target) = approved();
    review
        .invalidate_candidate(&mut objective, ts(20))
        .expect("invalidate");
    assert_eq!(objective.lifecycle(), Lifecycle::Frozen);
    assert_eq!(review.candidates()[0].status, CandidateStatus::Invalidated);
    assert_eq!(
        review.approvals()[0].state,
        ApprovalState::Invalidated {
            reason: InvalidationReason::CandidateChanged
        }
    );
    assert_eq!(
        review.approve(
            &mut objective,
            approve_cmd(2, 1, &target, Some("x")),
            &target,
            ts(21)
        ),
        Err(DomainError::StaleCandidate)
    );
    review
        .return_for_revision(&mut objective, candidate_id(1), ts(22))
        .expect("return");
    assert_eq!(review.candidates()[0].status, CandidateStatus::Historical);
}

#[test]
fn each_freeze_takes_the_next_ordinal_and_a_fresh_id() {
    let (mut objective, mut review, target) = frozen();
    review
        .return_for_revision(&mut objective, candidate_id(1), ts(11))
        .expect("return");
    assert_eq!(
        review.freeze(&mut objective, new_candidate(1, &target), ts(12)),
        Err(DomainError::Conflict("candidate id reused"))
    );
    assert_eq!(objective.lifecycle(), Lifecycle::Active, "nothing changed");
    review
        .freeze(&mut objective, new_candidate(2, &target), ts(12))
        .expect("refreeze");
    let summary: Vec<(u32, CandidateStatus)> = review
        .candidates()
        .iter()
        .map(|candidate| (candidate.ordinal, candidate.status))
        .collect();
    assert_eq!(
        summary,
        [
            (1, CandidateStatus::Historical),
            (2, CandidateStatus::Current)
        ]
    );
    let current = review.current_candidate().expect("current");
    assert_eq!(
        (current.objective_id, current.created_at),
        (objective.id(), ts(12))
    );
    assert_eq!(review.latest_candidate(), Some(current));
}

#[test]
fn ordinals_never_wrap() {
    let (objective, review, target) = frozen();
    let mut candidates = review.candidates().to_vec();
    candidates[0].ordinal = u32::MAX;
    candidates[0].status = CandidateStatus::Historical;
    let mut review =
        Review::restore(objective.id(), candidates, Vec::new(), Vec::new()).expect("restore");
    let mut active = stopped_objective();
    assert!(matches!(
        review.freeze(&mut active, new_candidate(2, &target), ts(12)),
        Err(DomainError::LimitReached(_))
    ));
    assert_eq!(active.lifecycle(), Lifecycle::Active);
}

#[test]
fn a_conflicted_refresh_of_a_frozen_candidate_returns_it_for_revision() {
    let (mut objective, mut review, _) = frozen();
    let moved = git(2);
    assert_eq!(
        review.refresh(
            &mut objective,
            candidate_id(9),
            &moved,
            RefreshOutcome::Conflict,
            ts(11)
        ),
        Err(DomainError::NotFound("candidate"))
    );
    assert_eq!(
        review.refresh(
            &mut objective,
            candidate_id(1),
            &moved,
            RefreshOutcome::Conflict,
            ts(11)
        ),
        Ok(RefreshResult::Conflicted)
    );
    assert_eq!(objective.lifecycle(), Lifecycle::Active);
    assert_eq!(review.candidates()[0].status, CandidateStatus::Historical);
    assert!(review.approvals().is_empty());
}

#[test]
fn a_rebuilt_refresh_must_contain_changes() {
    let (mut objective, mut review, _) = frozen();
    let moved = git(2);
    let mut empty = new_candidate(2, &moved);
    empty.changes = ChangeCounts::default();
    assert!(matches!(
        review.refresh(
            &mut objective,
            candidate_id(1),
            &moved,
            RefreshOutcome::Rebuilt(empty),
            ts(11)
        ),
        Err(DomainError::InvalidRequest(_))
    ));
    assert_eq!(
        review.current_candidate().map(|candidate| candidate.id),
        Some(candidate_id(1))
    );
    assert_eq!(objective.lifecycle(), Lifecycle::Frozen);
}

#[test]
fn candidate_changes_are_invalidated_only_on_a_frozen_or_approved_objective() {
    let (mut live, _) = active_objective();
    let mut review = Review::new(live.id());
    assert!(matches!(
        review.invalidate_candidate(&mut live, ts(5)),
        Err(DomainError::InvalidLifecycle(_))
    ));
    let (mut objective, mut review, _) = frozen();
    review
        .invalidate_candidate(&mut objective, ts(11))
        .expect("invalidate");
    assert_eq!(objective.lifecycle(), Lifecycle::Frozen);
    assert_eq!(
        review.invalidate_candidate(&mut objective, ts(12)),
        Err(DomainError::NotFound("candidate")),
        "nothing is current any more"
    );
    assert!(review.approvals().is_empty());
}

#[test]
fn status_codes_are_stable() {
    assert_eq!(
        [
            CandidateStatus::Current,
            CandidateStatus::Historical,
            CandidateStatus::Invalidated
        ]
        .map(CandidateStatus::code),
        ["current", "historical", "invalidated"]
    );
}
