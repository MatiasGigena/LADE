#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code fails loudly on broken fixtures"
)]

mod service_support;

use lade_core::domain::ids::{ObjectiveId, Uuid, WorkspaceId};
use lade_core::domain::objectives::{BranchName, Intent as CoreIntent, Objective};
use lade_protocol::client::{
    Basis, ClientBody, ClientEnvelope, Empty, ErrorCode, ExecutionState, Intent,
    ObjectiveCreateCommand, ObjectiveSelector, ObjectiveUpdateIntentCommand, OperationGetQuery,
    Outcome, ReplyData, Request, RequestFrame, ReviewVerifyCommand, Settings,
    SettingsUpdateCommand, Start,
};
use lade_protocol::types::ActivityRole;
use lade_protocol::{Framing, MAX_FRAME, encode};
use lade_system::storage::{self, WorkspaceRow};
use service_support::*;

fn sized_intent(mut request: Request, slack: usize) -> Request {
    let envelope = ClientEnvelope::new(ClientBody::Request(
        RequestFrame::new(new_id(), Some(new_id()), request.clone()).unwrap(),
    ));
    let available = MAX_FRAME - slack - serde_json::to_vec(&envelope).unwrap().len();
    let constraints = "\u{1}".repeat(available / 6) + &"a".repeat(available % 6);
    match &mut request {
        Request::ObjectiveCreate(command) => command.intent.constraints = constraints,
        Request::ObjectiveUpdateIntent(command) => command.intent.constraints = constraints,
        _ => panic!("expected intent mutation"),
    }
    let envelope = ClientEnvelope::new(ClientBody::Request(
        RequestFrame::new(new_id(), Some(new_id()), request.clone()).unwrap(),
    ));
    assert_eq!(
        serde_json::to_vec(&envelope).unwrap().len(),
        MAX_FRAME - slack
    );
    encode(&envelope, Framing::Client).expect("the intent request fits the wire contract");
    request
}

#[test]
fn an_unrepresentable_objective_is_refused_before_creation() {
    let env = Env::new("svc-objective-frame");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let operation = new_id();
    let command = ObjectiveCreateCommand {
        workspace_id: ws.workspace_id,
        title: "Large intent".into(),
        intent: Intent {
            outcome: "Readable intent".into(),
            constraints: String::new(),
            acceptance: String::new(),
        },
        integration_target: "main".into(),
        start: Start::Target,
    };
    let request = sized_intent(Request::ObjectiveCreate(command), 0);
    let result = client.call_op(operation, request);
    drop(client);
    service.shutdown();
    let conn = env.db();
    let stored: i64 = conn
        .query_row("SELECT COUNT(*) FROM objectives", [], |row| row.get(0))
        .unwrap();
    assert_eq!(result.unwrap_err().code, ErrorCode::LimitReached);
    assert_eq!(
        stored, 0,
        "a refused Objective must not have been committed"
    );
}

#[test]
fn an_unrepresentable_intent_update_preserves_the_prior_durable_intent() {
    let env = Env::new("svc-intent-frame");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "Prior intent");
    let operation = new_id();
    let refused = client
        .call_op(
            operation.clone(),
            sized_intent(
                Request::ObjectiveUpdateIntent(ObjectiveUpdateIntentCommand {
                    objective_id: objective.objective_id.clone(),
                    expected_intent_revision: objective.intent_revision.clone(),
                    intent: Intent {
                        outcome: "Replacement".into(),
                        constraints: String::new(),
                        acceptance: String::new(),
                    },
                }),
                0,
            ),
        )
        .unwrap_err();
    let recorded = client.call(Request::OperationGet(OperationGetQuery {
        operation_id: operation,
    }));
    drop(client);
    service.shutdown();
    let store = env.store();
    assert!(
        storage::load_objectives(store.conn()).unwrap()[0]
            .objective
            .record()
            .intent
            .constraints()
            .is_empty(),
        "refused update must preserve the prior durable intent"
    );
    drop(store);
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let after = detail(&mut client, &objective.objective_id);
    drop(client);
    service.shutdown();
    assert_eq!(refused.code, ErrorCode::LimitReached);
    assert!(
        matches!(recorded.unwrap(), ReplyData::OperationGet(recorded) if matches!(&recorded.outcome, Some(Outcome::Rejected { rejection, .. }) if rejection.code == ErrorCode::LimitReached))
    );
    assert_eq!(after.intent, objective.intent);
    assert_eq!(after.intent_revision, objective.intent_revision);
}

#[test]
fn large_intent_stays_readable_when_session_and_review_history_grow() {
    let env = Env::new("svc-intent-growth");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let operation = new_id();
    let request = sized_intent(
        Request::ObjectiveCreate(ObjectiveCreateCommand {
            workspace_id: ws.workspace_id,
            title: "Long intent".into(),
            intent: Intent {
                outcome: "Readable through history growth".into(),
                constraints: String::new(),
                acceptance: String::new(),
            },
            integration_target: "main".into(),
            start: Start::Target,
        }),
        160_000,
    );
    let ReplyData::ObjectiveCreate(objective) = client.call_op(operation.clone(), request).unwrap()
    else {
        panic!("expected objective")
    };
    let objective = *objective;
    start_execution(&mut client, &objective.objective_id);
    wait_state(&mut client, &objective.objective_id, ExecutionState::Idle);
    std::fs::write(
        worktree(&env, &objective.objective_id).join("work.txt"),
        "work\n",
    )
    .unwrap();
    changes(&mut client, &objective.objective_id, Basis::Working);
    client
        .call(Request::ReviewFreeze(ObjectiveSelector {
            objective_id: objective.objective_id.clone(),
        }))
        .unwrap();
    drop(client);
    service.shutdown();
    seed_review_history(&env, &objective.objective_id, 64);
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let after = client.call(Request::ObjectiveGet(ObjectiveSelector {
        objective_id: objective.objective_id.clone(),
    }));
    let ReplyData::OperationGet(recorded) = client
        .call(Request::OperationGet(OperationGetQuery {
            operation_id: operation,
        }))
        .unwrap()
    else {
        panic!("expected recovered create")
    };
    drop(client);
    service.shutdown();
    let ReplyData::ObjectiveGet(after) = after.unwrap() else {
        panic!("expected detail")
    };
    assert_eq!(after.intent, objective.intent);
    assert!(after.session.is_some());
    assert_eq!(after.candidates.len(), 64);
    assert_eq!(after.verifications.len(), 64);
    assert!(after.verifications.iter().all(|row| row.truncated));
    assert!(
        matches!(recorded.outcome, Some(Outcome::Ok(data)) if matches!(*data, ReplyData::ObjectiveCreate(ref detail) if detail.intent == objective.intent))
    );
    let conn = env.db();
    let (argv, tail): (String, String) = conn
        .query_row(
            "SELECT argv,output_tail FROM verifications LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let argv: Vec<String> = serde_json::from_str(&argv).unwrap();
    assert_eq!(argv.len(), 256);
    assert_eq!(argv[1], "\u{1}".repeat(400));
    assert_eq!(tail, "é\u{1}".repeat(2048));
}

fn seed_review_history(env: &Env, objective: &lade_protocol::types::Id, count: i64) {
    let mut conn = env.db();
    let tx = conn.transaction().unwrap();
    let template: String = tx
        .query_row(
            "SELECT id FROM candidates WHERE objective_id=?1",
            [objective.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    tx.execute(
        "UPDATE candidates SET status='historical' WHERE objective_id=?1",
        [objective.as_str()],
    )
    .unwrap();
    let mut argv = vec!["true".to_string()];
    argv.extend(std::iter::repeat_n("\u{1}".repeat(400), 255));
    for ordinal in 1..=count {
        let candidate = if ordinal == 1 {
            template.clone()
        } else {
            new_id().as_str().to_string()
        };
        if ordinal > 1 {
            tx.execute("INSERT INTO candidates(id,objective_id,ordinal,commit_id,tree,base_target_commit,manifest_digest,files,added,removed,created_at,status) SELECT ?1,objective_id,?2,commit_id,tree,base_target_commit,manifest_digest,files,added,removed,created_at,'historical' FROM candidates WHERE id=?3", rusqlite::params![candidate, ordinal, template]).unwrap();
        }
        tx.execute("INSERT INTO verifications(id,objective_id,candidate_id,argv,cwd_label,started_at,ended_at,exit_code,outcome,output_artifact,output_tail,truncated,seq) VALUES(?1,?2,?3,?4,'fixture',1700000000000,1700000000001,0,'passed',NULL,?5,0,?6)", rusqlite::params![new_id().as_str(),objective.as_str(),candidate,serde_json::to_string(&argv).unwrap(),"é\u{1}".repeat(2048),ordinal]).unwrap();
    }
    tx.commit().unwrap();
}

fn seed_objectives(env: &Env, count: usize, outcome: &str) {
    let mut store = env.store();
    let mut template = storage::load_objectives(store.conn()).unwrap().remove(0);
    let mut record = template.objective.clone().into_record();
    record.intent = CoreIntent::new(outcome, "", "").unwrap();
    template.objective = Objective::restore(record).unwrap();
    let tx = store.begin().unwrap();
    storage::save_objective(&tx, &template).unwrap();
    for _ in 1..count {
        let id = ObjectiveId::new(Uuid::parse(new_id().as_str()).unwrap());
        let mut row = template.clone();
        let mut record = row.objective.into_record();
        record.id = id;
        record.branch = BranchName::for_objective(id);
        row.objective = Objective::restore(record).unwrap();
        storage::save_objective(&tx, &row).unwrap();
    }
    tx.commit().unwrap();
}

fn dense_settings(commands: usize, words: usize) -> Settings {
    let mut argv = vec!["true".to_string()];
    argv.extend(std::iter::repeat_n("a".into(), words - 1));
    Settings {
        verification_commands: vec![argv; commands],
        default_model: None,
    }
}

#[test]
fn snapshot_byte_budget_preserves_all_objectives_and_exact_settings() {
    let env = Env::new("svc-snapshot-bytes");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    create_objective(&mut client, &ws.workspace_id, "Snapshot record");
    let settings = dense_settings(8, 128);
    client
        .call(Request::SettingsUpdate(SettingsUpdateCommand {
            default_model: None,
            verification_commands: Some(settings.verification_commands.clone()),
        }))
        .unwrap();
    drop(client);
    service.shutdown();
    seed_objectives(&env, 400, &"\u{1}".repeat(512));
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let result = client.call(Request::Snapshot(Empty {}));
    let refused = client.call(Request::SettingsUpdate(SettingsUpdateCommand {
        default_model: None,
        verification_commands: Some(dense_settings(32, 256).verification_commands),
    }));
    drop(client);
    service.shutdown();
    let ReplyData::Snapshot(data) = result.unwrap() else {
        panic!("expected snapshot")
    };
    assert_eq!(data.objectives.len(), 400);
    assert_eq!(data.workspaces.len(), 1);
    assert_eq!(
        data.workspaces[0].branches.first().map(String::as_str),
        Some("main")
    );
    assert_eq!(data.settings, settings);
    assert!(
        data.objectives
            .iter()
            .all(|row| !row.outcome.is_empty() && row.outcome.len() < 512)
    );
    assert_eq!(refused.unwrap_err().code, ErrorCode::LimitReached);
    let store = env.store();
    assert_eq!(
        storage::load_settings(store.conn()).unwrap(),
        Some(settings)
    );
    assert!(
        storage::load_objectives(store.conn())
            .unwrap()
            .iter()
            .all(|row| row.objective.record().intent.outcome() == "\u{1}".repeat(512))
    );
}

#[test]
fn snapshot_node_budget_keeps_rows_and_refuses_unrepresentable_growth() {
    let env = Env::new("svc-snapshot-nodes");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    create_objective(&mut client, &ws.workspace_id, "Snapshot record");
    drop(client);
    service.shutdown();
    seed_objectives(&env, 400, "x");
    for index in 0..64 {
        git(
            &env.repo,
            &[
                "update-ref",
                &format!("refs/heads/branch-{index:02}"),
                "HEAD",
            ],
        );
    }
    let store = env.store();
    for index in 1..64 {
        let path = env.root.path().join(format!("repository-{index}"));
        git(
            &env.repo,
            &[
                "worktree",
                "add",
                "--detach",
                "-q",
                path.to_str().unwrap(),
                "HEAD",
            ],
        );
        storage::insert_workspace(
            store.conn(),
            &WorkspaceRow {
                id: WorkspaceId::new(Uuid::parse(new_id().as_str()).unwrap()),
                path,
                name: format!("Workspace {index}"),
                default_branch: Some("main".into()),
                opened_at: 0,
            },
        )
        .unwrap();
    }
    let settings = dense_settings(32, 256);
    storage::save_settings(store.conn(), &settings).unwrap();
    drop(store);
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let result = client.call(Request::Snapshot(Empty {}));
    let create = client.call(Request::ObjectiveCreate(ObjectiveCreateCommand {
        workspace_id: ws.workspace_id,
        title: "Cannot add".into(),
        intent: Intent {
            outcome: "x".into(),
            constraints: String::new(),
            acceptance: String::new(),
        },
        integration_target: "main".into(),
        start: Start::Target,
    }));
    let new_repo = env.root.path().join("another-repository");
    git(
        &env.repo,
        &[
            "worktree",
            "add",
            "--detach",
            "-q",
            new_repo.to_str().unwrap(),
            "HEAD",
        ],
    );
    let open = client.call(Request::WorkspaceOpen(
        lade_protocol::client::WorkspaceOpenCommand {
            path: lade_protocol::types::WirePath::from_os_str(new_repo.as_os_str()).unwrap(),
        },
    ));
    drop(client);
    service.shutdown();
    let ReplyData::Snapshot(data) = result.unwrap() else {
        panic!("expected snapshot")
    };
    assert_eq!(data.objectives.len(), 400);
    assert_eq!(data.workspaces.len(), 64);
    assert!(
        data.workspaces
            .iter()
            .all(|workspace| workspace.branches.first().map(String::as_str) == Some("main"))
    );
    assert_eq!(data.settings, settings);
    assert!(
        data.workspaces
            .iter()
            .map(|row| row.branches.len())
            .sum::<usize>()
            < 64 * 64
    );
    assert_eq!(create.unwrap_err().code, ErrorCode::LimitReached);
    assert_eq!(open.unwrap_err().code, ErrorCode::LimitReached);
    let store = env.store();
    assert_eq!(storage::load_objectives(store.conn()).unwrap().len(), 400);
    assert_eq!(storage::load_workspaces(store.conn()).unwrap().len(), 64);
}

#[test]
fn a_wire_sized_verification_command_has_bounded_reply_event_and_recovery_previews() {
    if !sandbox_works() {
        return;
    }
    let env = Env::new("svc-verification-frame");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective =
        create_objective(&mut client, &ws.workspace_id, "Verification frame").objective_id;
    start_execution(&mut client, &objective);
    wait_state(&mut client, &objective, ExecutionState::Idle);
    std::fs::write(worktree(&env, &objective).join("work.txt"), "work\n").unwrap();
    changes(&mut client, &objective, Basis::Working);
    let ReplyData::ReviewFreeze(candidate) = client
        .call(Request::ReviewFreeze(ObjectiveSelector {
            objective_id: objective.clone(),
        }))
        .unwrap()
    else {
        panic!("expected candidate")
    };
    let mut command = ReviewVerifyCommand {
        objective_id: objective.clone(),
        candidate_id: candidate.candidate_id,
        command: vec!["true".to_string()],
    };
    command
        .command
        .extend(std::iter::repeat_n(String::new(), 44));
    let mut remaining = MAX_FRAME
        - serde_json::to_vec(&ClientEnvelope::new(ClientBody::Request(
            RequestFrame::new(
                new_id(),
                Some(new_id()),
                Request::ReviewVerify(command.clone()),
            )
            .unwrap(),
        )))
        .unwrap()
        .len();
    for arg in command.command.iter_mut().skip(1) {
        let len = (remaining / 6).min(4096);
        *arg = "\u{1}".repeat(len);
        remaining -= len * 6;
    }
    command
        .command
        .last_mut()
        .unwrap()
        .push_str(&"a".repeat(remaining));
    let request = Request::ReviewVerify(command.clone());
    let envelope = ClientEnvelope::new(ClientBody::Request(
        RequestFrame::new(new_id(), Some(new_id()), request.clone()).unwrap(),
    ));
    assert_eq!(serde_json::to_vec(&envelope).unwrap().len(), MAX_FRAME);
    encode(&envelope, Framing::Client).unwrap();
    let operation = new_id();
    let result = client.call_op(operation.clone(), request);
    let event = result.as_ref().ok().map(|_| {
        client.wait_event("verification preview", |event| {
            matches!(event, lade_protocol::client::Event::VerificationChanged(_))
        })
    });
    let recovered = client.call(Request::OperationGet(OperationGetQuery {
        operation_id: operation,
    }));
    drop(client);
    service.shutdown();
    let ReplyData::ReviewVerify(preview) = result.unwrap() else {
        panic!("expected verification")
    };
    assert!(preview.truncated);
    assert_ne!(preview.command, command.command);
    assert!(
        matches!(event, Some(lade_protocol::client::Event::VerificationChanged(row)) if row.truncated)
    );
    assert!(
        matches!(recovered.unwrap(), ReplyData::OperationGet(recorded) if matches!(&recorded.outcome, Some(Outcome::Ok(data)) if matches!(data.as_ref(), ReplyData::ReviewVerify(row) if row.truncated)))
    );
    let store = env.store();
    let review = storage::load_review(
        store.conn(),
        ObjectiveId::new(Uuid::parse(objective.as_str()).unwrap()),
    )
    .unwrap();
    assert_eq!(review.verifications()[0].argv, command.command);
}

#[test]
fn activity_pages_account_for_escaped_tool_names_and_retain_the_whole_history() {
    let env = Env::new("svc-activity-budget");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "Activity budget").objective_id;
    start_execution(&mut client, &objective);
    wait_state(&mut client, &objective, ExecutionState::Idle);
    drop(client);
    service.shutdown();
    let mut store = env.store();
    let tx = store.begin().unwrap();
    let oid = ObjectiveId::new(Uuid::parse(objective.as_str()).unwrap());
    for seq in 1..=200 {
        let item_id = new_id();
        let body = lade_protocol::worker::WorkerBody::ActivityItem {
            item_id: item_id.clone(),
            role: ActivityRole::Tool,
            title: "Tool output".into(),
            text: "x".repeat(3600),
            is_error: false,
            truncated: false,
            tool_name: Some("\u{1}".repeat(256)),
        };
        encode(
            &lade_protocol::worker::WorkerEnvelope::new(body),
            Framing::Worker,
        )
        .expect("each stored activity item is a valid worker frame");
        storage::upsert_activity(
            &tx,
            oid,
            &storage::ActivityRow {
                item_id: item_id.clone(),
                seq,
                at: 1_700_000_000_000,
                role: ActivityRole::Tool,
                title: "Tool output".into(),
                text: "x".repeat(3600),
                tool_name: Some("\u{1}".repeat(256)),
                is_error: false,
                truncated: false,
            },
        )
        .unwrap();
    }
    tx.commit().unwrap();
    drop(store);
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let mut before = None;
    let mut items = Vec::new();
    let mut pages = 0;
    let mut failure = None;
    loop {
        let reply = client.call(Request::ActivityPage(
            lade_protocol::client::ActivityPageQuery {
                objective_id: objective.clone(),
                before,
                limit: lade_protocol::types::PageLimit::parse("200").unwrap(),
            },
        ));
        let page = match reply {
            Ok(ReplyData::ActivityPage(page)) => page,
            Err(error) => {
                failure = Some(error);
                break;
            }
            other => panic!("expected activity: {other:?}"),
        };
        pages += 1;
        before = page.items.first().map(|item| item.seq.clone());
        let more = page.has_more;
        assert!(!more || !page.items.is_empty());
        items.extend(page.items);
        if !more {
            break;
        }
    }
    drop(client);
    service.shutdown();
    assert!(
        failure.is_none(),
        "valid activity history must page successfully: {failure:?}"
    );
    assert!(pages > 1);
    assert_eq!(items.len(), 200);
    let mut sequences: Vec<_> = items.iter().map(|item| item.seq.value()).collect();
    sequences.sort_unstable();
    assert_eq!(sequences, (1..=200).collect::<Vec<_>>());
    assert!(items.iter().all(|item| item.text == "x".repeat(3600)
        && item.tool_name.as_deref() == Some("\u{1}".repeat(256).as_str())));
}

#[test]
fn a_large_activity_item_makes_progress_or_reports_an_explicit_hard_limit() {
    let env = Env::new("svc-activity-single");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "Large activity").objective_id;
    start_execution(&mut client, &objective);
    wait_state(&mut client, &objective, ExecutionState::Idle);
    drop(client);
    service.shutdown();
    let item_id = new_id();
    for (length, fits) in [(150_000, true), (192 * 1024, false)] {
        let text = "\u{1}".repeat(length);
        if fits {
            let body = lade_protocol::worker::WorkerBody::ActivityItem {
                item_id: item_id.clone(),
                role: ActivityRole::Assistant,
                title: "Assistant".into(),
                text: text.clone(),
                is_error: false,
                truncated: false,
                tool_name: None,
            };
            encode(
                &lade_protocol::worker::WorkerEnvelope::new(body),
                Framing::Worker,
            )
            .expect("producer accepts the whole oversized-soft-budget item");
        } else {
            for chunk in text.as_bytes().chunks(16 * 1024) {
                let body = lade_protocol::worker::WorkerBody::ActivityDelta {
                    item_id: item_id.clone(),
                    append_text: String::from_utf8(chunk.to_vec()).unwrap(),
                };
                encode(
                    &lade_protocol::worker::WorkerEnvelope::new(body),
                    Framing::Worker,
                )
                .expect("streamed chunks can assemble a raw192KiB item");
            }
        }
        let store = env.store();
        storage::upsert_activity(
            store.conn(),
            ObjectiveId::new(Uuid::parse(objective.as_str()).unwrap()),
            &storage::ActivityRow {
                item_id: item_id.clone(),
                seq: 1,
                at: 1_700_000_000_000,
                role: ActivityRole::Assistant,
                title: "Assistant".into(),
                text: text.clone(),
                tool_name: None,
                is_error: false,
                truncated: false,
            },
        )
        .unwrap();
        drop(store);
        let service = env.start(FakeLauncher::shared());
        let mut client = env.connect();
        let result = client.call(Request::ActivityPage(
            lade_protocol::client::ActivityPageQuery {
                objective_id: objective.clone(),
                before: None,
                limit: lade_protocol::types::PageLimit::parse("200").unwrap(),
            },
        ));
        drop(client);
        service.shutdown();
        if fits {
            let ReplyData::ActivityPage(page) = result.unwrap() else {
                panic!("expected activity page")
            };
            assert_eq!(
                page.items.len(),
                1,
                "an oversized soft-budget record must make progress"
            );
            assert!(!page.has_more);
            assert_eq!(page.items[0].text, text);
        } else {
            assert_eq!(result.unwrap_err().code, ErrorCode::LimitReached);
        }
        let store = env.store();
        let saved: String = store
            .conn()
            .query_row(
                "SELECT text FROM activity WHERE item_id=?1",
                [item_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(saved.len(), length);
    }
}
