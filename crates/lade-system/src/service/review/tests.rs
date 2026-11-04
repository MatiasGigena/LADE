use std::collections::VecDeque;
use std::fs;

use lade_protocol::client::{
    CandidateSummary, ObjectiveCleanupCommand, ObjectiveHistoryDeleteCommand, ObjectiveSelector,
    Request, ReviewIntegrateCommand,
};

use super::*;
use crate::service::adapters_support::{commit_all, git};
use crate::service::limits::Pressure;
use crate::service::test_harness::Harness;
use crate::storage::JournalState;

fn lifecycle(harness: &Harness, oid: ObjectiveId) -> Lifecycle {
    harness.service.objectives[&oid].objective().lifecycle()
}

fn approval_id(reply: Result<ReplyData, Rejection>) -> Id {
    match reply {
        Ok(ReplyData::ReviewApprove(approval)) => approval.approval_id,
        other => panic!("approve failed: {other:?}"),
    }
}

fn integrate(id: &Id, approval: &Id) -> Request {
    Request::ReviewIntegrate(ReviewIntegrateCommand {
        objective_id: id.clone(),
        approval_id: approval.clone(),
    })
}

#[test]
fn a_frozen_approved_candidate_integrates_onto_the_target_and_journals_it() {
    let mut harness = Harness::new("review-integrate");
    let (id, oid, candidate) = harness.frozen("Integrate", "feature.txt");
    assert_eq!(candidate.ordinal.value(), 1);
    assert_eq!(candidate.files.value(), 1);
    assert_eq!(lifecycle(&harness, oid), Lifecycle::Frozen);
    let before = harness.main_head();
    let approval = approval_id(harness.approve(&id, &candidate));
    assert_eq!(lifecycle(&harness, oid), Lifecycle::Approved);

    let request = integrate(&id, &approval);
    assert!(matches!(
        harness.call(request),
        Ok(ReplyData::ReviewIntegrate(_))
    ));
    assert_eq!(lifecycle(&harness, oid), Lifecycle::Integrated);
    let after = harness.main_head();
    assert_ne!(after, before);
    let message = git(&harness.repo, &["log", "-1", "--format=%B", "main"]);
    assert!(message.starts_with("Integrate\n"), "{message}");
    assert!(message.contains(&format!("objective {oid}")));
    assert_eq!(
        git(&harness.repo, &["show", "main:feature.txt"]),
        "Integrate"
    );
    let state: String = harness
        .service
        .store
        .conn()
        .query_row("SELECT state FROM integration_journal", [], |r| r.get(0))
        .expect("journal");
    assert_eq!(state, JournalState::Done.as_str());
    assert!(
        storage::open_journal(harness.service.store.conn())
            .expect("open")
            .is_empty()
    );
    assert!(harness.service.integrations.queued.is_empty());
}

#[test]
fn integrating_after_the_target_moved_invalidates_the_approval() {
    let mut harness = Harness::new("review-target-moved");
    let (id, oid, candidate) = harness.frozen("Moved", "moved.txt");
    let approval = approval_id(harness.approve(&id, &candidate));
    fs::write(harness.repo.join("other.txt"), "other\n").expect("write");
    commit_all(&harness.repo, "target moves");
    let request = integrate(&id, &approval);
    let refused = harness.call(request).expect_err("stale target");
    assert_eq!(refused.code, ErrorCode::StaleTarget);
    let st = &harness.service.objectives[&oid];
    assert!(st.review.valid_approval().is_none());
    assert_eq!(lifecycle(&harness, oid), Lifecycle::Frozen);
}

#[test]
fn a_second_integration_onto_the_same_target_waits_for_the_first() {
    let mut harness = Harness::new("review-queue");
    let (first, first_oid, first_candidate) = harness.frozen("First", "first.txt");
    let (second, second_oid, second_candidate) = harness.frozen("Second", "second.txt");
    let first_approval = approval_id(harness.approve(&first, &first_candidate));
    let second_approval = approval_id(harness.approve(&second, &second_candidate));

    let first_ctx = Harness::ctx(lade_protocol::client::RequestName::ReviewIntegrate);
    let first_id = first_ctx.request_id.clone();
    let second_ctx = Harness::ctx(lade_protocol::client::RequestName::ReviewIntegrate);
    let second_id = second_ctx.request_id.clone();
    let first_cmd = ReviewIntegrateCommand {
        objective_id: first.clone(),
        approval_id: first_approval,
    };
    let second_cmd = ReviewIntegrateCommand {
        objective_id: second.clone(),
        approval_id: second_approval,
    };
    assert!(matches!(
        harness.service.review_integrate(first_ctx, &first_cmd),
        Flow::Deferred
    ));
    assert!(matches!(
        harness.service.review_integrate(second_ctx, &second_cmd),
        Flow::Deferred
    ));
    assert_eq!(harness.service.rt(second_oid).busy, Some("integrate"));
    let queued: usize = harness
        .service
        .integrations
        .queued
        .values()
        .map(VecDeque::len)
        .sum();
    assert_eq!(queued, 1);

    harness.settle();
    assert!(matches!(
        harness.reply(&first_id),
        Some(Ok(ReplyData::ReviewIntegrate(_)))
    ));
    let second_reply = harness
        .reply(&second_id)
        .expect("the queued request is retried");
    assert_eq!(
        second_reply.expect_err("the target moved").code,
        ErrorCode::StaleTarget
    );
    assert_eq!(lifecycle(&harness, first_oid), Lifecycle::Integrated);
    assert_eq!(lifecycle(&harness, second_oid), Lifecycle::Frozen);
    assert!(harness.service.rt(second_oid).busy.is_none());
}

#[test]
fn a_returned_candidate_reopens_the_objective_for_more_work() {
    let mut harness = Harness::new("review-return");
    let (id, oid, candidate) = harness.frozen("Return", "return.txt");
    let returned = harness.call(Request::ReviewReturn(CandidateCommand {
        objective_id: id.clone(),
        candidate_id: candidate.candidate_id.clone(),
    }));
    assert!(matches!(returned, Ok(ReplyData::ReviewReturn(_))));
    assert_eq!(lifecycle(&harness, oid), Lifecycle::Active);
    let stale = harness
        .approve(&id, &candidate)
        .expect_err("no longer frozen");
    assert_eq!(stale.code, ErrorCode::InvalidLifecycle);
}

#[test]
fn refresh_needs_a_moved_target_and_rebuilds_the_candidate_onto_it() {
    let mut harness = Harness::new("review-refresh");
    let (id, oid, candidate) = harness.frozen("Refresh", "refresh.txt");
    let refresh = |candidate: &CandidateSummary| {
        Request::ReviewRefresh(CandidateCommand {
            objective_id: id.clone(),
            candidate_id: candidate.candidate_id.clone(),
        })
    };
    let unmoved = harness
        .call(refresh(&candidate))
        .expect_err("target has not moved");
    assert_eq!(unmoved.code, ErrorCode::Conflict);
    fs::write(harness.repo.join("upstream.txt"), "upstream\n").expect("write");
    commit_all(&harness.repo, "upstream");
    let Ok(ReplyData::ReviewRefresh(fresh)) = harness.call(refresh(&candidate)) else {
        panic!("refresh");
    };
    assert_ne!(fresh.candidate_id, candidate.candidate_id);
    assert_eq!(fresh.base_target_commit, harness.main_head());
    assert_eq!(lifecycle(&harness, oid), Lifecycle::Frozen);
    let stale = harness.call(refresh(&candidate)).expect_err("superseded");
    assert_eq!(stale.code, ErrorCode::StaleCandidate);
}

#[test]
fn a_conflicting_refresh_returns_the_objective_for_revision() {
    let mut harness = Harness::new("review-refresh-conflict");
    let (id, oid, candidate) = harness.frozen("Conflict", "README");
    fs::write(harness.repo.join("README"), "upstream edit\n").expect("write");
    commit_all(&harness.repo, "conflicting upstream");
    let conflict = harness
        .call(Request::ReviewRefresh(CandidateCommand {
            objective_id: id,
            candidate_id: candidate.candidate_id,
        }))
        .expect_err("conflict");
    assert_eq!(conflict.code, ErrorCode::StaleTarget);
    assert_eq!(lifecycle(&harness, oid), Lifecycle::Active);
    assert!(harness.service.objectives[&oid].row.integration_conflict);
}

#[test]
fn verification_is_refused_while_storage_is_critical() {
    let mut harness = Harness::new("review-verify-storage");
    let (id, _, candidate) = harness.frozen("Verify", "verify.txt");
    harness.service.storage.free_space.level = Pressure::Critical;
    let refused = harness
        .call(Request::ReviewVerify(ReviewVerifyCommand {
            objective_id: id,
            candidate_id: candidate.candidate_id,
            command: vec!["true".into()],
        }))
        .expect_err("storage critical");
    assert_eq!(refused.code, ErrorCode::LimitReached);
}

#[test]
fn freezing_before_the_agent_wrote_anything_is_refused() {
    let mut harness = Harness::new("review-freeze-empty");
    let (id, _) = harness.create_objective("Empty");
    let refused = harness
        .call(Request::ReviewFreeze(ObjectiveSelector {
            objective_id: id,
        }))
        .expect_err("nothing to freeze");
    assert_eq!(refused.code, ErrorCode::InvalidLifecycle);
}

#[test]
fn an_objective_without_changes_completes_and_a_discarded_one_cleans_up_and_forgets() {
    let mut harness = Harness::new("review-closing");
    let (empty, empty_oid) = harness.create_objective("Nothing to do");
    let completed = harness.call(Request::ObjectiveComplete(ObjectiveSelector {
        objective_id: empty,
    }));
    assert!(matches!(completed, Ok(ReplyData::ObjectiveComplete(_))));
    assert_eq!(
        lifecycle(&harness, empty_oid),
        Lifecycle::CompletedNoChanges
    );

    let (id, oid, _) = harness.frozen("Discard", "discard.txt");
    let early = harness
        .call(Request::ObjectiveDeleteHistory(
            ObjectiveHistoryDeleteCommand {
                objective_id: id.clone(),
                confirm_history_loss: true,
            },
        ))
        .expect_err("history of an open Objective stays");
    assert_eq!(early.code, ErrorCode::InvalidLifecycle);
    let discarded = harness.call(Request::ObjectiveDiscard(ObjectiveSelector {
        objective_id: id.clone(),
    }));
    assert!(matches!(discarded, Ok(ReplyData::ObjectiveDiscard(_))));
    assert_eq!(lifecycle(&harness, oid), Lifecycle::Discarded);

    let worktree = harness.service.objectives[&oid]
        .row
        .worktree
        .clone()
        .expect("worktree");
    let cleanup = harness.call(Request::ObjectiveCleanup(ObjectiveCleanupCommand {
        objective_id: id.clone(),
        confirm_data_loss: true,
    }));
    assert!(matches!(cleanup, Ok(ReplyData::ObjectiveCleanup(_))));
    assert!(!worktree.exists());
    assert!(harness.service.objectives[&oid].row.worktree.is_none());

    let unconfirmed = harness
        .call(Request::ObjectiveDeleteHistory(
            ObjectiveHistoryDeleteCommand {
                objective_id: id.clone(),
                confirm_history_loss: false,
            },
        ))
        .expect_err("needs confirmation");
    assert_eq!(unconfirmed.code, ErrorCode::InvalidRequest);
    let deleted = harness.call(Request::ObjectiveDeleteHistory(
        ObjectiveHistoryDeleteCommand {
            objective_id: id,
            confirm_history_loss: true,
        },
    ));
    assert!(matches!(deleted, Ok(ReplyData::ObjectiveDeleteHistory(_))));
    assert!(harness.service.objectives[&oid].row.history_deleted);
    let refs = git(&harness.repo, &["for-each-ref", "--format=%(refname)"]);
    assert!(
        !refs.contains(&oid.to_string()) && !refs.contains("candidate-"),
        "the Objective's refs are gone: {refs}"
    );
}
