//! Shared fixtures for the review tests: candidates, a frozen and an approved Objective, finished
//! checks and approval commands.

use super::{
    ApproveCandidate, ChangeCounts, NewCandidate, NewVerification, Review, VerificationEnd,
};
use crate::domain::ids::GitId;
use crate::domain::objectives::Objective;
use crate::testing::{
    approval_id, candidate_id, digest, git, stopped_objective, ts, verification_id,
};

pub(super) fn new_candidate(id: u128, base: &GitId) -> NewCandidate {
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

pub(super) fn frozen() -> (Objective, Review, GitId) {
    let mut objective = stopped_objective();
    let mut review = Review::new(objective.id());
    let target = git(1);
    review
        .freeze(&mut objective, new_candidate(1, &target), ts(10))
        .expect("freeze");
    (objective, review, target)
}

pub(super) fn verify(
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

pub(super) fn approve_cmd(
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

pub(super) fn approved() -> (Objective, Review, GitId) {
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

/// A check of candidate 1 running `argv`.
pub(super) fn check(id: u128, argv: &[&str]) -> NewVerification {
    NewVerification {
        id: verification_id(id),
        candidate_id: candidate_id(1),
        argv: argv.iter().map(ToString::to_string).collect(),
        cwd_label: "verification copy".into(),
    }
}
