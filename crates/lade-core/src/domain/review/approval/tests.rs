use super::*;
use crate::domain::review::fixtures::{approve_cmd, approved, check, frozen, verify};
use crate::domain::review::{CandidateStatus, VerificationEnd};
use crate::testing::{approval_id, candidate_id, git, ts, verification_id};

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
    let command = check(1, &["cargo"]);
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
        review.start_verification(&objective, check(9, &["x"]), ts(20)),
        Err(DomainError::InvalidLifecycle(_))
    ));
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
fn an_approval_id_is_used_once_and_an_override_reason_is_bounded() {
    let (mut objective, mut review, target) = approved();
    review
        .invalidate_approval(&mut objective, InvalidationReason::TargetMoved, ts(20))
        .expect("invalidate");
    assert_eq!(
        review.approve(
            &mut objective,
            approve_cmd(1, 1, &target, None),
            &target,
            ts(21)
        ),
        Err(DomainError::Conflict("approval id reused"))
    );
    let long = "r".repeat(MAX_TEXT_BYTES + 1);
    assert_eq!(
        review.approve(
            &mut objective,
            approve_cmd(2, 1, &target, Some(&long)),
            &target,
            ts(21)
        ),
        Err(DomainError::LimitReached(
            "The override reason is too long."
        ))
    );
    assert_eq!(objective.lifecycle(), Lifecycle::Frozen);
    assert_eq!(review.approvals().len(), 1);
}

#[test]
fn a_blank_override_is_not_recorded_when_checks_passed() {
    let (mut objective, mut review, target) = frozen();
    verify(
        &mut review,
        &objective,
        1,
        &["make"],
        VerificationEnd::Exited { code: 0 },
    );
    review
        .approve(
            &mut objective,
            approve_cmd(1, 1, &target, Some(" \t")),
            &target,
            ts(13),
        )
        .expect("approve");
    let approval = review.valid_approval().expect("valid");
    assert_eq!(approval.override_reason, None);
    assert_eq!(
        (
            approval.candidate_id,
            &approval.target_commit,
            approval.approved_at
        ),
        (candidate_id(1), &target, ts(13))
    );
}

#[test]
fn invalidating_without_a_valid_approval_changes_nothing() {
    let (mut objective, mut review, target) = frozen();
    let revision = objective.revision();
    assert_eq!(
        review.invalidate_approval(&mut objective, InvalidationReason::ServiceRestart, ts(20)),
        Ok(false)
    );
    assert_eq!(
        review.observe_target(&mut objective, &git(2), ts(20)),
        Ok(true)
    );
    assert_eq!(objective.revision(), revision);
    assert_eq!(objective.lifecycle(), Lifecycle::Frozen);
    assert!(!review.target_stale(&target));
}

#[test]
fn a_plan_that_no_longer_matches_the_approval_cannot_complete() {
    let (mut objective, mut review, target) = approved();
    let plan = review
        .check_integration(&objective, approval_id(1), &target)
        .expect("plan");
    let mut tampered = plan.clone();
    tampered.candidate_tree = git(999);
    assert_eq!(
        review.complete_integration(&mut objective, &tampered, ts(30)),
        Err(DomainError::StaleCandidate)
    );
    let mut moved = plan.clone();
    moved.expected_target = git(2);
    assert_eq!(
        review.complete_integration(&mut objective, &moved, ts(30)),
        Err(DomainError::StaleTarget)
    );
    assert_eq!(objective.lifecycle(), Lifecycle::Approved);
    assert_eq!(review.valid_approval().map(|a| a.id), Some(approval_id(1)));
    assert_eq!(
        (plan.candidate_id, plan.candidate_tree, plan.approval_id),
        (candidate_id(1), git(201), approval_id(1))
    );
}

#[test]
fn approval_names_the_current_candidate_only() {
    let (mut objective, mut review, target) = frozen();
    review
        .start_verification(&objective, check(1, &["make"]), ts(11))
        .expect("start");
    review
        .finish_verification(
            verification_id(1),
            VerificationEnd::Exited { code: 0 },
            None,
            false,
            ts(12),
        )
        .expect("finish");
    review
        .invalidate_candidate(&mut objective, ts(13))
        .expect("invalidate");
    assert_eq!(review.candidates()[0].status, CandidateStatus::Invalidated);
    assert_eq!(
        review.approve(
            &mut objective,
            approve_cmd(1, 1, &target, None),
            &target,
            ts(14)
        ),
        Err(DomainError::StaleCandidate)
    );
}

#[test]
fn invalidation_codes_are_stable() {
    assert_eq!(
        [
            InvalidationReason::ReturnedForRevision,
            InvalidationReason::TargetMoved,
            InvalidationReason::CandidateChanged,
            InvalidationReason::ServiceRestart,
            InvalidationReason::Discarded,
        ]
        .map(InvalidationReason::code),
        [
            "returned_for_revision",
            "target_moved",
            "candidate_changed",
            "service_restart",
            "discarded"
        ]
    );
}
