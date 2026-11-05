use std::time::Instant;

use lade_protocol::client::{ObjectiveSelector, Request, RequestName};
use lade_protocol::types::ThinkingLevel;
use lade_protocol::worker::{MAX_ITEM_TEXT_BYTES, TurnState, WorkerErrorCode};

use super::writable::human_bytes;
use super::*;
use crate::service::ResourceLimits;
use crate::service::test_harness::Harness;
use crate::supervision::WorkerEvent;

fn model(provider: &str) -> Model {
    Model {
        provider: provider.into(),
        id: "model".into(),
    }
}

fn start(harness: &mut Harness, id: &Id, provider: &str) -> Result<ReplyData, Rejection> {
    harness.call(Request::ExecutionStart(ExecutionStartCommand {
        objective_id: id.clone(),
        model: model(provider),
        thinking: Some(ThinkingLevel::High),
    }))
}

fn state(harness: &Harness, oid: ObjectiveId) -> ExecutionState {
    harness.service.objectives[&oid]
        .objective()
        .execution()
        .state()
        .clone()
}

fn ready(harness: &mut Harness, oid: ObjectiveId) {
    harness.worker_event(
        oid,
        WorkerEvent::Body(WorkerBody::Ready {
            pi_session_id: "pi-session".into(),
            session_file: "/sessions/pi.jsonl".into(),
        }),
    );
}

/// An Objective whose worker was launched and reported ready.
fn running(harness: &mut Harness, title: &str) -> (Id, ObjectiveId) {
    let (id, oid) = harness.create_objective(title);
    start(harness, &id, "anthropic").expect("start");
    ready(harness, oid);
    (id, oid)
}

fn activity_events(events: &[Event]) -> Vec<&Event> {
    events
        .iter()
        .filter(|e| matches!(e, Event::ActivityAppended(_) | Event::ActivityDelta(_)))
        .collect()
}

#[test]
fn starting_provisions_the_worktree_and_launches_a_worker_for_the_new_session() {
    let mut harness = Harness::new("execution-start");
    let (id, oid) = harness.create_objective("Start");
    let Ok(ReplyData::ExecutionStart(detail)) = start(&mut harness, &id, "anthropic") else {
        panic!("start");
    };
    assert!(detail.session.is_some());
    let (request, _) = harness.last_worker();
    let st = &harness.service.objectives[&oid];
    let worktree = harness.service.paths.worktrees.join(oid.to_string());
    assert_eq!(st.row.worktree.as_ref(), Some(&worktree));
    assert!(
        worktree.join("README").is_file(),
        "the worktree is checked out"
    );
    assert_eq!(
        st.row.git_branch.as_deref(),
        Some(st.objective().record().branch.as_str())
    );
    assert_eq!(request.worktree, worktree);
    assert_eq!(request.provider, "anthropic");
    assert_eq!(
        request.binding.execution_generation,
        st.objective().execution().generation().value()
    );
    let WorkerBody::Start {
        resume,
        thinking,
        resources,
        system_note,
        ..
    } = &request.start
    else {
        panic!("a start body");
    };
    assert!(!resume);
    assert_eq!(*thinking, Some(ThinkingLevel::High));
    assert!(
        resources.is_some(),
        "the approved profile snapshot is attached"
    );
    assert!(system_note.contains("Start"));
    assert_eq!(state(&harness, oid), ExecutionState::Starting);

    ready(&mut harness, oid);
    assert_eq!(state(&harness, oid), ExecutionState::Idle);
    let session = harness.service.objectives[&oid]
        .objective()
        .execution()
        .session()
        .expect("session")
        .session_id;
    assert_eq!(
        storage::pi_session_file(harness.service.store.conn(), session).expect("file"),
        Some("/sessions/pi.jsonl".into())
    );
}

#[test]
fn preflight_refuses_the_fixture_provider_and_unavailable_providers_before_recording() {
    let mut harness = Harness::new("execution-preflight");
    let (id, oid) = harness.create_objective("Preflight");
    let fixture = start(&mut harness, &id, FIXTURE_PROVIDER).expect_err("fixture");
    assert_eq!(fixture.code, ErrorCode::ExecutionUnavailable);
    assert_eq!(fixture.detail, FIXTURE_REFUSED);
    *harness.launcher.unavailable_provider.lock().expect("lock") = Some("anthropic".into());
    let unavailable = start(&mut harness, &id, "anthropic").expect_err("unavailable");
    assert_eq!(unavailable.code, ErrorCode::ExecutionUnavailable);
    assert!(unavailable.detail.contains("no credentials"));
    assert_eq!(state(&harness, oid), ExecutionState::None);
    assert!(harness.launcher.launches.lock().expect("lock").is_empty());
}

#[test]
fn the_worker_limit_counts_other_live_agents() {
    let limits = ResourceLimits {
        max_workers: 1,
        ..ResourceLimits::default()
    };
    let mut harness = Harness::with_limits("execution-limit", limits);
    running(&mut harness, "First");
    let (second, oid) = harness.create_objective("Second");
    let refused = start(&mut harness, &second, "anthropic").expect_err("over the limit");
    assert_eq!(refused.code, ErrorCode::LimitReached);
    assert!(refused.detail.contains("1 of 1 agents"));
    assert_eq!(state(&harness, oid), ExecutionState::None);
}

#[test]
fn a_failed_launch_fails_the_execution_and_explains_why() {
    let mut harness = Harness::new("execution-launch-failed");
    let (id, oid) = harness.create_objective("Launch");
    *harness.launcher.refuse.lock().expect("lock") = Some("no bubblewrap".into());
    start(&mut harness, &id, "anthropic").expect("the start itself is accepted");
    let ExecutionState::Failed { reason } = state(&harness, oid) else {
        panic!("expected a failed execution");
    };
    assert!(reason.contains("sandbox_unavailable"), "{reason}");
    assert!(harness.service.rt(oid).control.is_none());
    let events = harness.take_events();
    assert!(events.iter().any(|e| matches!(
        e,
        Event::ServiceNotice(n) if n.code == NoticeCode::ExecutionFailed
    )));
    assert_eq!(
        activity_events(&events).len(),
        1,
        "one system activity item"
    );
    assert!(
        harness.service.objectives[&oid].row.worktree.is_some(),
        "the provisioned worktree is kept for a resume"
    );
}

#[test]
fn cancelling_aborts_the_worker_and_its_exit_completes_the_stop() {
    let mut harness = Harness::new("execution-cancel");
    let (id, oid) = running(&mut harness, "Cancel");
    let (_, log) = harness.last_worker();
    let cancel = harness.call(Request::ExecutionCancel(ObjectiveSelector {
        objective_id: id,
    }));
    assert!(cancel.is_ok());
    assert_eq!(state(&harness, oid), ExecutionState::Stopping);
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
    assert!(matches!(
        state(&harness, oid),
        ExecutionState::Cancelled { .. }
    ));
}

#[test]
fn an_unexpected_exit_fails_the_execution_with_the_status_or_signal() {
    for (code, signal, expected) in [
        (Some(3), None, "worker exited unexpectedly (status 3)"),
        (None, Some(9), "worker terminated by signal 9"),
        (None, None, "worker exited unexpectedly"),
    ] {
        let mut harness = Harness::new("execution-exit");
        let (_, oid) = running(&mut harness, "Exit");
        harness.worker_event(oid, WorkerEvent::Exited { code, signal });
        assert_eq!(
            state(&harness, oid),
            ExecutionState::Failed {
                reason: expected.into()
            }
        );
    }
}

#[test]
fn events_of_an_older_generation_are_ignored() {
    let mut harness = Harness::new("execution-stale");
    let (_, oid) = running(&mut harness, "Stale");
    let generation = harness.service.objectives[&oid]
        .objective()
        .execution()
        .generation()
        .value();
    harness
        .service
        .on_supervisor(crate::supervision::SupervisorEvent {
            binding: LaunchBinding {
                objective_id: oid.to_string(),
                execution_generation: generation - 1,
            },
            event: WorkerEvent::Exited {
                code: Some(1),
                signal: None,
            },
        });
    assert_eq!(state(&harness, oid), ExecutionState::Idle);
}

#[test]
fn turns_move_the_execution_between_running_and_idle() {
    let mut harness = Harness::new("execution-turns");
    let (_, oid) = running(&mut harness, "Turns");
    harness.worker_event(
        oid,
        WorkerEvent::Body(WorkerBody::TurnState {
            state: TurnState::Running,
        }),
    );
    assert_eq!(state(&harness, oid), ExecutionState::Running);
    harness.worker_event(
        oid,
        WorkerEvent::Body(WorkerBody::TurnState {
            state: TurnState::Idle,
        }),
    );
    assert_eq!(state(&harness, oid), ExecutionState::Idle);
    assert!(
        harness.service.objectives[&oid].capture.is_some(),
        "an ended turn captures the worktree"
    );
}

#[test]
fn activity_items_are_bounded_and_deltas_append_until_the_item_is_full() {
    let mut harness = Harness::new("execution-activity");
    let (_, oid) = running(&mut harness, "Activity");
    let (_, other) = harness.create_objective("Other");
    harness.take_events();
    let item = Id::from_random_bytes([7; 16]);
    harness.worker_event(
        oid,
        WorkerEvent::Body(WorkerBody::ActivityItem {
            item_id: item.clone(),
            role: lade_protocol::types::ActivityRole::Assistant,
            title: "t".repeat(2000),
            text: "hello".into(),
            is_error: false,
            truncated: false,
            tool_name: None,
        }),
    );
    let delta = |text: &str| {
        WorkerEvent::Body(WorkerBody::ActivityDelta {
            item_id: item.clone(),
            append_text: text.into(),
        })
    };
    harness.worker_event(oid, delta(" world"));
    harness.service.activity_delta(other, &item, " ignored");
    assert_eq!(
        harness.service.live_text(&item),
        Some(("hello world", false))
    );
    harness.worker_event(oid, delta(&"x".repeat(MAX_ITEM_TEXT_BYTES)));
    let (text, truncated) = harness.service.live_text(&item).expect("live");
    assert_eq!(text.len(), MAX_ITEM_TEXT_BYTES);
    assert!(truncated);

    harness.service.flush_deltas();
    let events = harness.take_events();
    let deltas: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::ActivityDelta(d) => Some(d.append_text.len()),
            _ => None,
        })
        .collect();
    assert_eq!(
        deltas,
        [MAX_ITEM_TEXT_BYTES - 5],
        "consecutive deltas coalesce"
    );
    let Some(Event::ActivityAppended(appended)) = events.first() else {
        panic!("the item is announced first");
    };
    assert_eq!(appended.items[0].title.chars().count(), 1024);

    harness.service.flush_activity();
    let (owner, row) = storage::load_activity_item(harness.service.store.conn(), &item)
        .expect("load")
        .expect("stored");
    assert_eq!(owner, oid);
    assert_eq!(row.text.len(), MAX_ITEM_TEXT_BYTES);
    assert!(row.truncated);
}

#[test]
fn an_item_id_belongs_to_the_objective_that_reported_it_first() {
    let mut harness = Harness::new("execution-item-owner");
    let (_, first) = running(&mut harness, "First");
    let (_, second) = running(&mut harness, "Second");
    let item = |text: &str| {
        WorkerEvent::Body(WorkerBody::ActivityItem {
            item_id: Id::from_random_bytes([8; 16]),
            role: lade_protocol::types::ActivityRole::Tool,
            title: "bash".into(),
            text: text.into(),
            is_error: false,
            truncated: false,
            tool_name: Some("bash".into()),
        })
    };
    harness.worker_event(first, item("mine"));
    harness.worker_event(second, item("stolen"));
    let id = Id::from_random_bytes([8; 16]);
    assert_eq!(harness.service.live_text(&id), Some(("mine", false)));
}

#[test]
fn worker_errors_are_announced_and_kept_in_the_activity() {
    let mut harness = Harness::new("execution-error");
    let (_, oid) = running(&mut harness, "Error");
    harness.take_events();
    harness.worker_event(
        oid,
        WorkerEvent::Body(WorkerBody::Error {
            code: WorkerErrorCode::ProviderFailed,
            message: "rate limited".into(),
        }),
    );
    let events = harness.take_events();
    assert!(events.iter().any(|e| matches!(
        e,
        Event::ServiceNotice(n) if n.code == NoticeCode::WorkerError
    )));
    let item = events.iter().find_map(|e| match e {
        Event::ActivityAppended(a) => Some(&a.items[0]),
        _ => None,
    });
    let item = item.expect("activity item");
    assert!(item.is_error);
    assert_eq!(item.text, "rate limited");
    assert_eq!(
        state(&harness, oid),
        ExecutionState::Idle,
        "the execution goes on"
    );
}

#[test]
fn exceeding_writable_storage_stops_the_current_generation_only() {
    let mut harness = Harness::new("execution-storage");
    let (_, oid) = running(&mut harness, "Storage");
    let (_, log) = harness.last_worker();
    let generation = harness.service.objectives[&oid]
        .objective()
        .execution()
        .generation()
        .value();
    let limit = harness.service.storage.writable_limit;
    harness.service.storage.measuring = true;
    harness.service.storage_measured(vec![StorageUsage {
        oid,
        generation: generation + 1,
        bytes: limit + 1,
    }]);
    assert!(!harness.service.storage.measuring);
    assert_eq!(
        state(&harness, oid),
        ExecutionState::Idle,
        "another generation"
    );
    harness.service.storage_measured(vec![StorageUsage {
        oid,
        generation,
        bytes: limit,
    }]);
    assert_eq!(
        state(&harness, oid),
        ExecutionState::Idle,
        "at the limit is allowed"
    );
    harness.service.storage_measured(vec![StorageUsage {
        oid,
        generation,
        bytes: limit + 1,
    }]);
    harness.settle();
    let ExecutionState::Failed { reason } = state(&harness, oid) else {
        panic!("expected the agent to be stopped");
    };
    assert!(reason.starts_with(LIMIT_REACHED), "{reason}");
    assert!(log.lock().expect("log").terminated);
}

#[test]
fn storage_checks_measure_only_live_agents_with_a_worktree() {
    let mut harness = Harness::new("execution-storage-check");
    harness.create_objective("Idle");
    harness.service.check_storage();
    assert!(!harness.service.storage.measuring, "nothing to measure");
    let (_, oid) = running(&mut harness, "Live");
    harness.service.check_storage();
    assert!(harness.service.storage.measuring);
    harness.service.check_storage();
    harness.settle();
    assert!(!harness.service.storage.measuring);
    assert_eq!(state(&harness, oid), ExecutionState::Idle);
}

#[test]
fn a_freeze_waiting_too_long_for_the_worker_is_interrupted() {
    let mut harness = Harness::new("execution-timeout");
    let (_, oid) = harness.create_objective("Timeout");
    let ctx = Harness::ctx(RequestName::ReviewFreeze);
    let request_id = ctx.request_id.clone();
    let recent = Harness::ctx(RequestName::ReviewFreeze);
    let (_, other) = harness.create_objective("Recent");
    let long_ago = Instant::now()
        .checked_sub(STOP_TIMEOUT + Duration::from_secs(1))
        .expect("uptime");
    let rt = harness.service.rt(oid);
    rt.busy = Some("freeze");
    rt.waiting_freeze = Some((ctx, long_ago));
    harness.service.rt(other).waiting_freeze = Some((recent, Instant::now()));
    harness.service.check_timeouts();
    let rejected = harness
        .reply(&request_id)
        .expect("reply")
        .expect_err("timed out");
    assert_eq!(rejected.code, ErrorCode::Interrupted);
    assert!(harness.service.rt(oid).busy.is_none());
    assert!(harness.service.rt(other).waiting_freeze.is_some());
}

#[test]
fn byte_counts_read_in_the_largest_whole_unit() {
    assert_eq!(human_bytes(0), "0 KiB");
    assert_eq!(human_bytes(1), "1 KiB");
    assert_eq!(human_bytes(1025), "2 KiB");
    assert_eq!(human_bytes(3 << 19), "1.5 MiB");
    assert_eq!(human_bytes(2 << 30), "2.0 GiB");
    assert_eq!(human_bytes(u64::MAX), "1717986918.3 GiB");
}
