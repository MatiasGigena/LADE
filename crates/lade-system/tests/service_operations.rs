mod service_support;

use std::fs;

use lade_protocol::client::{
    ActionName, Badge, Basis, DeliveryState, DraftDeleteCommand, DraftRevision, DraftSaveCommand,
    DraftsListQuery, Empty, ErrorCode, ExecutionState, FeedbackSubmitCommand, InputsListQuery,
    ObjectiveCreateCommand, ObjectiveSelector, OperationGetQuery, OperationState, Outcome,
    Reference, ReferenceVersion, ReplyData, Request, Side, Start,
};
use lade_protocol::types::{Counter, DeliveryMode, Digest, Id, PageLimit, WirePath};
use service_support::*;

#[test]
fn corrupt_store_refuses_to_start_and_is_not_reset() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new("svc-corrupt");
    let state = env.root.path().join("state");
    fs::create_dir_all(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let garbage = vec![0x5a_u8; 8192];
    fs::write(state.join("lade.db"), &garbage).unwrap();
    let error = lade_system::service::start(env.config(), FakeLauncher::shared())
        .err()
        .expect("refused");
    assert!(
        matches!(error, lade_system::service::StartError::Store(_)),
        "{error}"
    );
    assert_eq!(fs::read(state.join("lade.db")).unwrap(), garbage);
    assert!(!env.socket().exists());
}

fn create_command(workspace: &Id, title: &str) -> Request {
    Request::ObjectiveCreate(ObjectiveCreateCommand {
        workspace_id: workspace.clone(),
        title: title.into(),
        intent: lade_protocol::client::Intent {
            outcome: "outcome".into(),
            constraints: String::new(),
            acceptance: String::new(),
        },
        integration_target: "main".into(),
        start: Start::Target,
    })
}

#[test]
fn duplicate_operations_replay_the_recorded_outcome() {
    let env = Env::new("svc-dup");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let operation = new_id();
    let first = client
        .call_op(operation.clone(), create_command(&ws.workspace_id, "First"))
        .unwrap();
    let again = client
        .call_op(operation.clone(), create_command(&ws.workspace_id, "First"))
        .unwrap();
    assert_eq!(first, again, "duplicate returns the original reply");
    let conflict = client
        .call_op(operation.clone(), create_command(&ws.workspace_id, "Other"))
        .unwrap_err();
    assert_eq!(conflict.code, ErrorCode::DuplicateConflict);
    match client.call(Request::Snapshot(Empty {})) {
        Ok(ReplyData::Snapshot(s)) => assert_eq!(s.objectives.len(), 1),
        other => panic!("{other:?}"),
    }
    match client.call(Request::OperationGet(OperationGetQuery {
        operation_id: operation.clone(),
    })) {
        Ok(ReplyData::OperationGet(status)) => {
            assert_eq!(status.state, OperationState::Completed);
            assert!(matches!(status.outcome, Some(Outcome::Ok(_))));
        }
        other => panic!("{other:?}"),
    }
    match client.call(Request::OperationGet(OperationGetQuery {
        operation_id: new_id(),
    })) {
        Ok(ReplyData::OperationGet(status)) => assert_eq!(status.state, OperationState::Unknown),
        other => panic!("{other:?}"),
    }
    let bad = new_id();
    let missing = Request::ExecutionCancel(ObjectiveSelector {
        objective_id: new_id(),
    });
    let r1 = client.call_op(bad.clone(), missing.clone()).unwrap_err();
    let r2 = client.call_op(bad, missing).unwrap_err();
    assert_eq!(r1, r2);
    drop(client);
    service.shutdown();
}

#[test]
fn objectives_and_journal_survive_restart() {
    let env = Env::new("svc-restart");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let operation = new_id();
    let created = match client
        .call_op(
            operation.clone(),
            create_command(&ws.workspace_id, "Durable"),
        )
        .unwrap()
    {
        ReplyData::ObjectiveCreate(d) => *d,
        other => panic!("{other:?}"),
    };
    drop(client);
    service.shutdown();

    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let after = detail(&mut client, &created.objective_id);
    assert_eq!(after.title, "Durable");
    assert_eq!(after.intent, created.intent);
    let replay = client
        .call_op(operation, create_command(&ws.workspace_id, "Durable"))
        .unwrap();
    assert!(
        matches!(replay, ReplyData::ObjectiveCreate(d) if d.objective_id == created.objective_id)
    );
    match client.call(Request::Snapshot(Empty {})) {
        Ok(ReplyData::Snapshot(s)) => {
            assert_eq!(s.objectives.len(), 1);
            assert_eq!(s.workspaces.len(), 1);
        }
        other => panic!("{other:?}"),
    }
    drop(client);
    service.shutdown();
}

#[test]
fn reopening_a_workspace_unavailable_at_start_reattaches_it() {
    let env = Env::new("svc-reattach");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let created = create_objective(&mut client, &ws.workspace_id, "Survives a move");
    drop(client);
    service.shutdown();

    let moved = env.root.path().join("repo-moved");
    fs::rename(&env.repo, &moved).unwrap();
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    match client.call(Request::Snapshot(Empty {})) {
        Ok(ReplyData::Snapshot(s)) => assert!(s.workspaces.is_empty()),
        other => panic!("{other:?}"),
    }
    let start = client
        .call(Request::ExecutionStart(
            lade_protocol::client::ExecutionStartCommand {
                objective_id: created.objective_id.clone(),
                model: lade_protocol::types::Model {
                    provider: "lade-fixture".into(),
                    id: "scripted".into(),
                },
                thinking: None,
            },
        ))
        .unwrap_err();
    assert_eq!(start.code, ErrorCode::NotFound, "{start:?}");
    assert_eq!(
        detail(&mut client, &created.objective_id).execution.state,
        ExecutionState::None
    );
    fs::rename(&moved, &env.repo).unwrap();
    let reopened = open_workspace(&mut client, &env.repo);
    assert_eq!(reopened.workspace_id, ws.workspace_id);
    let after = detail(&mut client, &created.objective_id);
    assert_eq!(after.workspace_id, ws.workspace_id);
    assert!(action_enabled(&after, ActionName::ExecutionStart));
    let list = changes(&mut client, &created.objective_id, Basis::Working);
    assert!(list.files.is_empty());
    start_execution(&mut client, &created.objective_id);
    wait_state(&mut client, &created.objective_id, ExecutionState::Idle);
    drop(client);
    service.shutdown();

    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    match client.call(Request::Snapshot(Empty {})) {
        Ok(ReplyData::Snapshot(s)) => assert_eq!(s.workspaces.len(), 1),
        other => panic!("{other:?}"),
    }
    drop(client);
    service.shutdown();
}

#[test]
fn restart_marks_live_execution_interrupted() {
    let env = Env::new("svc-interrupt");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "Interrupted").objective_id;
    start_execution(&mut client, &objective);
    let idle = wait_state(&mut client, &objective, ExecutionState::Idle);
    assert!(action_enabled(&idle, ActionName::ExecutionCancel));
    assert!(worktree(&env, &objective).join("README").is_file());
    drop(client);
    service.shutdown();

    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let after = detail(&mut client, &objective);
    assert_eq!(after.execution.state, ExecutionState::Cancelled);
    assert_eq!(after.execution.reason.as_deref(), Some("interrupted"));
    assert!(after.badges.contains(&Badge::Interrupted));
    assert_eq!(
        after.attention,
        lade_protocol::client::Attention::NeedsAttention
    );
    assert!(action_enabled(&after, ActionName::ExecutionResume));
    match client.call(Request::ExecutionResume(ObjectiveSelector {
        objective_id: objective.clone(),
    })) {
        Ok(ReplyData::ExecutionResume(d)) => assert_eq!(d.execution.generation.value(), 2),
        other => panic!("{other:?}"),
    }
    wait_state(&mut client, &objective, ExecutionState::Idle);
    drop(client);
    service.shutdown();
}

#[test]
fn restart_makes_unacknowledged_dispatch_uncertain() {
    let env = Env::new("svc-uncertain");
    let service = env.start(FakeLauncher::silent());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "Uncertain").objective_id;
    start_execution(&mut client, &objective);
    wait_state(&mut client, &objective, ExecutionState::Idle);
    client
        .call(Request::MessageSend(
            lade_protocol::client::MessageSendCommand {
                objective_id: objective.clone(),
                text: "do the thing".into(),
                mode: DeliveryMode::Prompt,
            },
        ))
        .unwrap();
    let inputs = |client: &mut Client| match client.call(Request::InputsList(InputsListQuery {
        objective_id: objective.clone(),
        limit: PageLimit::parse("200").unwrap(),
        after: None,
    })) {
        Ok(ReplyData::InputsList(list)) => list.inputs,
        other => panic!("{other:?}"),
    };
    eventually("dispatched", || {
        (inputs(&mut client)[0].delivery == DeliveryState::Dispatched).then_some(())
    });
    drop(client);
    service.shutdown();

    let service = env.start(FakeLauncher::silent());
    let mut client = env.connect();
    assert_eq!(inputs(&mut client)[0].delivery, DeliveryState::Uncertain);
    let after = detail(&mut client, &objective);
    assert!(after.badges.contains(&Badge::DeliveryUncertain));
    client
        .call(Request::ExecutionResume(ObjectiveSelector {
            objective_id: objective.clone(),
        }))
        .unwrap();
    wait_state(&mut client, &objective, ExecutionState::Idle);
    assert_eq!(inputs(&mut client)[0].delivery, DeliveryState::Uncertain);
    drop(client);
    service.shutdown();
}

fn reference(capture: &Id, digest: &Digest, path: &str, start: u64, text: &str) -> Reference {
    Reference {
        artifact_digest: digest.clone(),
        path: WirePath::from_bytes(path.as_bytes().to_vec()).unwrap(),
        version: ReferenceVersion::Working {
            capture_id: capture.clone(),
        },
        side: Side::New,
        byte_start: Counter::new(start),
        byte_end: Counter::new(start + text.len() as u64),
        selected_text: text.into(),
        line_hint: Some(Counter::new(1)),
    }
}

fn save(
    client: &mut Client,
    objective: &Id,
    draft: &Id,
    expected: u64,
    reference: Reference,
    body: &str,
) -> Result<ReplyData, lade_protocol::client::Rejection> {
    client.call(Request::DraftSave(DraftSaveCommand {
        draft_id: draft.clone(),
        objective_id: objective.clone(),
        expected_revision: Counter::new(expected),
        reference,
        body: body.into(),
    }))
}

#[test]
fn draft_cas_and_stale_batch_blocks_whole_submission() {
    let env = Env::new("svc-drafts");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "Feedback").objective_id;
    start_execution(&mut client, &objective);
    let idle = wait_state(&mut client, &objective, ExecutionState::Idle);
    let wt = worktree(&env, &objective);
    fs::write(wt.join("a.txt"), "hello world\n").unwrap();
    fs::write(wt.join("b.txt"), "second file\n").unwrap();
    let list = changes(&mut client, &objective, Basis::Working);
    assert_eq!(list.files.len(), 2);
    let capture = list.capture_id.clone().expect("capture id");
    let a = read_file(&mut client, &objective, Basis::Working, "a.txt");
    let b = read_file(&mut client, &objective, Basis::Working, "b.txt");
    assert_eq!(a.text.as_deref(), Some("hello world\n"));

    let d1 = new_id();
    let d2 = new_id();
    let ref_a = reference(&capture, &a.artifact_digest, "a.txt", 0, "hello");
    let ref_b = reference(&capture, &b.artifact_digest, "b.txt", 0, "second");
    let wrong = reference(&capture, &a.artifact_digest, "a.txt", 0, "HELLO");
    assert_eq!(
        save(&mut client, &objective, &d1, 0, wrong, "x")
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
    let saved = save(
        &mut client,
        &objective,
        &d1,
        0,
        ref_a.clone(),
        "greet properly",
    )
    .unwrap();
    assert!(matches!(&saved, ReplyData::DraftSave(d) if d.revision.value() == 1));
    assert_eq!(
        save(&mut client, &objective, &d1, 0, ref_a.clone(), "dup")
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    save(
        &mut client,
        &objective,
        &d1,
        1,
        ref_a.clone(),
        "greet properly v2",
    )
    .unwrap();
    assert_eq!(
        save(
            &mut client,
            &objective,
            &d1,
            1,
            ref_a.clone(),
            "lost update"
        )
        .unwrap_err()
        .code,
        ErrorCode::Conflict
    );
    save(&mut client, &objective, &d2, 0, ref_b, "rename this").unwrap();

    fs::write(wt.join("b.txt"), "changed file\n").unwrap();
    let session = idle.session.expect("session");
    let submit = |drafts: Vec<DraftRevision>| {
        Request::FeedbackSubmit(FeedbackSubmitCommand {
            objective_id: objective.clone(),
            destination_session_id: session.session_id.clone(),
            association_revision: session.association_revision.clone(),
            drafts,
            mode: DeliveryMode::FollowUp,
            note: None,
        })
    };
    let both = vec![
        DraftRevision {
            draft_id: d1.clone(),
            revision: Counter::new(2),
        },
        DraftRevision {
            draft_id: d2.clone(),
            revision: Counter::new(1),
        },
    ];
    let rejected = client.call(submit(both)).unwrap_err();
    assert_eq!(rejected.code, ErrorCode::StaleReference);
    assert_eq!(
        rejected.stale_drafts,
        std::slice::from_ref(&d2),
        "{}",
        rejected.detail
    );
    assert!(
        !rejected.detail.contains(d2.as_str()),
        "ids travel in stale_drafts: {}",
        rejected.detail
    );
    match client.call(Request::InputsList(InputsListQuery {
        objective_id: objective.clone(),
        limit: PageLimit::parse("200").unwrap(),
        after: None,
    })) {
        Ok(ReplyData::InputsList(list)) => assert!(list.inputs.is_empty()),
        other => panic!("{other:?}"),
    }
    match client.call(Request::DraftsList(DraftsListQuery {
        objective_id: objective.clone(),
        limit: PageLimit::parse("200").unwrap(),
        after: None,
    })) {
        Ok(ReplyData::DraftsList(list)) => {
            assert_eq!(list.drafts.len(), 2);
            assert!(list.drafts.iter().all(|d| d.input_id.is_none()));
        }
        other => panic!("{other:?}"),
    }

    client
        .call(Request::DraftDelete(DraftDeleteCommand {
            draft_id: d2,
            expected_revision: Counter::new(1),
        }))
        .unwrap();
    let accepted = match client
        .call(submit(vec![DraftRevision {
            draft_id: d1,
            revision: Counter::new(2),
        }]))
        .unwrap()
    {
        ReplyData::FeedbackSubmit(input) => input,
        other => panic!("{other:?}"),
    };
    assert!(accepted.text_preview.contains("greet properly v2"));
    eventually("delivery received", || {
        match client.call(Request::InputsList(InputsListQuery {
            objective_id: objective.clone(),
            limit: PageLimit::parse("200").unwrap(),
            after: None,
        })) {
            Ok(ReplyData::InputsList(list)) => (list.inputs.len() == 1
                && list.inputs[0].delivery == DeliveryState::Received)
                .then_some(()),
            _ => None,
        }
    });
    drop(client);
    service.shutdown();
}
