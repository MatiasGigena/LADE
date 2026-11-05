use lade_protocol::client::{
    self as wire, ErrorCode, OperationState, OperationStatus, Outcome, Rejection, SafeAction,
};
use lade_protocol::types::{Counter, Id};

use crate::presentation::state::drafts::DraftEditor;
use crate::presentation::state::model::{DeliveryState, DraftId, DraftState, ModelRef, ReportCode};
use crate::presentation::state::store::PresentationState;
use crate::transport::core::tests::*;
use crate::transport::core::*;
use crate::transport::requests::tests::{O, epoch, input_summary, oid, wire_detail, wire_draft};

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
        matches!(refused.updates.as_slice(), [StoreUpdate::Notice(n)] if n.code == ReportCode::NotConnected)
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

/// The state after newer pages arrived: `historical` is now submitted and stale, and the
/// `message` input was received. Returns the state with the newer draft and input.
fn newer_page_state(
    historical: &wire::Draft,
    message: &RequestFrame,
) -> (PresentationState, wire::Draft, wire::InputSummary) {
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
    (state, current, input)
}

/// Saves `draft`'s body as its first revision and returns the request frame.
fn save_first_revision(core: &mut Core, draft: &wire::Draft) -> RequestFrame {
    core.submit(Intent::SaveDraft {
        objective_id: oid(),
        draft_id: DraftId::new(draft.draft_id.as_str()),
        expected_revision: 0,
        reference: reference(),
        body: draft.body.clone(),
    })
    .frames
    .remove(0)
}

fn apply(state: &mut PresentationState, step: Step) {
    for update in step.updates {
        state.apply(update);
    }
}

#[test]
fn recovered_historical_outcomes_do_not_override_newer_paginated_drafts_or_inputs() {
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
    let saved = save_first_revision(&mut core, &historical);
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
    let (mut state, current, input) = newer_page_state(&historical, &message);
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
    apply(
        &mut state,
        core.on_reply(ok(
            recover(saved.operation_id.as_ref().unwrap()),
            status(
                OperationState::Completed,
                Some(Outcome::Ok(Box::new(ReplyData::DraftSave(historical)))),
            ),
        )),
    );
    assert_eq!(
        state.drafts[&oid()][0].state,
        DraftState::Submitted,
        "a recovered acknowledgement is historical, not a current draft event"
    );
    apply(
        &mut state,
        core.on_reply(ok(
            recover(message.operation_id.as_ref().unwrap()),
            status(
                OperationState::Completed,
                Some(Outcome::Ok(Box::new(ReplyData::MessageSend(
                    input_summary(message.operation_id.as_ref().unwrap()),
                )))),
            ),
        )),
    );
    assert_eq!(state.inputs[&oid()][0].delivery, DeliveryState::Received);
    apply(
        &mut state,
        core.on_reply(ok(&draft_next, draft_list(vec![wire_draft(11)], false))),
    );
    apply(
        &mut state,
        core.on_reply(ok(&input_next, input_list(&[78], None))),
    );
    assert_eq!(state.drafts[&oid()][0].state, DraftState::Submitted);
    assert!(state.drafts[&oid()][0].stale);
    assert_eq!(state.inputs[&oid()][0].delivery, DeliveryState::Received);
}

/// Sends a message, loses the link before its reply and reconnects, returning the
/// `operation.get` that asks for the message's outcome.
fn recovering(core: &mut Core) -> RequestFrame {
    core.submit(send_message());
    core.on_link_lost();
    core.on_welcome(epoch())
        .frames
        .into_iter()
        .find(|f| f.request.name() == wire::RequestName::OperationGet)
        .expect("operation.get after reconnect")
}

fn status(state: OperationState, outcome: Option<Outcome>) -> ReplyData {
    ReplyData::OperationGet(OperationStatus { state, outcome })
}

fn warnings(step: &Step) -> Vec<(ReportCode, &str)> {
    step.updates
        .iter()
        .filter_map(|u| match u {
            StoreUpdate::Notice(n) if n.level == NoticeLevel::Warning => {
                Some((n.code, n.message.as_str()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_blocked_operation_is_reported_on_its_objective_and_reloads_it() {
    let (mut core, _) = connected();
    let recover = recovering(&mut core);
    let step = core.on_reply(ok(&recover, status(OperationState::Blocked, None)));
    assert_eq!(
        warnings(&step),
        [(
            ReportCode::Blocked,
            "“Send message” is blocked by the service."
        )]
    );
    assert!(step.updates.iter().any(|u| matches!(
        u,
        StoreUpdate::CommandStatus { objective_id, status: Some(s) }
            if objective_id == &oid() && s.code == ReportCode::Blocked
    )));
    assert!(names(&step).contains(&"objective.get"));
}

#[test]
fn a_refused_outcome_check_reports_the_reason_as_an_unknown_outcome() {
    let (mut core, _) = connected();
    let recover = recovering(&mut core);
    let step = core.on_reply(Reply::rejected(
        recover.request_id.clone(),
        None,
        Rejection::new(
            ErrorCode::InvalidRequest,
            SafeAction::None,
            "no such operation",
        ),
    ));
    let warnings = warnings(&step);
    let [(ReportCode::NotReceived, message)] = warnings.as_slice() else {
        panic!("one unknown-operation warning: {warnings:?}");
    };
    assert!(
        message.contains("check the outcome of “Send message”"),
        "{message}"
    );
}

#[test]
fn a_recovered_rejection_settles_like_the_original_reply() {
    let (mut core, _) = connected();
    let recover = recovering(&mut core);
    let rejection = Rejection::new(
        ErrorCode::InvalidLifecycle,
        SafeAction::None,
        "Start the agent first.",
    );
    let step = core.on_reply(ok(
        &recover,
        status(
            OperationState::Completed,
            Some(Outcome::Rejected {
                name: wire::RequestName::MessageSend,
                rejection,
            }),
        ),
    ));
    assert!(
        step.updates.iter().any(
            |u| matches!(u, StoreUpdate::Refused { objective_id, .. } if objective_id == &oid())
        ),
        "{:?}",
        step.updates
    );
    assert!(
        !step
            .updates
            .iter()
            .any(|u| matches!(u, StoreUpdate::Notice(n) if n.code == ReportCode::Recovered))
    );
}

#[test]
fn a_pending_operation_becomes_uncertain_after_the_last_recheck() {
    let (mut core, _) = connected();
    let mut check = recovering(&mut core);
    for _ in 0..MAX_RECHECKS {
        let step = core.on_reply(ok(&check, status(OperationState::Pending, None)));
        assert!(warnings(&step).is_empty());
        check = core.tick().frames.pop().expect("rechecked on tick");
    }
    let step = core.on_reply(ok(&check, status(OperationState::Accepted, None)));
    assert_eq!(
        warnings(&step),
        [(
            ReportCode::Uncertain,
            "The outcome of “Send message” is uncertain. Refresh before repeating it."
        )]
    );
    assert!(core.tick().frames.is_empty(), "no further checks");
}

/// Starts the agent, loses the link before the reply and reconnects. Returns the focused
/// `objective.get` and the `operation.get` the reconnect sent.
fn start_lost_on_reconnect(core: &mut Core) -> (RequestFrame, RequestFrame) {
    core.submit(Intent::LoadObjective {
        objective_id: oid(),
    });
    core.submit(Intent::StartExecution {
        objective_id: oid(),
        model: ModelRef {
            provider: "lade-fixture".into(),
            id: "fixture".into(),
        },
        thinking: None,
    });
    core.on_link_lost();
    let reconnect = core.on_welcome(epoch());
    let find = |name| {
        reconnect
            .frames
            .iter()
            .find(|f| f.request.name() == name)
            .cloned()
            .unwrap()
    };
    (
        find(wire::RequestName::ObjectiveGet),
        find(wire::RequestName::OperationGet),
    )
}

fn detail_at(revision: u64, title: &str) -> Box<wire::ObjectiveDetail> {
    let mut detail = wire_detail(O);
    detail.revision = Counter::new(revision);
    detail.title = title.into();
    Box::new(detail)
}

fn started(revision: u64) -> ReplyData {
    status(
        OperationState::Completed,
        Some(Outcome::Ok(Box::new(ReplyData::ExecutionStart(detail_at(
            revision,
            "as started",
        ))))),
    )
}

#[test]
fn a_recovered_objective_detail_older_than_the_reloaded_one_is_dropped() {
    let (mut core, _) = connected();
    let (reload, recover) = start_lost_on_reconnect(&mut core);
    let mut state = PresentationState::new(ConnectionState::Connected);
    apply(
        &mut state,
        core.on_reply(ok(
            &reload,
            ReplyData::ObjectiveGet(detail_at(5, "current")),
        )),
    );
    let step = core.on_reply(ok(&recover, started(4)));
    assert!(
        step.updates
            .iter()
            .any(|u| matches!(u, StoreUpdate::Notice(n) if n.code == ReportCode::Recovered))
    );
    apply(&mut state, step);
    let held = state.detail(&oid()).unwrap();
    assert_eq!(
        (held.summary.revision, held.summary.title.as_str()),
        (5, "current")
    );
    assert_eq!(state.objective(&oid()).unwrap().revision, 5);
}

#[test]
fn a_recovered_objective_detail_newer_than_the_reloaded_one_is_applied() {
    let (mut core, _) = connected();
    let (reload, recover) = start_lost_on_reconnect(&mut core);
    let mut state = PresentationState::new(ConnectionState::Connected);
    apply(
        &mut state,
        core.on_reply(ok(&reload, ReplyData::ObjectiveGet(detail_at(4, "before")))),
    );
    apply(&mut state, core.on_reply(ok(&recover, started(5))));
    let held = state.detail(&oid()).unwrap();
    assert_eq!(
        (held.summary.revision, held.summary.title.as_str()),
        (5, "as started")
    );
}
