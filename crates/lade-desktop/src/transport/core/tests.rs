use std::sync::atomic::{AtomicU8, Ordering};

use lade_protocol::client::{
    self as wire, Empty, ErrorCode, EventFrame, OperationStatus, Rejection, SafeAction,
};
use lade_protocol::types::{Counter, Id};

use super::*;
use crate::presentation::state::model::{
    ArtifactDigest, CaptureId, DeliveryMode, DraftId, Reference, ReferenceVersion, RepoPath, Side,
};
use crate::transport::requests::tests::{O, epoch, event, oid, snapshot_data};

fn ids() -> impl FnMut() -> Id + Send + 'static {
    static NEXT: AtomicU8 = AtomicU8::new(1);
    move || Id::from_random_bytes([NEXT.fetch_add(1, Ordering::Relaxed); 16])
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
        !step
            .updates
            .iter()
            .any(|u| matches!(u, StoreUpdate::InputChanged { .. }))
    );
    let current_inputs = step
        .frames
        .iter()
        .find(|f| matches!(f.request, Request::InputsList(_)))
        .unwrap();
    let current = core.on_reply(ok(
        current_inputs,
        ReplyData::InputsList(wire::InputList {
            inputs: vec![input_summary(&operation_id)],
            next_after: None,
        }),
    ));
    assert!(
        matches!(&current.updates[..], [StoreUpdate::Inputs { inputs, .. }] if inputs.len() == 1)
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

#[test]
fn draft_pages_do_not_reconcile_or_release_saves_before_completion() {
    let (mut core, _) = connected();
    let save = Intent::SaveDraft {
        objective_id: oid(),
        draft_id: DraftId::new("3b1c2d3e-4f50-4a61-8b72-839405a6b7c8"),
        expected_revision: 0,
        reference: reference(),
        body: "keep this local text".into(),
    };
    let first = core.submit(save).frames.remove(0);
    let draft = wire_draft(10);
    let cursor = wire::DraftCursor {
        created_at: lade_protocol::types::Timestamp::from_unix_ms(10).unwrap(),
        draft_id: draft.draft_id.clone(),
    };
    let page = core.on_reply(ok(
        &first,
        ReplyData::DraftsList(wire::DraftList {
            drafts: vec![draft.clone()],
            next_after: Some(cursor.clone()),
        }),
    ));
    assert!(
        page.updates.is_empty(),
        "partial pages must not replace the draft store"
    );
    assert_eq!(names(&page), ["drafts.list"]);
    let Request::DraftsList(query) = &page.frames[0].request else {
        unreachable!()
    };
    assert_eq!(query.after.as_ref(), Some(&cursor));
    assert!(
        core.submit(Intent::LoadDrafts {
            objective_id: oid()
        })
        .frames
        .is_empty()
    );
    let complete = core.on_reply(ok(
        &page.frames[0],
        ReplyData::DraftsList(wire::DraftList {
            drafts: vec![wire_draft(11)],
            next_after: None,
        }),
    ));
    assert_eq!(names(&complete), ["draft.save"]);
    assert!(
        matches!(&complete.updates[..], [StoreUpdate::Drafts { drafts, .. }] if drafts.len() == 2)
    );
}

fn wire_draft(byte: u8) -> wire::Draft {
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

fn draft_list(drafts: Vec<wire::Draft>, more: bool) -> ReplyData {
    let next_after = more.then(|| wire::DraftCursor {
        created_at: lade_protocol::types::Timestamp::from_unix_ms(10).unwrap(),
        draft_id: drafts.last().unwrap().draft_id.clone(),
    });
    ReplyData::DraftsList(wire::DraftList { drafts, next_after })
}

fn input_list(bytes: &[u8], next: Option<u64>) -> ReplyData {
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

fn live(core: &mut Core, event: Event) -> Step {
    core.on_event(EventFrame {
        service_epoch: epoch(),
        sequence: Counter::new(1),
        event,
    })
}

#[test]
fn failed_later_draft_page_keeps_local_text_and_requires_a_fresh_reconciliation() {
    use crate::presentation::state::drafts::{DraftEditor, SaveStatus};
    use crate::presentation::state::store::PresentationState;
    let (mut core, _) = connected();
    let durable = convert::draft(&wire_draft(30));
    let mut editor = DraftEditor::from_durable(&durable);
    editor.edit("my unsaved replacement", 0);
    let save = editor.poll(300, true).unwrap();
    let mut state = PresentationState::new(ConnectionState::Connected);
    state.drafts.insert(oid(), vec![editor]);
    let intent = Intent::SaveDraft {
        objective_id: save.objective_id,
        draft_id: save.draft_id,
        expected_revision: save.expected_revision,
        reference: save.reference,
        body: save.body,
    };
    let first = core.submit(intent.clone()).frames.remove(0);
    let partial = core.on_reply(ok(&first, draft_list(vec![wire_draft(10)], true)));
    assert!(partial.updates.is_empty());
    let rejected = core.on_reply(Reply::rejected(
        partial.frames[0].request_id.clone(),
        Some(wire::RequestName::DraftsList),
        Rejection::new(
            ErrorCode::DurabilityFailure,
            SafeAction::None,
            "Draft storage is unavailable.",
        ),
    ));
    assert!(rejected.frames.is_empty());
    assert!(
        !rejected
            .updates
            .iter()
            .any(|u| matches!(u, StoreUpdate::Drafts { .. }))
    );
    for update in rejected.updates {
        state.apply(update);
    }
    let retained = &state.drafts[&oid()];
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].body, "my unsaved replacement");
    assert!(matches!(
        retained[0].status(true),
        SaveStatus::Failed { .. }
    ));
    let retry = core.submit(intent);
    assert_eq!(names(&retry), ["drafts.list"]);
    assert!(matches!(&retry.frames[0].request, Request::DraftsList(q) if q.after.is_none()));
}

#[test]
fn disconnect_restarts_both_collections_and_ignores_old_continuations() {
    let (mut core, _) = connected();
    let first_d = core
        .submit(Intent::LoadDrafts {
            objective_id: oid(),
        })
        .frames
        .remove(0);
    let first_i = core
        .submit(Intent::LoadInputs {
            objective_id: oid(),
        })
        .frames
        .remove(0);
    let next_d = core
        .on_reply(ok(&first_d, draft_list(vec![wire_draft(10)], true)))
        .frames
        .remove(0);
    let next_i = core
        .on_reply(ok(&first_i, input_list(&[10], Some(1))))
        .frames
        .remove(0);
    core.on_link_lost();
    let restart = core.on_welcome(epoch());
    assert_eq!(
        restart
            .frames
            .iter()
            .filter(|f| matches!(&f.request,
        Request::DraftsList(q) if q.after.is_none()))
            .count(),
        1
    );
    assert_eq!(
        restart
            .frames
            .iter()
            .filter(|f| matches!(&f.request,
        Request::InputsList(q) if q.after.is_none()))
            .count(),
        1
    );
    for reply in [
        ok(&next_d, draft_list(vec![wire_draft(11)], false)),
        ok(&next_i, input_list(&[11], None)),
    ] {
        let ignored = core.on_reply(reply);
        assert!(ignored.updates.is_empty() && ignored.frames.is_empty());
    }
    let drafts = restart
        .frames
        .iter()
        .find(|f| matches!(f.request, Request::DraftsList(_)))
        .unwrap();
    let complete = core.on_reply(ok(drafts, draft_list(vec![wire_draft(12)], false)));
    assert!(
        matches!(&complete.updates[..], [StoreUpdate::Drafts { drafts, .. }]
        if drafts.len() == 1 && drafts[0].draft_id.as_str() == wire_draft(12).draft_id.as_str())
    );
}

#[test]
fn draft_completion_preserves_same_revision_events_additions_and_successful_deletions() {
    let (mut core, _) = connected();
    let first = core
        .submit(Intent::LoadDrafts {
            objective_id: oid(),
        })
        .frames
        .remove(0);
    let next = core
        .on_reply(ok(
            &first,
            draft_list(vec![wire_draft(10), wire_draft(11)], true),
        ))
        .frames
        .remove(0);
    let mut submitted = wire_draft(10);
    submitted.state = wire::DraftState::Submitted;
    submitted.input_id = Some(Id::from_random_bytes([75; 16]));
    let event = live(&mut core, Event::DraftChanged(submitted));
    assert!(
        matches!(&event.updates[..], [StoreUpdate::DraftChanged(d)] if d.state == crate::presentation::state::model::DraftState::Submitted)
    );
    live(&mut core, Event::DraftChanged(wire_draft(2)));
    let deleted = core
        .submit(Intent::DeleteDraft {
            draft_id: DraftId::new(wire_draft(11).draft_id.as_str()),
            expected_revision: 1,
        })
        .frames
        .remove(0);
    core.on_reply(ok(&deleted, ReplyData::DraftDelete(Empty {})));
    let complete = core.on_reply(ok(&next, draft_list(vec![wire_draft(12)], false)));
    let [StoreUpdate::Drafts { drafts, .. }] = &complete.updates[..] else {
        panic!("complete drafts")
    };
    assert_eq!(
        drafts
            .iter()
            .map(|d| d.draft_id.as_str())
            .collect::<Vec<_>>(),
        [wire_draft(10), wire_draft(12), wire_draft(2)]
            .iter()
            .map(|d| d.draft_id.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        drafts[0].state,
        crate::presentation::state::model::DraftState::Submitted
    );
    assert!(drafts[0].input_id.is_some());
}

#[test]
fn input_pages_preserve_chronological_order_and_live_delivery_changes() {
    let (mut core, _) = connected();
    let first = core
        .submit(Intent::LoadInputs {
            objective_id: oid(),
        })
        .frames
        .remove(0);
    assert!(
        core.submit(Intent::LoadInputs {
            objective_id: oid()
        })
        .frames
        .is_empty()
    );
    let partial = core.on_reply(ok(&first, input_list(&[10], Some(1))));
    assert!(partial.updates.is_empty());
    assert!(
        matches!(&partial.frames[0].request, Request::InputsList(q) if q.after.as_ref().map(Counter::value) == Some(1))
    );
    let mut delivered = input_summary(&epoch());
    delivered.input_id = Id::from_random_bytes([10; 16]);
    delivered.delivery = wire::DeliveryState::Received;
    live(&mut core, Event::InputChanged(delivered));
    let mut added = input_summary(&epoch());
    added.input_id = Id::from_random_bytes([12; 16]);
    live(&mut core, Event::InputChanged(added));
    let complete = core.on_reply(ok(&partial.frames[0], input_list(&[11], None)));
    let [StoreUpdate::Inputs { inputs, .. }] = &complete.updates[..] else {
        panic!("complete inputs")
    };
    assert_eq!(
        inputs
            .iter()
            .map(|i| i.input_id.as_str())
            .collect::<Vec<_>>(),
        [10, 11, 12]
            .iter()
            .map(|b| Id::from_random_bytes([*b; 16]).to_string())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        inputs[0].delivery,
        crate::presentation::state::model::DeliveryState::Received
    );
}

#[test]
fn a_later_page_reflects_earlier_events_instead_of_replaying_stale_metadata() {
    let (mut core, _) = connected();
    let first = core
        .submit(Intent::LoadDrafts {
            objective_id: oid(),
        })
        .frames
        .remove(0);
    let next = core
        .on_reply(ok(&first, draft_list(vec![wire_draft(10)], true)))
        .frames
        .remove(0);
    live(&mut core, Event::DraftChanged(wire_draft(11)));
    let mut newer = wire_draft(11);
    newer.state = wire::DraftState::Submitted;
    let complete = core.on_reply(ok(&next, draft_list(vec![newer], false)));
    assert!(
        matches!(&complete.updates[..], [StoreUpdate::Drafts { drafts, .. }]
        if drafts.len() == 2 && drafts[1].state == crate::presentation::state::model::DraftState::Submitted)
    );
}

#[test]
fn event_gaps_and_epoch_changes_restart_background_pages_and_ignore_old_replies() {
    for foreign_epoch in [false, true] {
        let (mut core, welcome) = connected();
        core.on_reply(ok(&welcome.frames[0], snapshot_data(10)));
        let first = core
            .submit(Intent::LoadDrafts {
                objective_id: oid(),
            })
            .frames
            .remove(0);
        let first_i = core
            .submit(Intent::LoadInputs {
                objective_id: oid(),
            })
            .frames
            .remove(0);
        let old = core
            .on_reply(ok(&first, draft_list(vec![wire_draft(10)], true)))
            .frames
            .remove(0);
        let old_i = core
            .on_reply(ok(&first_i, input_list(&[10], Some(1))))
            .frames
            .remove(0);
        let mut changed = event(12);
        if foreign_epoch {
            changed.service_epoch = Id::from_random_bytes([9; 16]);
        }
        let restart = core.on_event(changed);
        assert_eq!(names(&restart), ["snapshot", "drafts.list", "inputs.list"]);
        assert!(
            !restart
                .updates
                .iter()
                .any(|u| matches!(u, StoreUpdate::Drafts { .. } | StoreUpdate::Inputs { .. }))
        );
        assert!(matches!(&restart.frames[1].request, Request::DraftsList(q) if q.after.is_none()));
        assert!(matches!(&restart.frames[2].request, Request::InputsList(q) if q.after.is_none()));
        assert!(
            core.on_reply(ok(&old, draft_list(vec![wire_draft(11)], false)))
                .updates
                .is_empty()
        );
        assert!(
            core.on_reply(ok(&old_i, input_list(&[11], None)))
                .updates
                .is_empty()
        );
        let complete = core.on_reply(ok(
            &restart.frames[1],
            draft_list(vec![wire_draft(12)], false),
        ));
        assert!(
            matches!(&complete.updates[..], [StoreUpdate::Drafts { drafts, .. }] if drafts.len() == 1)
        );
    }
}

#[test]
fn malformed_or_unsendable_continuations_fail_without_publishing_partial_history() {
    for unsendable in [false, true] {
        let (mut core, _) = connected();
        let first = core
            .submit(Intent::LoadInputs {
                objective_id: oid(),
            })
            .frames
            .remove(0);
        let next = core
            .on_reply(ok(&first, input_list(&[10], Some(1))))
            .frames
            .remove(0);
        let failed = if unsendable {
            core.unsendable(
                &next.request_id,
                &Rejection::new(ErrorCode::InvalidRequest, SafeAction::None, "frame refused"),
            )
        } else {
            core.on_reply(ok(&next, input_list(&[11], Some(1))))
        };
        assert!(failed.frames.is_empty());
        assert!(matches!(&failed.updates[..], [StoreUpdate::Notice(_)]));
        let retry = core.submit(Intent::LoadInputs {
            objective_id: oid(),
        });
        assert!(matches!(&retry.frames[0].request, Request::InputsList(q) if q.after.is_none()));
    }
}

#[test]
fn disconnect_during_deferred_save_keeps_text_and_reenables_saving_after_complete_reload() {
    use crate::presentation::state::drafts::{DraftEditor, SaveStatus};
    use crate::presentation::state::store::PresentationState;
    let (mut core, _) = connected();
    let draft = convert::draft(&wire_draft(30));
    let mut editor = DraftEditor::from_durable(&draft);
    editor.edit("text edited while paging", 0);
    let save = editor.poll(300, true).unwrap();
    let first = core
        .submit(Intent::SaveDraft {
            objective_id: save.objective_id,
            draft_id: save.draft_id,
            expected_revision: save.expected_revision,
            reference: save.reference,
            body: save.body,
        })
        .frames
        .remove(0);
    let mut state = PresentationState::new(ConnectionState::Connected);
    state.drafts.insert(oid(), vec![editor]);
    let partial = core.on_reply(ok(&first, draft_list(vec![wire_draft(10)], true)));
    assert!(partial.updates.is_empty());
    core.on_link_lost();
    state.apply(StoreUpdate::Connection(ConnectionState::Reconnecting {
        attempt: 0,
    }));
    assert_eq!(state.drafts[&oid()][0].body, "text edited while paging");
    assert_eq!(
        state.drafts[&oid()][0].status(false),
        SaveStatus::Disconnected
    );
    let reconnect = core.on_welcome(epoch());
    for update in reconnect.updates {
        state.apply(update);
    }
    assert!(
        state.drafts.get_mut(&oid()).unwrap()[0]
            .poll(1000, true)
            .is_none()
    );
    let first = reconnect
        .frames
        .iter()
        .find(|f| matches!(f.request, Request::DraftsList(_)))
        .unwrap();
    let complete = core.on_reply(ok(first, draft_list(vec![wire_draft(30)], false)));
    for update in complete.updates {
        state.apply(update);
    }
    let retained = &mut state.drafts.get_mut(&oid()).unwrap()[0];
    assert_eq!(retained.body, "text edited while paging");
    assert_eq!(retained.status(true), SaveStatus::Unsaved);
    let save = retained
        .poll(1000, true)
        .expect("reconciliation clears the lost in-flight gate");
    let sent = core.submit(Intent::SaveDraft {
        objective_id: save.objective_id,
        draft_id: save.draft_id,
        expected_revision: save.expected_revision,
        reference: save.reference,
        body: save.body,
    });
    assert_eq!(names(&sent), ["draft.save"]);
}

#[test]
fn recovered_historical_outcomes_do_not_override_newer_paginated_drafts_or_inputs() {
    use crate::presentation::state::drafts::DraftEditor;
    use crate::presentation::state::store::PresentationState;
    let (mut core, _) = connected();
    core.submit(Intent::LoadObjective {
        objective_id: oid(),
    });
    let list = core
        .submit(Intent::LoadDrafts {
            objective_id: oid(),
        })
        .frames
        .remove(0);
    core.on_reply(ok(&list, draft_list(vec![], false)));
    let historical = wire_draft(10);
    let saved = core
        .submit(Intent::SaveDraft {
            objective_id: oid(),
            draft_id: DraftId::new(historical.draft_id.as_str()),
            expected_revision: 0,
            reference: reference(),
            body: historical.body.clone(),
        })
        .frames
        .remove(0);
    let message = core.submit(send_message()).frames.remove(0);
    core.submit(Intent::LoadInputs {
        objective_id: oid(),
    });
    core.on_link_lost();
    let reconnect = core.on_welcome(epoch());
    let draft_first = reconnect
        .frames
        .iter()
        .find(|f| matches!(f.request, Request::DraftsList(_)))
        .unwrap();
    let input_first = reconnect
        .frames
        .iter()
        .find(|f| matches!(f.request, Request::InputsList(_)))
        .unwrap();
    let mut current = historical.clone();
    current.state = wire::DraftState::Submitted;
    current.stale = true;
    current.input_id = Some(Id::from_random_bytes([77; 16]));
    let mut state = PresentationState::new(ConnectionState::Connected);
    state.drafts.insert(
        oid(),
        vec![DraftEditor::from_durable(&convert::draft(&current))],
    );
    let mut input = input_summary(message.operation_id.as_ref().unwrap());
    input.delivery = wire::DeliveryState::Received;
    state.inputs.insert(oid(), vec![convert::input(&input)]);
    let draft_next = core
        .on_reply(ok(draft_first, draft_list(vec![current], true)))
        .frames
        .remove(0);
    let input_next = core
        .on_reply(ok(
            input_first,
            ReplyData::InputsList(wire::InputList {
                inputs: vec![input.clone()],
                next_after: Some(Counter::new(1)),
            }),
        ))
        .frames
        .remove(0);
    let recover = |operation_id: &Id| {
        reconnect.frames.iter().find(|f|
        matches!(&f.request, Request::OperationGet(q) if &q.operation_id == operation_id)).unwrap()
    };
    let draft_recovered = core.on_reply(ok(
        recover(saved.operation_id.as_ref().unwrap()),
        ReplyData::OperationGet(OperationStatus {
            state: OperationState::Completed,
            outcome: Some(Outcome::Ok(Box::new(ReplyData::DraftSave(historical)))),
        }),
    ));
    for update in draft_recovered.updates {
        state.apply(update);
    }
    assert_eq!(
        state.drafts[&oid()][0].state,
        crate::presentation::state::model::DraftState::Submitted,
        "a recovered acknowledgement is historical, not a current draft event"
    );
    let input_recovered = core.on_reply(ok(
        recover(message.operation_id.as_ref().unwrap()),
        ReplyData::OperationGet(OperationStatus {
            state: OperationState::Completed,
            outcome: Some(Outcome::Ok(Box::new(ReplyData::MessageSend(
                input_summary(message.operation_id.as_ref().unwrap()),
            )))),
        }),
    ));
    for update in input_recovered.updates {
        state.apply(update);
    }
    assert_eq!(
        state.inputs[&oid()][0].delivery,
        crate::presentation::state::model::DeliveryState::Received
    );
    for update in core
        .on_reply(ok(&draft_next, draft_list(vec![wire_draft(11)], false)))
        .updates
    {
        state.apply(update);
    }
    for update in core
        .on_reply(ok(&input_next, input_list(&[78], None)))
        .updates
    {
        state.apply(update);
    }
    assert_eq!(
        state.drafts[&oid()][0].state,
        crate::presentation::state::model::DraftState::Submitted
    );
    assert!(state.drafts[&oid()][0].stale);
    assert_eq!(
        state.inputs[&oid()][0].delivery,
        crate::presentation::state::model::DeliveryState::Received
    );
}

#[test]
fn complete_history_has_no_total_record_quota() {
    let (mut core, _) = connected();
    let mut frame = core
        .submit(Intent::LoadInputs {
            objective_id: oid(),
        })
        .frames
        .remove(0);
    let mut expected = Vec::new();
    for page in 0..4_u64 {
        let inputs = (0..200_u64)
            .map(|position| {
                let sequence = page * 200 + position + 1;
                let mut bytes = [0; 16];
                bytes[..8].copy_from_slice(&sequence.to_be_bytes());
                let mut input = input_summary(&epoch());
                input.input_id = Id::from_random_bytes(bytes);
                expected.push(input.input_id.to_string());
                input
            })
            .collect();
        let step = core.on_reply(ok(
            &frame,
            ReplyData::InputsList(wire::InputList {
                inputs,
                next_after: (page < 3).then(|| Counter::new((page + 1) * 200)),
            }),
        ));
        if page < 3 {
            assert!(step.updates.is_empty());
            assert_eq!(names(&step), ["inputs.list"]);
            frame = step.frames[0].clone();
        } else {
            assert!(step.frames.is_empty());
            let [StoreUpdate::Inputs { inputs, .. }] = &step.updates[..] else {
                panic!("complete history")
            };
            assert_eq!(
                inputs
                    .iter()
                    .map(|i| i.input_id.as_str().to_owned())
                    .collect::<Vec<_>>(),
                expected
            );
        }
    }
}
