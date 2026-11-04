use super::*;
use crate::domain::ids::GitId;
use crate::domain::objectives::{Lifecycle, Objective};
use crate::error::DomainError;

use crate::testing::{
    active_objective, approval_id, candidate_id, defined_objective, digest, git, stopped_objective,
    ts, verification_id,
};

fn new_candidate(id: u128, base: &GitId) -> NewCandidate {
    NewCandidate {
        id: candidate_id(id),
        commit: git(100 + id),
        tree: git(200 + id),
        base_target_commit: base.clone(),
        manifest_digest: digest(id),
        changes: ChangeCounts {
            files: 2,
            added: 10,
            removed: 3,
        },
    }
}

fn frozen() -> (Objective, Review, GitId) {
    let mut objective = stopped_objective();
    let mut review = Review::new(objective.id());
    let target = git(1);
    review
        .freeze(&mut objective, new_candidate(1, &target), ts(10))
        .expect("freeze");
    (objective, review, target)
}

fn verify(
    review: &mut Review,
    objective: &Objective,
    id: u128,
    argv: &[&str],
    end: VerificationEnd,
) {
    let candidate = review.current_candidate().expect("current").id;
    let command = NewVerification {
        id: verification_id(id),
        candidate_id: candidate,
        argv: argv.iter().map(ToString::to_string).collect(),
        cwd_label: "verification copy".into(),
    };
    review
        .start_verification(objective, command, ts(11))
        .expect("start");
    review
        .finish_verification(verification_id(id), end, Some(digest(50)), false, ts(12))
        .expect("finish");
}

fn approve_cmd(
    id: u128,
    candidate: u128,
    target: &GitId,
    reason: Option<&str>,
) -> ApproveCandidate {
    ApproveCandidate {
        approval_id: approval_id(id),
        candidate_id: candidate_id(candidate),
        target_commit: target.clone(),
        override_reason: reason.map(ToString::to_string),
    }
}

fn approved() -> (Objective, Review, GitId) {
    let (mut objective, mut review, target) = frozen();
    verify(
        &mut review,
        &objective,
        1,
        &["cargo", "test"],
        VerificationEnd::Exited { code: 0 },
    );
    review
        .approve(
            &mut objective,
            approve_cmd(1, 1, &target, None),
            &target,
            ts(13),
        )
        .expect("approve");
    (objective, review, target)
}

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
fn approval_requires_passing_latest_checks_or_override() {
    let (mut objective, mut review, target) = frozen();
    assert_eq!(review.evidence(candidate_id(1)), Evidence::Missing);
    assert!(matches!(
        review.approve(
            &mut objective,
            approve_cmd(1, 1, &target, None),
            &target,
            ts(13)
        ),
        Err(DomainError::InvalidRequest(_))
    ));
    assert!(matches!(
        review.approve(
            &mut objective,
            approve_cmd(1, 1, &target, Some("   ")),
            &target,
            ts(13)
        ),
        Err(DomainError::InvalidRequest(_))
    ));
    verify(
        &mut review,
        &objective,
        1,
        &["cargo", "test"],
        VerificationEnd::Exited { code: 1 },
    );
    verify(
        &mut review,
        &objective,
        2,
        &["git", "diff", "--check"],
        VerificationEnd::Exited { code: 0 },
    );
    assert_eq!(review.evidence(candidate_id(1)), Evidence::Failed);
    assert!(review.verification_failed());
    assert!(
        review
            .approve(
                &mut objective,
                approve_cmd(1, 1, &target, None),
                &target,
                ts(13)
            )
            .is_err()
    );
    verify(
        &mut review,
        &objective,
        3,
        &["cargo", "test"],
        VerificationEnd::Exited { code: 0 },
    );
    assert_eq!(review.latest_verifications(candidate_id(1)).len(), 2);
    assert_eq!(review.evidence(candidate_id(1)), Evidence::Passed);
    review
        .approve(
            &mut objective,
            approve_cmd(1, 1, &target, None),
            &target,
            ts(14),
        )
        .expect("approve");
    assert_eq!(objective.lifecycle(), Lifecycle::Approved);
    assert_eq!(
        review.valid_approval().map(|a| a.override_reason.clone()),
        Some(None)
    );
}

#[test]
fn override_reason_permits_failed_or_missing_checks() {
    let (mut objective, mut review, target) = frozen();
    verify(
        &mut review,
        &objective,
        1,
        &["make"],
        VerificationEnd::Interrupted,
    );
    review
        .approve(
            &mut objective,
            approve_cmd(1, 1, &target, Some("flaky network test")),
            &target,
            ts(13),
        )
        .expect("override");
    assert_eq!(
        review
            .valid_approval()
            .and_then(|a| a.override_reason.as_deref()),
        Some("flaky network test")
    );
}

#[test]
fn approval_rejects_running_checks_stale_candidate_and_target() {
    let (mut objective, mut review, target) = frozen();
    let command = NewVerification {
        id: verification_id(1),
        candidate_id: candidate_id(1),
        argv: vec!["cargo".into()],
        cwd_label: String::new(),
    };
    review
        .start_verification(&objective, command, ts(11))
        .expect("start");
    assert_eq!(
        review.approve(
            &mut objective,
            approve_cmd(1, 1, &target, Some("x")),
            &target,
            ts(12)
        ),
        Err(DomainError::Conflict(
            "Wait for the running check to finish."
        ))
    );
    review
        .finish_verification(
            verification_id(1),
            VerificationEnd::Exited { code: 0 },
            None,
            false,
            ts(12),
        )
        .expect("finish");
    assert!(
        review
            .finish_verification(
                verification_id(1),
                VerificationEnd::Error,
                None,
                false,
                ts(12)
            )
            .is_err()
    );
    let moved = git(2);
    assert_eq!(
        review.approve(
            &mut objective,
            approve_cmd(1, 1, &target, None),
            &moved,
            ts(13)
        ),
        Err(DomainError::StaleTarget)
    );
    assert_eq!(
        review.approve(
            &mut objective,
            approve_cmd(1, 1, &moved, None),
            &moved,
            ts(13)
        ),
        Err(DomainError::StaleTarget)
    );
    assert_eq!(
        review.approve(
            &mut objective,
            approve_cmd(1, 9, &target, None),
            &target,
            ts(13)
        ),
        Err(DomainError::NotFound("candidate"))
    );
    assert_eq!(objective.lifecycle(), Lifecycle::Frozen);
}

#[test]
fn a_newer_passing_run_does_not_hide_an_older_running_check() {
    let (mut objective, mut review, target) = frozen();
    review
        .start_verification(
            &objective,
            NewVerification {
                id: verification_id(1),
                candidate_id: candidate_id(1),
                argv: vec!["cargo".into(), "test".into()],
                cwd_label: "verification copy".into(),
            },
            ts(11),
        )
        .expect("first check starts");
    verify(
        &mut review,
        &objective,
        2,
        &["cargo", "test"],
        VerificationEnd::Exited { code: 0 },
    );
    assert_eq!(review.evidence(candidate_id(1)), Evidence::Running);
    let before = objective.clone();
    assert_eq!(
        review.approve(
            &mut objective,
            approve_cmd(1, 1, &target, Some("override")),
            &target,
            ts(13),
        ),
        Err(DomainError::Conflict(
            "Wait for the running check to finish."
        ))
    );
    assert_eq!(objective, before);
    assert!(review.approvals().is_empty());
    review
        .finish_verification(
            verification_id(1),
            VerificationEnd::Exited { code: 1 },
            None,
            false,
            ts(14),
        )
        .expect("older check finishes");
    assert_eq!(review.evidence(candidate_id(1)), Evidence::Passed);
    review
        .approve(
            &mut objective,
            approve_cmd(1, 1, &target, None),
            &target,
            ts(15),
        )
        .expect("latest completed run permits approval");
}

#[test]
fn approval_requires_frozen_lifecycle() {
    let (mut objective, mut review, target) = approved();
    assert!(matches!(
        review.approve(
            &mut objective,
            approve_cmd(2, 1, &target, Some("x")),
            &target,
            ts(20)
        ),
        Err(DomainError::InvalidLifecycle(_))
    ));
    assert!(matches!(
        review.start_verification(
            &objective,
            NewVerification {
                id: verification_id(9),
                candidate_id: candidate_id(1),
                argv: vec!["x".into()],
                cwd_label: String::new()
            },
            ts(20)
        ),
        Err(DomainError::InvalidLifecycle(_))
    ));
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
fn integration_requires_valid_approval_and_unchanged_target() {
    let (mut objective, mut review, target) = approved();
    assert_eq!(
        review.check_integration(&objective, approval_id(9), &target),
        Err(DomainError::NotFound("approval"))
    );
    assert_eq!(
        review.check_integration(&objective, approval_id(1), &git(2)),
        Err(DomainError::StaleTarget)
    );
    let plan = review
        .check_integration(&objective, approval_id(1), &target)
        .expect("plan");
    assert_eq!(plan.candidate_commit, git(101));
    assert_eq!(plan.expected_target, target);
    review
        .complete_integration(&mut objective, &plan, ts(30))
        .expect("integrated");
    assert_eq!(objective.lifecycle(), Lifecycle::Integrated);
    assert_eq!(review.approvals()[0].state, ApprovalState::Consumed);
    assert!(
        review
            .complete_integration(&mut objective, &plan, ts(31))
            .is_err(),
        "consumed once"
    );
    assert!(review.discard(&mut objective, ts(32)).is_err(), "terminal");
}

#[test]
fn integration_rejects_frozen_and_invalidated() {
    let (mut objective, mut review, target) = approved();
    assert_eq!(
        review.invalidate_approval(&mut objective, InvalidationReason::CandidateChanged, ts(20)),
        Ok(true)
    );
    assert_eq!(
        review.invalidate_approval(&mut objective, InvalidationReason::CandidateChanged, ts(20)),
        Ok(false)
    );
    assert!(matches!(
        review.check_integration(&objective, approval_id(1), &target),
        Err(DomainError::InvalidLifecycle(_))
    ));
    review
        .approve(
            &mut objective,
            approve_cmd(2, 1, &target, None),
            &target,
            ts(21),
        )
        .expect("re-approve");
    assert_eq!(
        review.check_integration(&objective, approval_id(1), &target),
        Err(DomainError::Conflict(
            "The approval is no longer valid. Approve again."
        ))
    );
    assert!(
        review
            .check_integration(&objective, approval_id(2), &target)
            .is_ok()
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
fn restart_expires_approval_and_interrupts_checks() {
    let (mut objective, mut review, target) = approved();
    review
        .recover_after_restart(&mut objective, ts(40))
        .expect("recover");
    assert_eq!(objective.lifecycle(), Lifecycle::Frozen);
    assert_eq!(
        review.approvals()[0].state,
        ApprovalState::Invalidated {
            reason: InvalidationReason::ServiceRestart
        }
    );
    let command = NewVerification {
        id: verification_id(5),
        candidate_id: candidate_id(1),
        argv: vec!["make".into()],
        cwd_label: String::new(),
    };
    review
        .start_verification(&objective, command, ts(41))
        .expect("start");
    assert!(
        review.discard(&mut objective, ts(41)).is_err(),
        "running verification"
    );
    review
        .recover_after_restart(&mut objective, ts(42))
        .expect("recover");
    let last = review.verifications().last().expect("verification");
    assert_eq!(
        (last.outcome, last.ended_at),
        (VerificationOutcome::Interrupted, Some(ts(42)))
    );
    assert!(
        review
            .check_integration(&objective, approval_id(1), &target)
            .is_err()
    );
}

#[test]
fn discard_and_complete_without_changes() {
    let (mut objective, mut review, _) = approved();
    review
        .discard(&mut objective, ts(20))
        .expect("discard approved");
    assert_eq!(objective.lifecycle(), Lifecycle::Discarded);
    assert_eq!(
        review.approvals()[0].state,
        ApprovalState::Invalidated {
            reason: InvalidationReason::Discarded
        }
    );

    let mut defined = defined_objective();
    let mut review = Review::new(defined.id());
    assert_eq!(
        review.complete_without_changes(&mut defined, false, ts(2)),
        Err(DomainError::Conflict(
            "There are changes. Freeze them for review, or discard the Objective."
        ))
    );
    review
        .complete_without_changes(&mut defined, true, ts(2))
        .expect("complete");
    assert_eq!(defined.lifecycle(), Lifecycle::CompletedNoChanges);

    let (mut approved_objective, mut approved_review, _) = approved();
    assert!(matches!(
        approved_review.complete_without_changes(&mut approved_objective, true, ts(20)),
        Err(DomainError::InvalidLifecycle(_))
    ));
}

#[test]
fn review_rejects_foreign_objective_and_restores_consistently() {
    let (objective, review, _) = approved();
    let mut other = defined_objective();
    let mut foreign = Review::new(crate::testing::objective_id(77));
    assert!(matches!(
        foreign.discard(&mut other, ts(5)),
        Err(DomainError::InvalidRequest(_))
    ));
    let restored = Review::restore(
        objective.id(),
        review.candidates().to_vec(),
        review.verifications().to_vec(),
        review.approvals().to_vec(),
    )
    .expect("restore");
    assert_eq!(restored, review);
    let mut duplicated = review.candidates().to_vec();
    duplicated.push(review.candidates()[0].clone());
    assert!(Review::restore(objective.id(), duplicated, Vec::new(), Vec::new()).is_err());
}
