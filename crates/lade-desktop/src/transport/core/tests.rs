use std::sync::atomic::{AtomicU8, Ordering};

use lade_protocol::client::{self as wire, Empty, ErrorCode, EventFrame, Rejection, SafeAction};
use lade_protocol::types::{Counter, Id};

use super::*;
use crate::presentation::state::model::{
    ArtifactDigest, CaptureId, DeliveryMode, DraftId, Reference, ReferenceVersion, RepoPath, Side,
};
use crate::transport::requests::tests::{O, epoch, event, oid, snapshot_data};

pub(super) fn ids() -> impl FnMut() -> Id + Send + 'static {
    static NEXT: AtomicU8 = AtomicU8::new(1);
    move || Id::from_random_bytes([NEXT.fetch_add(1, Ordering::Relaxed); 16])
}

pub(super) fn connected() -> (Core, Step) {
    let mut core = Core::new(ids(), None);
    let step = core.on_welcome(epoch());
    (core, step)
}

pub(super) fn names(step: &Step) -> Vec<&'static str> {
    step.frames
        .iter()
        .map(|f| f.request.name().as_str())
        .collect()
}

pub(super) fn ok(frame: &RequestFrame, data: ReplyData) -> Reply {
    Reply::ok(frame.request_id.clone(), data)
}

pub(super) fn send_message() -> Intent {
    Intent::SendMessage {
        objective_id: oid(),
        text: "lade-fixture: write hello.txt hi".into(),
        mode: DeliveryMode::Prompt,
    }
}

pub(super) fn input_summary(operation_id: &Id) -> wire::InputSummary {
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
fn terminal_interactions_are_never_replayed_after_reconnect() {
    for intent in [
        Intent::OpenTerminal {
            objective_id: oid(),
            cols: 80,
            rows: 24,
        },
        Intent::TerminalInput {
            objective_id: oid(),
            terminal_id: O.into(),
            data: "echo hello\r".into(),
        },
        Intent::ResizeTerminal {
            objective_id: oid(),
            terminal_id: O.into(),
            cols: 100,
            rows: 30,
        },
        Intent::CloseTerminal {
            objective_id: oid(),
            terminal_id: O.into(),
        },
    ] {
        let (mut core, _) = connected();
        let sent = core.submit(intent.clone());
        assert_eq!(sent.frames.len(), 1);
        core.on_link_lost();
        let reconnected = core.on_welcome(epoch());
        assert!(
            reconnected.frames.iter().all(|frame| !frame
                .request
                .name()
                .as_str()
                .starts_with("terminal.")),
            "replayed {intent:?}: {:?}",
            names(&reconnected)
        );
    }
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
    let step = core.on_event(event(11));
    assert!(step.frames.is_empty());
    assert_eq!(step.updates.len(), 1);
    assert_eq!(core.last_sequence(), Some(11));
    assert!(core.on_event(event(11)).updates.is_empty());
    let step = core.on_event(event(13));
    assert_eq!(
        names(&step),
        ["snapshot", "objective.get", "drafts.list", "inputs.list"]
    );
    assert!(step.updates.contains(&StoreUpdate::WorktreeChanged {
        objective_id: oid()
    }));
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

pub(super) fn reference() -> Reference {
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
        ReplyData::DraftsList(wire::DraftList {
            drafts: Vec::new(),
            next_after: None,
        }),
    ));
    assert_eq!(names(&step), ["draft.save"]);
    assert!(step.frames[0].operation_id.is_some());
    assert_eq!(names(&core.submit(save.clone())), ["draft.save"]);
    core.on_link_lost();
    core.on_welcome(epoch());
    assert_eq!(names(&core.submit(save)), ["drafts.list"]);
}

#[test]
fn rejected_draft_reconciliation_blocks_saves_and_allows_an_explicit_retry() {
    let (mut core, _) = connected();
    let draft_id = DraftId::new("3b1c2d3e-4f50-4a61-8b72-839405a6b7c8");
    let save = Intent::SaveDraft {
        objective_id: oid(),
        draft_id: draft_id.clone(),
        expected_revision: 0,
        reference: reference(),
        body: "why?".into(),
    };
    let list = core.submit(save.clone()).frames.remove(0);
    let rejected = core.on_reply(Reply::rejected(
        list.request_id,
        Some(wire::RequestName::DraftsList),
        Rejection::new(
            ErrorCode::DurabilityFailure,
            SafeAction::None,
            "Draft storage is unavailable.",
        ),
    ));
    assert!(
        rejected.frames.is_empty(),
        "failed reconciliation sent {:?}",
        names(&rejected)
    );
    assert!(
        rejected.updates.iter().any(|update| matches!(
            update,
            StoreUpdate::DraftSaveRefused { draft_id: failed, .. } if failed == &draft_id
        )),
        "the editor must leave Saving and keep its text"
    );
    let retried = core.submit(save);
    assert_eq!(names(&retried), ["drafts.list"]);
    let reconciled = core.on_reply(ok(
        &retried.frames[0],
        ReplyData::DraftsList(wire::DraftList {
            drafts: Vec::new(),
            next_after: None,
        }),
    ));
    assert_eq!(names(&reconciled), ["draft.save"]);
}

#[test]
fn rejected_query_reaches_its_surface() {
    let (mut core, _) = connected();
    let step = core.submit(Intent::LoadChanges {
        objective_id: oid(),
        basis: crate::presentation::state::model::Basis::Working,
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

pub(super) fn wire_draft(byte: u8) -> wire::Draft {
    wire::Draft {
        draft_id: Id::from_random_bytes([byte; 16]),
        objective_id: Id::parse(O).unwrap(),
        destination_session_id: Id::from_random_bytes([99; 16]),
        reference: crate::transport::convert::to_reference(&reference()).unwrap(),
        body: format!("draft {byte}"),
        revision: Counter::new(1),
        state: wire::DraftState::Unsent,
        stale: false,
        input_id: None,
    }
}

pub(super) fn draft_list(drafts: Vec<wire::Draft>, more: bool) -> ReplyData {
    let next_after = more.then(|| wire::DraftCursor {
        created_at: lade_protocol::types::Timestamp::from_unix_ms(10).unwrap(),
        draft_id: drafts.last().unwrap().draft_id.clone(),
    });
    ReplyData::DraftsList(wire::DraftList { drafts, next_after })
}

pub(super) fn input_list(bytes: &[u8], next: Option<u64>) -> ReplyData {
    ReplyData::InputsList(wire::InputList {
        inputs: bytes
            .iter()
            .map(|byte| {
                let mut input = input_summary(&epoch());
                input.input_id = Id::from_random_bytes([*byte; 16]);
                input
            })
            .collect(),
        next_after: next.map(Counter::new),
    })
}

pub(super) fn live(core: &mut Core, event: Event) -> Step {
    core.on_event(EventFrame {
        service_epoch: epoch(),
        sequence: Counter::new(1),
        event,
    })
}
