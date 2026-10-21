mod service_support;

use lade_protocol::client::{
    Basis, ClientBody, ClientEnvelope, Draft, DraftDeleteCommand, DraftRevision, DraftSaveCommand,
    DraftsListQuery, ErrorCode, ExecutionState, FeedbackSubmitCommand, InputSummary,
    InputsListQuery, MessageSendCommand, OperationGetQuery, Outcome, Reference, ReferenceVersion,
    ReplyData, Request, RequestFrame, Side,
};
use lade_protocol::types::{Counter, DeliveryMode, Id, PageLimit, WirePath};
use lade_protocol::{Framing, MAX_FRAME, encode};
use service_support::*;

fn all_drafts(client: &mut Client, objective: &Id) -> (Vec<Draft>, usize) {
    let mut after = None;
    let mut drafts = Vec::new();
    let mut pages = 0;
    loop {
        let ReplyData::DraftsList(page) = client
            .call(Request::DraftsList(DraftsListQuery {
                objective_id: objective.clone(),
                limit: PageLimit::parse("200").unwrap(),
                after: after.clone(),
            }))
            .unwrap()
        else {
            panic!("expected saved drafts")
        };
        pages += 1;
        if let Some(next) = &page.next_after {
            assert!(
                after
                    .as_ref()
                    .is_none_or(|old: &lade_protocol::client::DraftCursor| {
                        (old.created_at.unix_ms(), old.draft_id.as_str())
                            < (next.created_at.unix_ms(), next.draft_id.as_str())
                    }),
                "cursor must advance"
            );
            assert!(!page.drafts.is_empty());
        }
        drafts.extend(page.drafts);
        after = page.next_after;
        if after.is_none() {
            return (drafts, pages);
        }
    }
}

fn all_inputs(client: &mut Client, objective: &Id) -> (Vec<InputSummary>, usize) {
    let mut after = None;
    let mut inputs = Vec::new();
    let mut pages = 0;
    loop {
        let ReplyData::InputsList(page) = client
            .call(Request::InputsList(InputsListQuery {
                objective_id: objective.clone(),
                limit: PageLimit::parse("200").unwrap(),
                after: after.clone(),
            }))
            .unwrap()
        else {
            panic!("expected accepted inputs")
        };
        pages += 1;
        if let Some(next) = &page.next_after {
            assert!(
                after
                    .as_ref()
                    .is_none_or(|old: &Counter| old.value() < next.value())
            );
            assert!(!page.inputs.is_empty());
        }
        inputs.extend(page.inputs);
        after = page.next_after;
        if after.is_none() {
            return (inputs, pages);
        }
    }
}

fn sized_save(mut command: DraftSaveCommand, operation: &Id, slack: usize) -> Request {
    command.body.clear();
    let envelope = ClientEnvelope::new(ClientBody::Request(
        RequestFrame::new(
            new_id(),
            Some(operation.clone()),
            Request::DraftSave(command.clone()),
        )
        .unwrap(),
    ));
    let available = MAX_FRAME - slack - serde_json::to_vec(&envelope).unwrap().len();
    command.body = "\u{1}".repeat(available / 6) + &"a".repeat(available % 6);
    assert!(command.body.len() < lade_core::domain::inspection::MAX_TEXT_BYTES);
    let request = Request::DraftSave(command);
    let envelope = ClientEnvelope::new(ClientBody::Request(
        RequestFrame::new(new_id(), Some(operation.clone()), request.clone()).unwrap(),
    ));
    assert_eq!(
        serde_json::to_vec(&envelope).unwrap().len(),
        MAX_FRAME - slack
    );
    encode(&envelope, Framing::Client).expect("the save request itself fits the wire contract");
    request
}

#[test]
fn large_saved_drafts_remain_readable_through_the_service() {
    let env = Env::new("svc-draft-history");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "Draft history").objective_id;
    start_execution(&mut client, &objective);
    wait_state(&mut client, &objective, ExecutionState::Idle);
    let capture = changes(&mut client, &objective, Basis::Working)
        .capture_id
        .unwrap();
    let file = read_file(&mut client, &objective, Basis::Working, "README");
    for _ in 0..3 {
        client
            .call(Request::DraftSave(DraftSaveCommand {
                draft_id: new_id(),
                objective_id: objective.clone(),
                expected_revision: Counter::new(0),
                reference: Reference {
                    artifact_digest: file.artifact_digest.clone(),
                    path: WirePath::from_bytes(b"README".to_vec()).unwrap(),
                    version: ReferenceVersion::Working {
                        capture_id: capture.clone(),
                    },
                    side: Side::New,
                    byte_start: Counter::new(0),
                    byte_end: Counter::new(5),
                    selected_text: "hello".into(),
                    line_hint: Some(Counter::new(1)),
                },
                body: "\"".repeat(200_000),
            }))
            .unwrap();
    }
    drop(client);
    service.shutdown();
    let conn = rusqlite::Connection::open(env.root.path().join("state/lade.db")).unwrap();
    conn.execute("UPDATE drafts SET created_at = 1700000000000", [])
        .unwrap();
    drop(conn);
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let (drafts, pages) = all_drafts(&mut client, &objective);
    assert_eq!(drafts.len(), 3);
    assert!(pages > 1, "large records use byte-bounded pages");
    assert!(
        drafts
            .iter()
            .all(|draft| draft.body == "\"".repeat(200_000))
    );
    let ReplyData::DraftsList(first) = client
        .call(Request::DraftsList(DraftsListQuery {
            objective_id: objective.clone(),
            limit: PageLimit::parse("1").unwrap(),
            after: None,
        }))
        .unwrap()
    else {
        panic!("expected first draft page")
    };
    let anchor = first.drafts[0].clone();
    client
        .call(Request::DraftDelete(DraftDeleteCommand {
            draft_id: anchor.draft_id.clone(),
            expected_revision: anchor.revision,
        }))
        .unwrap();
    let ReplyData::DraftsList(tail) = client
        .call(Request::DraftsList(DraftsListQuery {
            objective_id: objective.clone(),
            limit: PageLimit::parse("200").unwrap(),
            after: first.next_after,
        }))
        .unwrap()
    else {
        panic!("expected drafts after a deleted cursor anchor")
    };
    assert_eq!(tail.drafts.len(), 2);
    assert!(
        tail.drafts
            .iter()
            .all(|draft| draft.draft_id != anchor.draft_id)
    );
    drop(client);
    service.shutdown();
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    assert_eq!(all_drafts(&mut client, &objective).0.len(), 2);
    drop(client);
    service.shutdown();
}

#[test]
fn large_accepted_input_history_remains_readable_through_the_service() {
    let env = Env::new("svc-input-history");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "Input history").objective_id;
    start_execution(&mut client, &objective);
    wait_state(&mut client, &objective, ExecutionState::Idle);
    let mut accepted = Vec::new();
    for _ in 0..180 {
        let ReplyData::MessageSend(input) = client
            .call(Request::MessageSend(MessageSendCommand {
                objective_id: objective.clone(),
                text: "\u{1}".repeat(1024),
                mode: DeliveryMode::FollowUp,
            }))
            .unwrap()
        else {
            panic!("expected accepted input")
        };
        accepted.push(input.input_id);
    }
    let (inputs, pages) = all_inputs(&mut client, &objective);
    assert_eq!(
        inputs
            .iter()
            .map(|input| &input.input_id)
            .collect::<Vec<_>>(),
        accepted.iter().collect::<Vec<_>>()
    );
    assert!(pages > 1, "escaped text uses byte-bounded pages");
    assert!(
        inputs
            .iter()
            .all(|input| input.text_preview == "\u{1}".repeat(1024))
    );
    drop(client);
    service.shutdown();
    let mut conn = rusqlite::Connection::open(env.root.path().join("state/lade.db")).unwrap();
    let tx = conn.transaction().unwrap();
    let first_seq: i64 = tx
        .query_row("SELECT MAX(seq) FROM accepted_inputs", [], |row| row.get(0))
        .unwrap();
    let template = accepted.last().unwrap().clone();
    for index in 1..=400 {
        let id = new_id();
        tx.execute(
            "INSERT INTO accepted_inputs (id,operation_id,objective_id,session_id,association_revision,mode,items,note,rendered_text,accepted_at,seq) \
             SELECT ?1,?2,objective_id,session_id,association_revision,mode,items,note,rendered_text,accepted_at,?3 \
             FROM accepted_inputs WHERE id=?4",
            rusqlite::params![id.as_str(), new_id().as_str(), first_seq + index, template.as_str()],
        ).unwrap();
        tx.execute("INSERT INTO input_delivery(input_id,state,generation,blocked_reason,updated_at) VALUES(?1,'queued',NULL,NULL,0)", [id.as_str()]).unwrap();
        accepted.push(id);
    }
    tx.commit().unwrap();
    drop(conn);
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    assert_eq!(
        all_inputs(&mut client, &objective)
            .0
            .iter()
            .map(|input| &input.input_id)
            .collect::<Vec<_>>(),
        accepted.iter().collect::<Vec<_>>()
    );
    drop(client);
    service.shutdown();
}

#[test]
fn a_wire_sized_draft_is_refused_before_creation_or_update() {
    let env = Env::new("svc-draft-admission");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "Draft admission").objective_id;
    start_execution(&mut client, &objective);
    wait_state(&mut client, &objective, ExecutionState::Idle);
    let capture = changes(&mut client, &objective, Basis::Working)
        .capture_id
        .unwrap();
    let file = read_file(&mut client, &objective, Basis::Working, "README");
    let mut command = DraftSaveCommand {
        draft_id: new_id(),
        objective_id: objective.clone(),
        expected_revision: Counter::new(0),
        reference: Reference {
            artifact_digest: file.artifact_digest,
            path: WirePath::from_bytes(b"README".to_vec()).unwrap(),
            version: ReferenceVersion::Working {
                capture_id: capture,
            },
            side: Side::New,
            byte_start: Counter::new(0),
            byte_end: Counter::new(5),
            selected_text: "hello".into(),
            line_hint: Some(Counter::new(1)),
        },
        body: String::new(),
    };
    let create = new_id();
    let rejected = client
        .call_op(create.clone(), sized_save(command.clone(), &create, 0))
        .unwrap_err();
    assert_eq!(rejected.code, ErrorCode::LimitReached);
    assert!(all_drafts(&mut client, &objective).0.is_empty());
    let ReplyData::OperationGet(recorded) = client
        .call(Request::OperationGet(OperationGetQuery {
            operation_id: create,
        }))
        .unwrap()
    else {
        panic!("expected operation outcome")
    };
    assert!(
        matches!(recorded.outcome, Some(Outcome::Rejected { rejection, .. }) if rejection.code == ErrorCode::LimitReached)
    );
    command.body = "prior durable text".into();
    client.call(Request::DraftSave(command.clone())).unwrap();
    command.expected_revision = Counter::new(1);
    let update = new_id();
    assert_eq!(
        client
            .call_op(update.clone(), sized_save(command, &update, 0))
            .unwrap_err()
            .code,
        ErrorCode::LimitReached
    );
    drop(client);
    service.shutdown();
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let (drafts, _) = all_drafts(&mut client, &objective);
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].body, "prior durable text");
    assert_eq!(drafts[0].revision.value(), 1);
    drop(client);
    service.shutdown();
}

#[test]
fn a_large_representable_draft_can_be_submitted_listed_and_recovered() {
    let env = Env::new("svc-draft-representable");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "Large draft").objective_id;
    start_execution(&mut client, &objective);
    let session = wait_state(&mut client, &objective, ExecutionState::Idle)
        .session
        .unwrap();
    let capture = changes(&mut client, &objective, Basis::Working)
        .capture_id
        .unwrap();
    let file = read_file(&mut client, &objective, Basis::Working, "README");
    let draft_id = new_id();
    let command = DraftSaveCommand {
        draft_id: draft_id.clone(),
        objective_id: objective.clone(),
        expected_revision: Counter::new(0),
        reference: Reference {
            artifact_digest: file.artifact_digest,
            path: WirePath::from_bytes(b"README".to_vec()).unwrap(),
            version: ReferenceVersion::Working {
                capture_id: capture,
            },
            side: Side::New,
            byte_start: Counter::new(0),
            byte_end: Counter::new(5),
            selected_text: "hello".into(),
            line_hint: Some(Counter::new(1)),
        },
        body: String::new(),
    };
    let operation = new_id();
    let Request::DraftSave(command) = sized_save(command, &operation, 8192) else {
        panic!("expected save")
    };
    let body = command.body.clone();
    client
        .call_op(operation.clone(), Request::DraftSave(command))
        .unwrap();
    client
        .call(Request::FeedbackSubmit(FeedbackSubmitCommand {
            objective_id: objective.clone(),
            destination_session_id: session.session_id,
            association_revision: session.association_revision,
            drafts: vec![DraftRevision {
                draft_id,
                revision: Counter::new(1),
            }],
            mode: DeliveryMode::FollowUp,
            note: None,
        }))
        .unwrap();
    assert!(all_drafts(&mut client, &objective).0[0].input_id.is_some());
    drop(client);
    service.shutdown();
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    assert_eq!(all_drafts(&mut client, &objective).0[0].body, body);
    let ReplyData::OperationGet(recorded) = client
        .call(Request::OperationGet(OperationGetQuery {
            operation_id: operation,
        }))
        .unwrap()
    else {
        panic!("expected recovered save")
    };
    assert!(
        matches!(recorded.outcome, Some(Outcome::Ok(data)) if matches!(*data, ReplyData::DraftSave(ref draft) if draft.body == body))
    );
    drop(client);
    service.shutdown();
}
