use std::sync::atomic::{AtomicU8, Ordering};

use lade_protocol::client::{
    self as wire, Empty, ErrorCode, EventFrame, OperationStatus, Rejection, SafeAction,
    SnapshotData,
};
use lade_protocol::types::{Counter, Id};

use super::*;
use crate::presentation::model::{
    ArtifactDigest, CaptureId, DeliveryMode, DraftId, Reference, ReferenceVersion, RepoPath, Side,
};
use crate::transport::requests::tests::{O, wire_detail};

fn ids() -> impl FnMut() -> Id + Send + 'static {
    static NEXT: AtomicU8 = AtomicU8::new(1);
    move || Id::from_random_bytes([NEXT.fetch_add(1, Ordering::Relaxed); 16])
}

fn epoch() -> Id {
    Id::parse("eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee").unwrap()
}

fn oid() -> ObjectiveId {
    ObjectiveId::new(O)
}

fn connected() -> (Core, Step) {
    let mut core = Core::new(ids(), None);
    let step = core.on_welcome(epoch());
    (core, step)
}

fn names(step: &Step) -> Vec<&'static str> {
    step.frames
        .iter()
        .map(|f| f.request.name().as_str())
        .collect()
}

fn ok(frame: &RequestFrame, data: ReplyData) -> Reply {
    Reply::ok(frame.request_id.clone(), data)
}

fn snapshot_data(sequence: u64) -> ReplyData {
    ReplyData::Snapshot(SnapshotData {
        service_epoch: epoch(),
        sequence: Counter::new(sequence),
        workspaces: Vec::new(),
        objectives: Vec::new(),
        settings: wire::Settings {
            verification_commands: Vec::new(),
            default_model: None,
        },
    })
}

fn event(sequence: u64) -> EventFrame {
    EventFrame {
        service_epoch: epoch(),
        sequence: Counter::new(sequence),
        event: Event::ObjectiveChanged(wire_detail(O).summary()),
    }
}

fn send_message() -> Intent {
    Intent::SendMessage {
        objective_id: oid(),
        text: "lade-fixture: write hello.txt hi".into(),
        mode: DeliveryMode::Prompt,
    }
}

fn input_summary(operation_id: &Id) -> wire::InputSummary {
    wire::InputSummary {
        input_id: Id::from_random_bytes([0x77; 16]),
        objective_id: Id::parse(O).unwrap(),
        operation_id: operation_id.clone(),
        mode: lade_protocol::types::DeliveryMode::Prompt,
        text_preview: "lade-fixture: write".into(),
        accepted_at: lade_protocol::types::Timestamp::parse("2026-10-10T12:00:00.000Z").unwrap(),
        delivery: wire::DeliveryState::Queued,
        blocked_reason: None,
    }
}

#[test]
fn welcome_requests_a_snapshot_and_reports_connected() {
    let (core, step) = connected();
    assert!(core.is_connected());
    assert_eq!(names(&step), ["snapshot"]);
    assert_eq!(
        step.updates,
        vec![StoreUpdate::Connection(ConnectionState::Connected)]
    );
    assert!(step.frames[0].operation_id.is_none());
}

#[test]
fn open_path_is_sent_once_after_connecting() {
    let mut core = Core::new(ids(), Some(PathBuf::from("/tmp/repo")));
    let step = core.on_welcome(epoch());
    assert_eq!(names(&step), ["snapshot", "workspace.open"]);
    assert!(step.frames[1].operation_id.is_some());
    core.on_link_lost();
    let again = core.on_welcome(epoch());
    assert!(!names(&again).contains(&"workspace.open"));
}

#[test]
fn mutating_requests_carry_one_operation_id_and_queries_none() {
    let (mut core, _) = connected();
    let step = core.submit(send_message());
    let frame = &step.frames[0];
    assert_eq!(frame.request.name(), wire::RequestName::MessageSend);
    assert!(frame.operation_id.is_some());
    assert_ne!(frame.operation_id.as_ref(), Some(&frame.request_id));
    let query = core.submit(Intent::LoadObjective {
        objective_id: oid(),
    });
    assert!(query.frames[0].operation_id.is_none());
}

#[test]
fn lost_command_reply_is_recovered_with_the_same_operation_id() {
    let (mut core, _) = connected();
    let sent = core.submit(send_message()).frames.remove(0);
    let operation_id = sent.operation_id.clone().unwrap();
    core.on_link_lost();
    assert_eq!(core.uncertain().len(), 1);
    // Commands are never queued blindly while disconnected.
    let refused = core.submit(Intent::Freeze {
        objective_id: oid(),
    });
    assert!(refused.frames.is_empty());
    assert!(
        matches!(refused.updates.as_slice(), [StoreUpdate::Notice(n)] if n.code == "not_connected")
    );

    let step = core.on_welcome(epoch());
    let recover = step
        .frames
        .iter()
        .find(|f| f.request.name() == wire::RequestName::OperationGet)
        .expect("operation.get after reconnect");
    let Request::OperationGet(query) = &recover.request else {
        unreachable!()
    };
    assert_eq!(
        query.operation_id, operation_id,
        "same operation, never a fresh one"
    );
    assert!(
        !names(&step).contains(&"message.send"),
        "the command is not resent"
    );
    assert!(step.updates.iter().any(|u| matches!(
        u,
        StoreUpdate::CommandStatus { status: Some(s), .. } if s.message.starts_with("Checking outcome")
    )));

    // The recorded outcome is applied exactly like the lost reply.
    let outcome = Outcome::Ok(Box::new(ReplyData::MessageSend(input_summary(
        &operation_id,
    ))));
    let step = core.on_reply(ok(
        recover,
        ReplyData::OperationGet(OperationStatus {
            state: OperationState::Completed,
            outcome: Some(outcome),
        }),
    ));
    assert!(
        step.updates
            .iter()
            .any(|u| matches!(u, StoreUpdate::InputChanged { .. }))
    );
    assert!(core.uncertain().is_empty());
}

#[test]
fn unknown_operation_is_reported_not_resent() {
    let (mut core, _) = connected();
    core.submit(send_message());
    core.on_link_lost();
    let step = core.on_welcome(epoch());
    let recover = step
        .frames
        .iter()
        .find(|f| f.request.name() == wire::RequestName::OperationGet)
        .unwrap()
        .clone();
    let step = core.on_reply(ok(
        &recover,
        ReplyData::OperationGet(OperationStatus {
            state: OperationState::Unknown,
            outcome: None,
        }),
    ));
    assert!(!names(&step).contains(&"message.send"));
    assert!(step.updates.iter().any(
        |u| matches!(u, StoreUpdate::Notice(n) if n.message.contains("did not reach the service"))
    ));
}

#[test]
fn running_operation_is_rechecked_on_tick() {
    let (mut core, _) = connected();
    core.submit(send_message());
    core.on_link_lost();
    let step = core.on_welcome(epoch());
    let recover = step.frames.last().unwrap().clone();
    let step = core.on_reply(ok(
        &recover,
        ReplyData::OperationGet(OperationStatus {
            state: OperationState::Pending,
            outcome: None,
        }),
    ));
    assert!(!names(&step).contains(&"operation.get"));
    assert_eq!(names(&core.tick()), ["operation.get"]);
    assert!(core.tick().frames.is_empty());
}

#[test]
fn queries_while_disconnected_are_sent_after_reconnect() {
    let mut core = Core::new(ids(), None);
    let step = core.submit(Intent::LoadDrafts {
        objective_id: oid(),
    });
    assert!(step.frames.is_empty() && step.updates.is_empty());
    let step = core.on_welcome(epoch());
    assert_eq!(names(&step), ["snapshot", "drafts.list"]);
}

#[test]
fn sequence_gap_triggers_resnapshot_and_requery_of_the_open_objective() {
    let (mut core, step) = connected();
    let snapshot = step.frames[0].clone();
    core.submit(Intent::LoadObjective {
        objective_id: oid(),
    });
    core.on_reply(ok(&snapshot, snapshot_data(10)));
    assert_eq!(core.last_sequence(), Some(10));
    // In order: applied, no requests.
    let step = core.on_event(event(11));
    assert!(step.frames.is_empty());
    assert_eq!(step.updates.len(), 1);
    assert_eq!(core.last_sequence(), Some(11));
    // Old or duplicate: ignored.
    assert!(core.on_event(event(11)).updates.is_empty());
    // Gap: the event still applies, then snapshot + the open Objective are re-queried.
    let step = core.on_event(event(13));
    assert_eq!(
        names(&step),
        ["snapshot", "objective.get", "drafts.list", "inputs.list"]
    );
    assert!(step.updates.contains(&StoreUpdate::WorktreeChanged {
        objective_id: oid()
    }));
    // A second gap before the snapshot answers does not stack snapshots.
    assert!(!names(&core.on_event(event(20))).contains(&"snapshot"));
}

#[test]
fn foreign_epoch_event_resyncs() {
    let (mut core, step) = connected();
    core.on_reply(ok(&step.frames[0], snapshot_data(1)));
    let mut frame = event(2);
    frame.service_epoch = Id::from_random_bytes([9; 16]);
    assert!(names(&core.on_event(frame)).contains(&"snapshot"));
}

fn reference() -> Reference {
    Reference {
        artifact_digest: ArtifactDigest::new("a".repeat(64)),
        path: RepoPath::utf8("hello.txt"),
        version: ReferenceVersion::Working {
            capture_id: CaptureId::new("6b1c2d3e-4f50-4a61-8b72-839405a6b7c8"),
        },
        side: Side::New,
        byte_start: 0,
        byte_end: 2,
        selected_text: "hi".into(),
        line_hint: Some(1),
    }
}

#[test]
fn draft_saves_wait_for_drafts_list_on_each_connection() {
    let (mut core, _) = connected();
    let save = Intent::SaveDraft {
        objective_id: oid(),
        draft_id: DraftId::new("3b1c2d3e-4f50-4a61-8b72-839405a6b7c8"),
        expected_revision: 0,
        reference: reference(),
        body: "why?".into(),
    };
    let step = core.submit(save.clone());
    assert_eq!(
        names(&step),
        ["drafts.list"],
        "save deferred until reconciled"
    );
    let list = step.frames[0].clone();
    let step = core.on_reply(ok(
        &list,
        ReplyData::DraftsList(wire::DraftList { drafts: Vec::new() }),
    ));
    assert_eq!(names(&step), ["draft.save"]);
    assert!(step.frames[0].operation_id.is_some());
    // Reconciled: the next save goes straight out ...
    assert_eq!(names(&core.submit(save.clone())), ["draft.save"]);
    // ... until the link drops; then drafts are listed again first.
    core.on_link_lost();
    core.on_welcome(epoch());
    assert_eq!(names(&core.submit(save)), ["drafts.list"]);
}

#[test]
fn rejected_query_reaches_its_surface() {
    let (mut core, _) = connected();
    let step = core.submit(Intent::LoadChanges {
        objective_id: oid(),
        basis: crate::presentation::model::Basis::Working,
    });
    let reply = Reply::rejected(
        step.frames[0].request_id.clone(),
        Some(wire::RequestName::ChangesList),
        Rejection::new(ErrorCode::NotFound, SafeAction::None, "no worktree yet"),
    );
    let step = core.on_reply(reply);
    assert!(matches!(
        step.updates.as_slice(),
        [StoreUpdate::ChangesFailed { message, .. }]
            if message == "Couldn't load Change Set. No worktree yet."
    ));
    assert_eq!(core.pending_count(), 1, "only the welcome snapshot remains");
}

#[test]
fn replies_to_unknown_requests_are_ignored() {
    let (mut core, _) = connected();
    let step = core.on_reply(Reply::ok(
        Id::from_random_bytes([0x42; 16]),
        ReplyData::DraftDelete(Empty {}),
    ));
    assert!(step.updates.is_empty() && step.frames.is_empty());
}
