use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use lade_protocol::client::{ErrorCode, Event, NoticeCode, ReplyData};
use lade_protocol::client::{
    ExecutionStartCommand, ObjectiveSelector, Request, SessionConfigureCommand,
    SessionImportCommand, UiRespondCommand,
};
use lade_protocol::types::{UiAnswer, UiCloseReason, UiPrompt, WirePath};
use lade_protocol::worker::{LoadStatus, LoadedKind, LoadedResource, ResourceSource};
use lade_protocol::worker::{SessionCommand, WorkerBody};

use super::*;
use crate::service::test_harness::{Harness, WorkerLog};
use crate::supervision::WorkerEvent;

fn model(provider: &str, id: &str) -> Model {
    Model {
        provider: provider.into(),
        id: id.into(),
    }
}

/// An Objective with an idle, ready worker, and that worker's log.
fn ready(harness: &mut Harness, title: &str) -> (Id, ObjectiveId, Arc<Mutex<WorkerLog>>) {
    let (id, oid) = harness.create_objective(title);
    harness
        .call(Request::ExecutionStart(ExecutionStartCommand {
            objective_id: id.clone(),
            model: model("anthropic", "first"),
            thinking: Some(ThinkingLevel::Low),
        }))
        .expect("start");
    harness.worker_event(
        oid,
        WorkerEvent::Body(WorkerBody::Ready {
            pi_session_id: "pi-1".into(),
            session_file: "/s/1.jsonl".into(),
        }),
    );
    let (_, log) = harness.last_worker();
    (id, oid, log)
}

/// The id of the last session command the worker was sent.
fn last_command(log: &Arc<Mutex<WorkerLog>>) -> (Id, SessionCommand) {
    let log = log.lock().expect("log");
    log.sent
        .iter()
        .rev()
        .find_map(|body| match body {
            WorkerBody::Command {
                command_id,
                command,
            } => Some((command_id.clone(), command.clone())),
            _ => None,
        })
        .expect("a session command")
}

fn result(command_id: Id, ok: bool) -> WorkerEvent {
    WorkerEvent::Body(WorkerBody::CommandResult {
        command_id,
        ok,
        message: "refused by pi".into(),
        entries: Some(Vec::new()),
        leaf_id: Some("leaf".into()),
        truncated: None,
    })
}

fn deferred(harness: &mut Harness, request: Request) -> Id {
    let ctx = Harness::ctx(request.name());
    let request_id = ctx.request_id.clone();
    harness.service.dispatch(ctx, request);
    harness.settle();
    request_id
}

fn changed(generation: u64, reason: SessionChangeReason, provider: &str) -> WorkerEvent {
    WorkerEvent::Body(WorkerBody::SessionChanged {
        generation: Counter::new(generation),
        reason,
        pi_session_id: format!("pi-{generation}"),
        session_file: format!("/s/{generation}.jsonl"),
        model: model(provider, "second"),
        thinking: ThinkingLevel::High,
        leaf_id: None,
    })
}

#[test]
fn a_session_command_is_answered_by_the_worker_result_with_its_id() {
    let mut harness = Harness::new("session-tree");
    let (id, oid, log) = ready(&mut harness, "Tree");
    let request_id = deferred(
        &mut harness,
        Request::SessionTree(ObjectiveSelector {
            objective_id: id.clone(),
        }),
    );
    assert!(harness.reply(&request_id).is_none(), "waits for the worker");
    let (command_id, command) = last_command(&log);
    assert!(matches!(command, SessionCommand::Tree));

    let busy = harness
        .call(Request::SessionTree(ObjectiveSelector {
            objective_id: id.clone(),
        }))
        .expect_err("one command at a time");
    assert_eq!(busy.code, ErrorCode::Conflict);
    harness.worker_event(oid, result(Id::from_random_bytes([9; 16]), true));
    assert!(
        harness.reply(&request_id).is_none(),
        "another command's result is ignored"
    );
    harness.worker_event(oid, result(command_id, true));
    let Some(Ok(ReplyData::SessionTree(tree))) = harness.reply(&request_id) else {
        panic!("tree reply");
    };
    assert_eq!(tree.leaf_id.as_deref(), Some("leaf"));
    assert!(!harness.service.command_pending(oid));
}

#[test]
fn a_refused_command_is_a_conflict_and_a_worker_exit_interrupts_a_pending_one() {
    let mut harness = Harness::new("session-refused");
    let (id, oid, log) = ready(&mut harness, "Refused");
    let compact = |id: &Id| {
        Request::SessionCompact(lade_protocol::client::SessionCompactCommand {
            objective_id: id.clone(),
            instructions: Some("short".into()),
        })
    };
    let request_id = deferred(&mut harness, compact(&id));
    let (command_id, command) = last_command(&log);
    assert!(matches!(
        command,
        SessionCommand::Compact { instructions: Some(ref i) } if i == "short"
    ));
    harness.worker_event(oid, result(command_id, false));
    let refused = harness
        .reply(&request_id)
        .expect("reply")
        .expect_err("refused");
    assert_eq!(refused.code, ErrorCode::Conflict);
    assert!(refused.detail.contains("refused by pi"));

    let request_id = deferred(&mut harness, compact(&id));
    harness.worker_event(
        oid,
        WorkerEvent::Exited {
            code: Some(1),
            signal: None,
        },
    );
    let interrupted = harness
        .reply(&request_id)
        .expect("reply")
        .expect_err("exit");
    assert_eq!(interrupted.code, ErrorCode::Interrupted);
}

#[test]
fn commands_need_an_idle_worker() {
    let mut harness = Harness::new("session-target");
    let (id, _) = harness.create_objective("Not started");
    let unavailable = harness
        .call(Request::SessionTree(ObjectiveSelector { objective_id: id }))
        .expect_err("no worker");
    assert_eq!(unavailable.code, ErrorCode::ExecutionUnavailable);
    assert_eq!(unavailable.detail, "Start or resume the agent first.");

    let (id, oid, _) = ready(&mut harness, "Running");
    harness.worker_event(
        oid,
        WorkerEvent::Body(WorkerBody::TurnState {
            state: lade_protocol::worker::TurnState::Running,
        }),
    );
    let navigate = harness
        .call(Request::SessionNavigate(
            lade_protocol::client::SessionNavigateCommand {
                objective_id: id.clone(),
                entry_id: "e".into(),
            },
        ))
        .expect_err("mid-turn");
    assert_eq!(
        navigate.detail,
        "Wait until the agent finishes its current turn."
    );
    let request_id = deferred(
        &mut harness,
        Request::SessionTree(ObjectiveSelector { objective_id: id }),
    );
    assert!(
        harness.reply(&request_id).is_none(),
        "the tree can be read while a turn runs"
    );
}

#[test]
fn a_pending_command_times_out() {
    let mut harness = Harness::new("session-timeout");
    let (id, oid, _) = ready(&mut harness, "Timeout");
    let request_id = deferred(
        &mut harness,
        Request::SessionTree(ObjectiveSelector { objective_id: id }),
    );
    harness.service.expire_session_commands();
    assert!(harness.reply(&request_id).is_none(), "not yet");
    let command = harness.service.live(oid).command.as_mut().expect("pending");
    command.since = Instant::now()
        .checked_sub(COMMAND_TIMEOUT + std::time::Duration::from_secs(1))
        .expect("uptime");
    harness.service.expire_session_commands();
    let timed_out = harness
        .reply(&request_id)
        .expect("reply")
        .expect_err("timeout");
    assert_eq!(timed_out.code, ErrorCode::Interrupted);
    assert!(harness.take_events().iter().any(|e| matches!(
        e,
        Event::ServiceNotice(n) if n.code == NoticeCode::SessionCommandTimeout
    )));
}

#[test]
fn dialogs_are_answered_once_and_only_for_the_current_generation() {
    let mut harness = Harness::new("session-dialogs");
    let (id, oid, log) = ready(&mut harness, "Dialogs");
    harness.worker_event(oid, changed(1, SessionChangeReason::Start, "anthropic"));
    let ui_id = Id::from_random_bytes([5; 16]);
    let open = |generation: u64| {
        WorkerEvent::Body(WorkerBody::UiRequest {
            ui_id: ui_id.clone(),
            generation: Counter::new(generation),
            prompt: UiPrompt::Confirm {
                title: "Run?".into(),
                message: "run the tool".into(),
            },
        })
    };
    harness.worker_event(oid, open(7));
    assert!(
        harness.service.live(oid).ui.is_empty(),
        "another generation's dialog is dropped"
    );
    harness.worker_event(oid, open(1));
    assert!(
        harness
            .take_events()
            .iter()
            .any(|e| matches!(e, Event::UiRequest(_)))
    );
    let respond = |generation: u64| {
        Request::UiRespond(UiRespondCommand {
            objective_id: id.clone(),
            ui_id: ui_id.clone(),
            generation: Counter::new(generation),
            answer: UiAnswer::Confirm { confirmed: true },
        })
    };
    let stale = harness.call(respond(0)).expect_err("stale");
    assert_eq!(stale.code, ErrorCode::StaleExecution);
    assert!(matches!(
        harness.call(respond(1)),
        Ok(ReplyData::UiRespond(_))
    ));
    assert!(matches!(
        log.lock().expect("log").sent.last(),
        Some(WorkerBody::UiRespond {
            answer: UiAnswer::Confirm { confirmed: true },
            ..
        })
    ));
    assert!(harness.take_events().iter().any(|e| matches!(
        e,
        Event::UiClosed(c) if c.reason == UiCloseReason::Answered
    )));
    let again = harness.call(respond(1)).expect_err("already answered");
    assert_eq!(again.code, ErrorCode::NotFound);
}

#[test]
fn a_new_pi_session_generation_replaces_older_dialogs() {
    let mut harness = Harness::new("session-replace");
    let (_, oid, _) = ready(&mut harness, "Replace");
    harness.worker_event(oid, changed(1, SessionChangeReason::Start, "anthropic"));
    harness.worker_event(
        oid,
        WorkerEvent::Body(WorkerBody::UiRequest {
            ui_id: Id::from_random_bytes([6; 16]),
            generation: Counter::new(1),
            prompt: UiPrompt::Input {
                title: "Name".into(),
                placeholder: String::new(),
            },
        }),
    );
    harness.take_events();
    harness.worker_event(oid, changed(2, SessionChangeReason::Navigate, "anthropic"));
    assert!(harness.service.live(oid).ui.is_empty());
    let events = harness.take_events();
    assert!(events.iter().any(|e| matches!(
        e,
        Event::UiClosed(c) if c.reason == UiCloseReason::Replaced
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        Event::SessionChanged(s) if s.generation.value() == 2 && s.pi_session_id.as_deref() == Some("pi-2")
    )));
}

#[test]
fn a_fork_rebinds_the_objective_to_a_new_agent_session() {
    let mut harness = Harness::new("session-fork");
    let (_, oid, _) = ready(&mut harness, "Fork");
    let session = |harness: &Harness| {
        harness.service.objectives[&oid]
            .objective()
            .execution()
            .session()
            .expect("session")
            .session_id
    };
    let before = session(&harness);
    harness.worker_event(oid, changed(2, SessionChangeReason::Fork, "anthropic"));
    let after = session(&harness);
    assert_ne!(before, after);
    assert_eq!(
        storage::pi_session_file(harness.service.store.conn(), after).expect("file"),
        Some("/s/2.jsonl".into())
    );
    harness.worker_event(oid, changed(3, SessionChangeReason::Navigate, "anthropic"));
    assert_eq!(session(&harness), after, "navigation keeps the session");
}

#[test]
fn configuring_a_stopped_agent_persists_the_selection_and_refuses_a_bad_model() {
    let mut harness = Harness::new("session-configure");
    let (id, oid, _) = ready(&mut harness, "Configure");
    harness
        .call(Request::ExecutionCancel(ObjectiveSelector {
            objective_id: id.clone(),
        }))
        .expect("cancel");
    harness.worker_event(
        oid,
        WorkerEvent::Exited {
            code: Some(0),
            signal: None,
        },
    );
    let configure = |provider: &str| {
        Request::SessionConfigure(SessionConfigureCommand {
            objective_id: id.clone(),
            model: model(provider, "second"),
            thinking: ThinkingLevel::Max,
        })
    };
    let Ok(ReplyData::SessionConfigure(state)) = harness.call(configure("openai")) else {
        panic!("configure");
    };
    assert_eq!(state.model, model("openai", "second"));
    assert_eq!(state.thinking, ThinkingLevel::Max);
    assert_eq!(harness.service.thinking_of(oid), Some(ThinkingLevel::Max));
    let stored = storage::load_thinking(harness.service.store.conn()).expect("load");
    assert_eq!(stored, [(oid, ThinkingLevel::Max)]);
    assert!(harness.call(configure("")).is_err(), "an invalid model");
    *harness.launcher.unavailable_provider.lock().expect("lock") = Some("gone".into());
    let refused = harness
        .call(configure("gone"))
        .expect_err("unavailable provider");
    assert_eq!(refused.code, ErrorCode::ExecutionUnavailable);
    assert_eq!(
        harness.service.objectives[&oid]
            .objective()
            .execution()
            .session()
            .expect("session")
            .model
            .provider(),
        "openai"
    );
}

#[test]
fn a_live_provider_change_needs_a_restart_but_a_model_change_goes_to_the_worker() {
    let mut harness = Harness::new("session-configure-live");
    let (id, _, log) = ready(&mut harness, "Live");
    let configure = |provider: &str| {
        Request::SessionConfigure(SessionConfigureCommand {
            objective_id: id.clone(),
            model: model(provider, "second"),
            thinking: ThinkingLevel::Medium,
        })
    };
    let refused = harness
        .call(configure("openai"))
        .expect_err("provider change");
    assert_eq!(refused.code, ErrorCode::InvalidRequest);
    deferred(&mut harness, configure("anthropic"));
    let (_, command) = last_command(&log);
    assert!(matches!(
        command,
        SessionCommand::Configure { ref model, thinking: ThinkingLevel::Medium } if model.id == "second"
    ));
}

#[test]
fn imports_copy_only_absolute_bounded_pi_session_files() {
    let mut harness = Harness::new("session-import");
    let (id, _, log) = ready(&mut harness, "Import");
    let file = harness.root().join("session.jsonl");
    let import = |path: &std::path::Path| {
        Request::SessionImport(SessionImportCommand {
            objective_id: id.clone(),
            path: WirePath::from_os_str(path.as_os_str()).expect("path"),
        })
    };
    let missing = harness.call(import(&file)).expect_err("missing");
    assert_eq!(missing.code, ErrorCode::NotFound);
    let relative = harness
        .call(import(std::path::Path::new("session.jsonl")))
        .expect_err("relative");
    assert_eq!(relative.code, ErrorCode::InvalidRequest);
    fs::write(&file, "{\"type\":\"message\"}\n").expect("write");
    let not_session = harness.call(import(&file)).expect_err("no header");
    assert!(not_session.detail.contains("session header"));
    let dir = harness.root().join("a-directory");
    fs::create_dir(&dir).expect("dir");
    assert!(harness.call(import(&dir)).is_err());

    let content = "{\"type\":\"session\",\"id\":\"x\"}\n{\"type\":\"message\"}\n";
    fs::write(&file, content).expect("write");
    deferred(&mut harness, import(&file));
    let (_, command) = last_command(&log);
    let SessionCommand::Import { path } = command else {
        panic!("an import command");
    };
    let copy = std::path::PathBuf::from(path);
    assert_ne!(copy, file, "the worker reads a private copy");
    assert_eq!(fs::read_to_string(&copy).expect("copy"), content);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        fs::metadata(&copy).expect("meta").permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn loaded_resources_are_summarized_once_per_generation() {
    let mut harness = Harness::new("session-resources");
    let (_, oid, _) = ready(&mut harness, "Resources");
    let resource = |kind, name: &str, source, status| LoadedResource {
        kind,
        name: name.into(),
        source,
        status,
        sha256: None,
        message: (status != LoadStatus::Loaded).then(|| "not approved".into()),
    };
    let loaded = || {
        WorkerEvent::Body(WorkerBody::ResourcesLoaded {
            generation: Counter::new(1),
            resources: vec![
                resource(
                    LoadedKind::Skill,
                    "review",
                    ResourceSource::LadeProfile,
                    LoadStatus::Loaded,
                ),
                resource(
                    LoadedKind::Context,
                    "AGENTS.md",
                    ResourceSource::Repository,
                    LoadStatus::Loaded,
                ),
                resource(
                    LoadedKind::Extension,
                    "net",
                    ResourceSource::LadeProfile,
                    LoadStatus::Refused,
                ),
            ],
        })
    };
    harness.take_events();
    harness.worker_event(oid, loaded());
    harness.worker_event(oid, loaded());
    let items: Vec<_> = harness
        .take_events()
        .into_iter()
        .filter_map(|e| match e {
            Event::ActivityAppended(a) => Some(a.items[0].clone()),
            _ => None,
        })
        .collect();
    assert_eq!(items.len(), 1, "the same generation is reported once");
    let text = &items[0].text;
    assert!(text.contains("Skills: review."), "{text}");
    assert!(text.contains("Context: AGENTS.md (repository)."), "{text}");
    assert!(text.contains("Extensions: none."), "{text}");
    assert!(
        text.contains("Extension net was refused: not approved"),
        "{text}"
    );
    assert!(items[0].is_error);
}

#[test]
fn the_start_extras_carry_the_saved_thinking_level_and_pi_session_file() {
    let mut harness = Harness::new("session-extras");
    let (_, oid, _) = ready(&mut harness, "Extras");
    assert_eq!(
        harness.service.start_extras(oid),
        (Some(ThinkingLevel::Low), Some("/s/1.jsonl".into()))
    );
    let state = harness
        .service
        .session_state(oid, SessionChangeReason::Start);
    assert_eq!(state.model, model("anthropic", "first"));
    assert_eq!(state.thinking, ThinkingLevel::Low);
    assert_eq!(state.generation.value(), 0, "no pi session reported yet");
}
