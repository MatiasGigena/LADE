use lade_protocol::client::{self as wire, Empty, ErrorCode, Rejection, SafeAction};
use lade_protocol::types::{Counter, Id};

use crate::presentation::state::model::DeliveryState;
use crate::presentation::state::model::DraftId;
use crate::presentation::state::model::DraftState;
use crate::transport::core::tests::*;
use crate::transport::core::*;
use crate::transport::requests::tests::{
    epoch, event, input_summary, oid, snapshot_data, wire_draft,
};

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
        if drafts.len() == 1 && drafts[0].id.as_str() == wire_draft(12).draft_id.as_str())
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
        matches!(&event.updates[..], [StoreUpdate::DraftChanged(d)] if d.state == DraftState::Submitted)
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
        drafts.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(),
        [wire_draft(10), wire_draft(12), wire_draft(2)]
            .iter()
            .map(|d| d.draft_id.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(drafts[0].state, DraftState::Submitted);
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
    assert_eq!(inputs[0].delivery, DeliveryState::Received);
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
        if drafts.len() == 2 && drafts[1].state == DraftState::Submitted)
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

#[test]
fn a_history_reply_of_the_wrong_kind_or_objective_is_an_invalid_page() {
    let (mut core, _) = connected();
    let list = core
        .submit(Intent::LoadDrafts {
            objective_id: oid(),
        })
        .frames
        .remove(0);
    let step = core.on_reply(ok(&list, input_list(&[1], None)));
    assert!(
        !step
            .updates
            .iter()
            .any(|u| matches!(u, StoreUpdate::Drafts { .. })),
        "nothing published from a mismatched reply"
    );
    let list = core
        .submit(Intent::LoadInputs {
            objective_id: oid(),
        })
        .frames
        .remove(0);
    let ReplyData::InputsList(mut foreign) = input_list(&[1], None) else {
        unreachable!()
    };
    foreign.inputs[0].objective_id = Id::from_random_bytes([0x99; 16]);
    let step = core.on_reply(ok(&list, ReplyData::InputsList(foreign)));
    assert!(
        !step
            .updates
            .iter()
            .any(|u| matches!(u, StoreUpdate::Inputs { .. }))
    );
}

#[test]
fn an_input_cursor_that_does_not_advance_is_an_invalid_page() {
    let (mut core, _) = connected();
    let first = core
        .submit(Intent::LoadInputs {
            objective_id: oid(),
        })
        .frames
        .remove(0);
    let next = core.on_reply(ok(&first, input_list(&[1], Some(5))));
    let [frame] = next.frames.as_slice() else {
        panic!("one continuation: {:?}", names(&next));
    };
    let step = core.on_reply(ok(frame, input_list(&[2], Some(5))));
    assert!(
        step.frames.is_empty(),
        "no request for the same cursor again"
    );
    assert!(
        !step
            .updates
            .iter()
            .any(|u| matches!(u, StoreUpdate::Inputs { .. }))
    );
    let first = core
        .submit(Intent::LoadInputs {
            objective_id: oid(),
        })
        .frames
        .remove(0);
    let step = core.on_reply(ok(&first, input_list(&[], Some(1))));
    assert!(
        step.frames.is_empty(),
        "an empty page cannot carry a cursor"
    );
}
