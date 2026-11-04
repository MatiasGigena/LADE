use std::collections::VecDeque;
use std::fs;

use lade_protocol::client::{
    CandidateSummary, ObjectiveCleanupCommand, ObjectiveHistoryDeleteCommand, ObjectiveSelector,
    Request, ReviewIntegrateCommand,
};
use lade_protocol::types::{GitId as WireGitId, Model};
use lade_protocol::worker::WorkerBody;

use super::*;
use crate::service::adapters_support::{commit_all, git};
use crate::service::limits::Pressure;
use crate::service::test_harness::Harness;
use crate::storage::JournalState;
use crate::supervision::WorkerEvent;

fn main_head(harness: &Harness) -> WireGitId {
    WireGitId::from_hex(git(&harness.repo, &["rev-parse", "main"]).trim()).expect("head")
}

fn lifecycle(harness: &Harness, oid: ObjectiveId) -> Lifecycle {
    harness.service.objectives[&oid].objective().lifecycle()
}

/// Starts an agent for a new Objective, lets it write `file`, and freezes its work.
fn frozen(harness: &mut Harness, title: &str, file: &str) -> (Id, ObjectiveId, CandidateSummary) {
    let (id, oid) = harness.create_objective(title);
    harness
        .call(Request::ExecutionStart(
            lade_protocol::client::ExecutionStartCommand {
                objective_id: id.clone(),
                model: Model {
                    provider: "anthropic".into(),
                    id: "model".into(),
                },
                thinking: None,
            },
        ))
        .expect("start");
    harness.worker_event(
        oid,
        WorkerEvent::Body(WorkerBody::Ready {
            pi_session_id: "pi".into(),
            session_file: "/s.jsonl".into(),
        }),
    );
    let worktree = harness.service.objectives[&oid]
        .row
        .worktree
        .clone()
        .expect("worktree");
    fs::write(worktree.join(file), format!("{title}\n")).expect("agent work");
    let ctx = Harness::ctx(lade_protocol::client::RequestName::ReviewFreeze);
    let request_id = ctx.request_id.clone();
    assert!(matches!(
        harness.service.review_freeze(ctx, &id),
        Flow::Deferred
    ));
    assert!(
        harness.reply(&request_id).is_none(),
        "the freeze waits for the worker to stop"
    );
    let (_, log) = harness.last_worker();
    assert!(matches!(
        log.lock().expect("log").sent.last(),
        Some(WorkerBody::Abort)
    ));
    harness.worker_event(
        oid,
        WorkerEvent::Exited {
            code: Some(0),
            signal: None,
        },
    );
    let Some(Ok(ReplyData::ReviewFreeze(candidate))) = harness.reply(&request_id) else {
        panic!("freeze");
    };
    (id, oid, candidate)
}

fn approve(
    harness: &mut Harness,
    id: &Id,
    candidate: &CandidateSummary,
) -> Result<ReplyData, Rejection> {
    let target_commit = main_head(harness);
    harness.call(Request::ReviewApprove(ReviewApproveCommand {
        objective_id: id.clone(),
        candidate_id: candidate.candidate_id.clone(),
        target_commit,
        override_reason: Some("reviewed by hand".into()),
    }))
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
    let (id, oid, candidate) = frozen(&mut harness, "Integrate", "feature.txt");
    assert_eq!(candidate.ordinal.value(), 1);
    assert_eq!(candidate.files.value(), 1);
    assert_eq!(lifecycle(&harness, oid), Lifecycle::Frozen);
    let before = main_head(&harness);
    let approval = approval_id(approve(&mut harness, &id, &candidate));
    assert_eq!(lifecycle(&harness, oid), Lifecycle::Approved);

    let request = integrate(&id, &approval);
    assert!(matches!(
        harness.call(request),
        Ok(ReplyData::ReviewIntegrate(_))
    ));
    assert_eq!(lifecycle(&harness, oid), Lifecycle::Integrated);
    let after = main_head(&harness);
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
    let (id, oid, candidate) = frozen(&mut harness, "Moved", "moved.txt");
    let approval = approval_id(approve(&mut harness, &id, &candidate));
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
    let (first, first_oid, first_candidate) = frozen(&mut harness, "First", "first.txt");
    let (second, second_oid, second_candidate) = frozen(&mut harness, "Second", "second.txt");
    let first_approval = approval_id(approve(&mut harness, &first, &first_candidate));
    let second_approval = approval_id(approve(&mut harness, &second, &second_candidate));

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
    let (id, oid, candidate) = frozen(&mut harness, "Return", "return.txt");
    let returned = harness.call(Request::ReviewReturn(CandidateCommand {
        objective_id: id.clone(),
        candidate_id: candidate.candidate_id.clone(),
    }));
    assert!(matches!(returned, Ok(ReplyData::ReviewReturn(_))));
    assert_eq!(lifecycle(&harness, oid), Lifecycle::Active);
    let stale = approve(&mut harness, &id, &candidate).expect_err("no longer frozen");
    assert_eq!(stale.code, ErrorCode::InvalidLifecycle);
}

#[test]
fn refresh_needs_a_moved_target_and_rebuilds_the_candidate_onto_it() {
    let mut harness = Harness::new("review-refresh");
    let (id, oid, candidate) = frozen(&mut harness, "Refresh", "refresh.txt");
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
    assert_eq!(fresh.base_target_commit, main_head(&harness));
    assert_eq!(lifecycle(&harness, oid), Lifecycle::Frozen);
    let stale = harness.call(refresh(&candidate)).expect_err("superseded");
    assert_eq!(stale.code, ErrorCode::StaleCandidate);
}

#[test]
fn a_conflicting_refresh_returns_the_objective_for_revision() {
    let mut harness = Harness::new("review-refresh-conflict");
    let (id, oid, candidate) = frozen(&mut harness, "Conflict", "README");
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
    let (id, _, candidate) = frozen(&mut harness, "Verify", "verify.txt");
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

    let (id, oid, _) = frozen(&mut harness, "Discard", "discard.txt");
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
