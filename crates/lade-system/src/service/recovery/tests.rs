use std::fs;

use lade_core::domain::execution::ExecutionState;
use lade_core::domain::objectives::Lifecycle;
use lade_protocol::client::{
    MessageSendCommand, ObjectiveSelector, OperationGetQuery, OperationState as WireState,
    ReplyData, Request, RequestName, ReviewIntegrateCommand,
};
use lade_protocol::types::{DeliveryMode, Id, Model};

use super::*;
use crate::service::adapters_support::git;
use crate::service::test_harness::Harness;
use crate::service::{Flow, ReqCtx};

fn lifecycle(harness: &Harness, oid: ObjectiveId) -> Lifecycle {
    harness.service.objectives[&oid].objective().lifecycle()
}

fn journal_states(harness: &Harness) -> Vec<String> {
    let conn = harness.service.store.conn();
    let mut stmt = conn
        .prepare("SELECT state FROM integration_journal ORDER BY created_at")
        .expect("prepare");
    stmt.query_map([], |r| r.get(0))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows")
}

/// Approves a frozen Candidate and journals its integration, leaving the squash job held.
fn integration_in_flight(harness: &mut Harness) -> (ObjectiveId, ReqCtx) {
    let (id, oid, candidate) = harness.frozen("Interrupted", "interrupted.txt");
    let Ok(ReplyData::ReviewApprove(approval)) = harness.approve(&id, &candidate) else {
        panic!("approve");
    };
    let ctx = Harness::ctx(RequestName::ReviewIntegrate);
    let retry = Harness::ctx(RequestName::ReviewIntegrate);
    let flow = harness.service.review_integrate(
        ctx,
        &ReviewIntegrateCommand {
            objective_id: id,
            approval_id: approval.approval_id,
        },
    );
    assert!(matches!(flow, Flow::Deferred));
    assert_eq!(journal_states(harness), [JournalState::Intent.as_str()]);
    (oid, retry)
}

#[test]
fn a_restart_reloads_workspaces_objectives_and_input_sequence() {
    let mut harness = Harness::new("recovery-load");
    let (id, oid) = harness.create_objective("Reload");
    harness
        .call(Request::ExecutionStart(
            lade_protocol::client::ExecutionStartCommand {
                objective_id: id.clone(),
                model: Model {
                    provider: "anthropic".into(),
                    id: "m".into(),
                },
                thinking: None,
            },
        ))
        .expect("start");
    harness
        .call(Request::MessageSend(MessageSendCommand {
            objective_id: id,
            text: "hello".into(),
            mode: DeliveryMode::Prompt,
        }))
        .expect("queued while starting");
    let seq = harness.service.input_seq;
    harness.restart();
    assert_eq!(harness.service.workspaces.len(), 1);
    assert!(harness.service.objectives.contains_key(&oid));
    assert_eq!(harness.service.input_seq, seq);
    assert_eq!(harness.service.objectives[&oid].inputs.len(), 1);
}

#[test]
fn an_agent_running_at_restart_is_interrupted_and_its_queued_input_waits_for_a_resume() {
    let mut harness = Harness::new("recovery-live");
    let (id, oid) = harness.create_objective("Live");
    harness
        .call(Request::ExecutionStart(
            lade_protocol::client::ExecutionStartCommand {
                objective_id: id.clone(),
                model: Model {
                    provider: "anthropic".into(),
                    id: "m".into(),
                },
                thinking: None,
            },
        ))
        .expect("start");
    harness
        .call(Request::MessageSend(MessageSendCommand {
            objective_id: id,
            text: "hello".into(),
            mode: DeliveryMode::Prompt,
        }))
        .expect("queued");
    harness.restart();
    let st = &harness.service.objectives[&oid];
    let state = st.objective().execution().state();
    assert!(!state.is_live(), "{state:?}");
    assert!(matches!(
        state,
        ExecutionState::Cancelled { interrupted: true }
    ));
    assert_eq!(
        st.inputs[0].1.delivery,
        lade_core::domain::feedback::DeliveryState::Queued,
        "never dispatched, so nothing was lost"
    );
    let stored = storage::load_objectives(harness.service.store.conn()).expect("objectives");
    assert!(
        !stored[0].objective.execution().state().is_live(),
        "the interruption is durable"
    );
}

#[test]
fn operations_pending_at_restart_become_uncertain_with_an_interrupted_outcome() {
    let mut harness = Harness::new("recovery-operations");
    let operation = Id::from_random_bytes([4; 16]);
    storage::record_operation(
        harness.service.store.conn(),
        OperationKey {
            id: &operation,
            digest: "digest",
            name: RequestName::ReviewFreeze,
        },
        OperationState::Pending,
        None,
        1,
    )
    .expect("pending");
    harness.restart();
    let Ok(ReplyData::OperationGet(status)) =
        harness.call(Request::OperationGet(OperationGetQuery {
            operation_id: operation,
        }))
    else {
        panic!("operation get");
    };
    assert_eq!(status.state, WireState::Uncertain);
    let Some(Outcome::Rejected { name, rejection }) = status.outcome else {
        panic!("an interrupted outcome");
    };
    assert_eq!(name, RequestName::ReviewFreeze);
    assert_eq!(rejection.code, ErrorCode::Interrupted);
}

#[test]
fn an_integration_whose_squash_landed_before_the_restart_is_completed() {
    let mut harness = Harness::new("recovery-journal-done");
    let (oid, _) = integration_in_flight(&mut harness);
    for (_, task) in harness.service.held.replace(Vec::new()).unwrap_or_default() {
        task();
    }
    let squashed = harness.main_head();
    harness.restart();
    assert_eq!(
        journal_states(&harness),
        [JournalState::ReconciledDone.as_str()]
    );
    assert_eq!(lifecycle(&harness, oid), Lifecycle::Integrated);
    assert_eq!(harness.main_head(), squashed);
    assert_eq!(
        git(&harness.repo, &["status", "--porcelain"]),
        "",
        "the target checkout matches the squash commit"
    );
}

#[test]
fn an_integration_that_never_ran_is_aborted_and_the_approval_kept_reviewable() {
    let mut harness = Harness::new("recovery-journal-aborted");
    let before = harness.main_head();
    let (oid, _) = integration_in_flight(&mut harness);
    harness.restart();
    assert_eq!(
        journal_states(&harness),
        [JournalState::ReconciledAborted.as_str()]
    );
    assert_eq!(harness.main_head(), before);
    assert_ne!(lifecycle(&harness, oid), Lifecycle::Integrated);
    assert!(
        harness
            .call(Request::ObjectiveGet(ObjectiveSelector {
                objective_id: crate::service::convert::wire_id(oid.uuid()),
            }))
            .is_ok()
    );
}

#[test]
fn the_restart_sweep_removes_leftover_scratch_and_verification_copies() {
    let mut harness = Harness::new("recovery-sweep");
    harness.open_workspace();
    let paths = harness.service.paths.clone();
    fs::write(paths.tmp.join("partial"), "x").expect("tmp file");
    fs::create_dir_all(paths.verify.join("old/copy")).expect("verify dir");
    git(&harness.repo, &["branch", "lade/verify/old", "main"]);
    git(&harness.repo, &["branch", "lade/keep", "main"]);
    harness.restart();
    assert_eq!(fs::read_dir(&paths.tmp).expect("tmp").count(), 0);
    assert_eq!(fs::read_dir(&paths.verify).expect("verify").count(), 0);
    let branches = git(&harness.repo, &["branch", "--format=%(refname:short)"]);
    assert!(!branches.contains("lade/verify/old"), "{branches}");
    assert!(branches.contains("lade/keep"), "other branches stay");
}
