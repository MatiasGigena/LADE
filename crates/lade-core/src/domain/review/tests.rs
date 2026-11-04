use super::fixtures::{approved, check, frozen};
use super::*;
use crate::domain::objectives::Lifecycle;
use crate::error::DomainError;
use crate::testing::{approval_id, candidate_id, defined_objective, objective_id, ts};

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
    let command = check(5, &["make"]);
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
    let mut foreign = Review::new(objective_id(77));
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

#[test]
fn restore_rejects_rows_that_cannot_come_from_one_review() {
    let (objective, review, _) = approved();
    let id = objective.id();
    let candidates = review.candidates().to_vec();
    let restore = |candidates: Vec<Candidate>, verifications, approvals| {
        Review::restore(id, candidates, verifications, approvals)
    };
    assert!(restore(Vec::new(), Vec::new(), Vec::new()).is_ok());

    let mut second = candidates[0].clone();
    second.id = candidate_id(2);
    second.ordinal = 2;
    let two_current = vec![candidates[0].clone(), second.clone()];
    assert!(restore(two_current, Vec::new(), Vec::new()).is_err());
    second.status = CandidateStatus::Historical;
    let out_of_order = vec![second.clone(), candidates[0].clone()];
    assert!(restore(out_of_order, Vec::new(), Vec::new()).is_err());

    let mut foreign = candidates.clone();
    foreign[0].objective_id = objective_id(9);
    assert!(restore(foreign, Vec::new(), Vec::new()).is_err());

    let orphan_check = review.verifications().to_vec();
    assert!(restore(Vec::new(), orphan_check, Vec::new()).is_err());
    let orphan_approval = review.approvals().to_vec();
    assert!(restore(Vec::new(), Vec::new(), orphan_approval).is_err());

    let mut two_valid = review.approvals().to_vec();
    two_valid.push(Approval {
        id: approval_id(2),
        ..two_valid[0].clone()
    });
    assert_eq!(
        restore(candidates, Vec::new(), two_valid),
        Err(DomainError::DurabilityFailure("inconsistent stored review"))
    );
}

#[test]
fn closing_without_changes_waits_for_running_checks() {
    let (mut objective, mut review, _) = frozen();
    review
        .start_verification(&objective, check(1, &["make"]), ts(11))
        .expect("start");
    assert_eq!(
        review.complete_without_changes(&mut objective, true, ts(12)),
        Err(DomainError::Conflict(
            "Wait for the running check to finish."
        ))
    );
    assert_eq!(objective.lifecycle(), Lifecycle::Frozen);
    assert_eq!(review.objective_id(), objective.id());
}
