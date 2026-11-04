use super::*;
use crate::domain::ids::Revision;
use crate::domain::inspection::{CurrentContent, Reference};
use crate::domain::objectives::{Lifecycle, Objective};
use crate::error::DomainError;

use crate::domain::execution::ModelRef;
use crate::domain::inspection::ArtifactDigest;
use crate::testing::{
    active_objective, defined_objective, digest, draft_id, input_id, objective_id, operation_id,
    reference_at, session_id, ts,
};

const FILE: &str = "fn main() {\r\n\tlet é = 1;\r\n\tprintln!(\"{é}\");\r\n}\r\n";

fn reference_for(selection: &str) -> Reference {
    let start = FILE.find(selection).expect("selection in file");
    reference_at(1, start, start + selection.len(), selection).expect("reference")
}

fn save_new(objective: &Objective, id: u128, selection: &str, body: &str) -> Draft {
    let request = SaveDraft {
        draft_id: draft_id(id),
        objective_id: objective.id(),
        expected_revision: Revision::new(0),
        reference: reference_for(selection),
        body: body.to_string(),
    };
    save_draft(None, objective, request, ts(3)).expect("create draft")
}

fn acceptance(objective: &Objective) -> Acceptance {
    let session = objective.execution().session().expect("session");
    Acceptance {
        input_id: input_id(1),
        operation_id: operation_id(1),
        session_id: session.session_id,
        association_revision: session.association_revision,
        mode: InputMode::Steer,
    }
}

fn present<'a>(digest: &'a ArtifactDigest, bytes: &'a [u8]) -> CurrentContent<'a> {
    CurrentContent::Present { digest, bytes }
}

#[test]
fn draft_create_binds_current_destination() {
    let (objective, _) = active_objective();
    let draft = save_new(&objective, 1, "\tlet é", "rename");
    assert_eq!(draft.revision(), Revision::INITIAL);
    assert_eq!(draft.record().destination_session_id, session_id(1));
    let without_session = defined_objective();
    let request = SaveDraft {
        draft_id: draft_id(2),
        objective_id: without_session.id(),
        expected_revision: Revision::new(0),
        reference: reference_for("\tlet é"),
        body: String::new(),
    };
    assert_eq!(
        save_draft(None, &without_session, request, ts(3)),
        Err(DomainError::ExecutionUnavailable)
    );
}

#[test]
fn draft_cas_save_conflicts_and_never_wraps() {
    let (objective, _) = active_objective();
    let draft = save_new(&objective, 1, "\tlet é", "first");
    let update = |expected: u64, body: &str| SaveDraft {
        draft_id: draft_id(1),
        objective_id: objective.id(),
        expected_revision: Revision::new(expected),
        reference: reference_for("\tlet é"),
        body: body.to_string(),
    };
    let saved =
        save_draft(Some(draft.clone()), &objective, update(1, "second"), ts(4)).expect("cas ok");
    assert_eq!(saved.revision(), Revision::new(2));
    assert_eq!(saved.record().body, "second");
    assert_eq!(
        save_draft(
            Some(saved.clone()),
            &objective,
            update(1, "lost update"),
            ts(5)
        ),
        Err(DomainError::Conflict(
            "The draft was changed elsewhere. Refresh and try again."
        ))
    );
    assert_eq!(
        save_draft(Some(saved.clone()), &objective, update(0, "dup"), ts(5)),
        Err(DomainError::Conflict("This draft already exists."))
    );
    assert_eq!(
        save_draft(None, &objective, update(3, "missing"), ts(5)),
        Err(DomainError::NotFound("draft"))
    );

    let mut record = saved.into_record();
    record.revision = Revision::new(u64::MAX);
    let mut exhausted = Draft::restore(record).expect("restore");
    let before = exhausted.clone();
    assert!(matches!(
        exhausted.update(
            Revision::new(u64::MAX),
            reference_for("\tlet é"),
            "x".into(),
            ts(6)
        ),
        Err(DomainError::LimitReached(_))
    ));
    assert_eq!(exhausted, before);
}

#[test]
fn submitted_drafts_cannot_be_edited_or_deleted() {
    let (objective, _) = active_objective();
    let mut draft = save_new(&objective, 1, "\tlet é", "fix");
    let current_digest = digest(1);
    let mut batch = [BatchEntry {
        draft: &mut draft,
        revision: Revision::INITIAL,
        current: present(&current_digest, b""),
    }];
    accept_feedback(&objective, acceptance(&objective), &mut batch, None, ts(5)).expect("accepted");
    assert_eq!(
        draft.record().state,
        DraftState::Submitted {
            input_id: input_id(1)
        }
    );
    assert!(matches!(
        draft.check_delete(Revision::INITIAL),
        Err(DomainError::InvalidLifecycle(_))
    ));
    assert!(
        draft
            .update(
                Revision::INITIAL,
                reference_for("\tlet é"),
                "x".into(),
                ts(6)
            )
            .is_err()
    );
}

#[test]
fn accepted_feedback_renders_byte_exact_references() {
    let (objective, _) = active_objective();
    let mut first = save_new(&objective, 1, "\tlet é = 1;", "Use a descriptive name.");
    let mut second = save_new(
        &objective,
        2,
        "\tprintln!(\"{é}\");\r\n}",
        "Drop the debug print.",
    );
    let changed = digest(9);
    let edited = format!("{FILE}// appended later\n");
    let mut batch = [
        BatchEntry {
            draft: &mut first,
            revision: Revision::INITIAL,
            current: present(&changed, edited.as_bytes()),
        },
        BatchEntry {
            draft: &mut second,
            revision: Revision::INITIAL,
            current: present(&changed, edited.as_bytes()),
        },
    ];
    let input = accept_feedback(
        &objective,
        acceptance(&objective),
        &mut batch,
        Some("Please revise:".into()),
        ts(5),
    )
    .expect("accepted");
    assert_eq!(input.items.len(), 2);
    assert_eq!(input.delivery, DeliveryState::Queued);
    let label = input.items[0].reference.version().label();
    let start = FILE.find("\tlet").expect("offset");
    let expected_first = format!(
        "file src/main.rs ({label}, bytes {start}\u{2013}{}, line ~1):\n> \tlet é = 1;\nUse a descriptive name.",
        start + "\tlet é = 1;".len()
    );
    assert!(input.rendered_text.starts_with("Please revise:\n\n"));
    assert!(
        input.rendered_text.contains(&expected_first),
        "{}",
        input.rendered_text
    );
    assert!(
        input
            .rendered_text
            .contains("> \tprintln!(\"{é}\");\n> }\nDrop the debug print.")
    );
    assert!(input.rendered_text.contains(" bytes "));
}

#[test]
fn any_stale_item_blocks_the_whole_batch() {
    let (objective, _) = active_objective();
    let mut d1 = save_new(&objective, 1, "fn main()", "a");
    let mut d2 = save_new(&objective, 2, "\tlet é", "b");
    let mut d3 = save_new(&objective, 3, "}\r\n", "c");
    let other = digest(5);
    let shifted = FILE.replacen("é", "e", 1);
    let before = (d1.clone(), d2.clone(), d3.clone());
    let mut batch = [
        BatchEntry {
            draft: &mut d1,
            revision: Revision::INITIAL,
            current: present(&other, shifted.as_bytes()),
        },
        BatchEntry {
            draft: &mut d2,
            revision: Revision::INITIAL,
            current: present(&other, shifted.as_bytes()),
        },
        BatchEntry {
            draft: &mut d3,
            revision: Revision::INITIAL,
            current: CurrentContent::Missing,
        },
    ];
    let result = accept_feedback(&objective, acceptance(&objective), &mut batch, None, ts(5));
    assert_eq!(
        result,
        Err(DomainError::StaleReference {
            drafts: vec![draft_id(2), draft_id(3)]
        })
    );
    assert_eq!((d1, d2, d3), before, "no draft is marked submitted");
}

#[test]
fn batch_rejects_mismatched_revisions_duplicates_and_foreign_drafts() {
    let (objective, _) = active_objective();
    let mut d1 = save_new(&objective, 1, "fn main()", "a");
    let same = digest(1);
    let mut batch = [BatchEntry {
        draft: &mut d1,
        revision: Revision::new(2),
        current: present(&same, b""),
    }];
    assert_eq!(
        accept_feedback(&objective, acceptance(&objective), &mut batch, None, ts(5)),
        Err(DomainError::Conflict(
            "A draft changed after you reviewed it. Refresh and try again."
        ))
    );
    assert_eq!(
        accept_feedback(&objective, acceptance(&objective), &mut [], None, ts(5)),
        Err(DomainError::InvalidRequest(
            "Select at least one draft to submit."
        ))
    );
    let mut empty_body = save_new(&objective, 2, "fn main()", "  ");
    let mut batch = [BatchEntry {
        draft: &mut empty_body,
        revision: Revision::INITIAL,
        current: present(&same, b""),
    }];
    assert!(matches!(
        accept_feedback(&objective, acceptance(&objective), &mut batch, None, ts(5)),
        Err(DomainError::InvalidRequest(_))
    ));
    let mut record = save_new(&objective, 3, "fn main()", "x").into_record();
    record.objective_id = objective_id(99);
    let mut foreign = Draft::restore(record).expect("restore");
    let mut batch = [BatchEntry {
        draft: &mut foreign,
        revision: Revision::INITIAL,
        current: present(&same, b""),
    }];
    assert_eq!(
        accept_feedback(&objective, acceptance(&objective), &mut batch, None, ts(5)),
        Err(DomainError::NotFound("draft"))
    );
}

#[test]
fn batch_size_matches_the_wire_contract() {
    fn entries<'a>(drafts: &'a mut [Draft], same: &'a ArtifactDigest) -> Vec<BatchEntry<'a>> {
        drafts
            .iter_mut()
            .map(|draft| BatchEntry {
                draft,
                revision: Revision::INITIAL,
                current: present(same, b""),
            })
            .collect()
    }
    let (objective, _) = active_objective();
    let same = digest(1);
    let mut drafts: Vec<Draft> = (0..=256)
        .map(|n| save_new(&objective, 1000 + n, "fn main()", "fix"))
        .collect();
    assert_eq!(
        accept_feedback(
            &objective,
            acceptance(&objective),
            &mut entries(&mut drafts, &same),
            None,
            ts(5)
        ),
        Err(DomainError::LimitReached(
            "Too many drafts for one submission. Submit fewer at once."
        ))
    );
    let mut full = entries(&mut drafts[..256], &same);
    let input = accept_feedback(&objective, acceptance(&objective), &mut full, None, ts(5))
        .expect("a full wire batch is accepted");
    assert_eq!(input.items.len(), 256);
}

#[test]
fn destination_must_match_current_association() {
    let (mut objective, generation) = active_objective();
    let mut draft = save_new(&objective, 1, "fn main()", "a");
    let old = acceptance(&objective);
    objective.request_stop(ts(4)).expect("stop");
    objective
        .observe_stopped(generation, ts(4))
        .expect("stopped");
    objective
        .replace_session(
            session_id(2),
            ModelRef::new("p", "m").expect("model"),
            ts(4),
        )
        .expect("replace");
    let same = digest(1);
    let mut batch = [BatchEntry {
        draft: &mut draft,
        revision: Revision::INITIAL,
        current: present(&same, b""),
    }];
    assert_eq!(
        accept_feedback(&objective, old, &mut batch, None, ts(5)),
        Err(DomainError::StaleAssociation)
    );
    let new = acceptance(&objective);
    assert_eq!(
        accept_feedback(&objective, new, &mut batch, None, ts(5)),
        Err(DomainError::StaleAssociation)
    );
    batch[0]
        .draft
        .retarget(Revision::INITIAL, session_id(2), ts(6))
        .expect("explicit retarget");
    batch[0].revision = Revision::new(2);
    let input = accept_feedback(&objective, new, &mut batch, None, ts(7)).expect("accepted");
    assert_eq!(
        input.delivery,
        DeliveryState::Blocked {
            reason: BlockedReason::ExecutionUnavailable
        }
    );
}

#[test]
fn frozen_objective_rejects_input_until_returned() {
    let (mut objective, generation) = active_objective();
    objective.request_stop(ts(4)).expect("stop");
    objective
        .observe_stopped(generation, ts(4))
        .expect("stopped");
    objective
        .transition(&[Lifecycle::Frozen], ts(5))
        .expect("freeze");
    assert!(matches!(
        accept_message(&objective, acceptance(&objective), "do more".into(), ts(6)),
        Err(DomainError::InvalidLifecycle(_))
    ));
    objective
        .transition(&[Lifecycle::Active], ts(7))
        .expect("return");
    let input = accept_message(&objective, acceptance(&objective), "do more".into(), ts(8))
        .expect("accepted");
    assert_eq!(
        input.delivery,
        DeliveryState::Blocked {
            reason: BlockedReason::ExecutionUnavailable
        }
    );
    assert!(input.items.is_empty());
    assert_eq!(input.rendered_text, "do more");
}

#[test]
fn delivery_transitions() {
    let (mut objective, generation) = active_objective();
    let mut input =
        accept_message(&objective, acceptance(&objective), "go".into(), ts(5)).expect("accepted");
    assert_eq!(
        input.dispatch(&objective, false),
        Ok(DispatchDecision::Dispatch { generation })
    );
    assert!(
        input.dispatch(&objective, false).is_err(),
        "already dispatched"
    );
    assert_eq!(input.mark_received(generation), Ok(()));
    assert_eq!(input.delivery, DeliveryState::Received { generation });

    let mut lost =
        accept_message(&objective, acceptance(&objective), "go".into(), ts(5)).expect("accepted");
    lost.dispatch(&objective, false).expect("dispatch");
    assert!(lost.mark_worker_lost());
    assert_eq!(lost.delivery, DeliveryState::Uncertain { generation });
    assert!(
        lost.dispatch(&objective, false).is_err(),
        "never redispatched automatically"
    );
    assert!(lost.requeue().is_err(), "uncertain is not blocked");
    objective.interrupt_after_restart(ts(6)).expect("restart");
    let next = objective.resume_execution(ts(7)).expect("resume");
    assert_eq!(lost.mark_received(next), Err(DomainError::StaleExecution));
    assert_eq!(
        lost.mark_received(generation),
        Ok(()),
        "reconciled evidence"
    );
}

#[test]
fn dispatch_revalidates_and_blocks() {
    let (mut objective, generation) = active_objective();
    let mut stale =
        accept_message(&objective, acceptance(&objective), "go".into(), ts(5)).expect("accepted");
    assert_eq!(
        stale.dispatch(&objective, true),
        Ok(DispatchDecision::Blocked {
            reason: BlockedReason::StaleReference
        })
    );
    let mut queued =
        accept_message(&objective, acceptance(&objective), "go".into(), ts(5)).expect("accepted");
    objective.request_stop(ts(6)).expect("stop");
    assert_eq!(
        queued.dispatch(&objective, false),
        Ok(DispatchDecision::Blocked {
            reason: BlockedReason::ExecutionUnavailable
        })
    );
    objective
        .observe_stopped(generation, ts(6))
        .expect("stopped");
    let next = objective.resume_execution(ts(7)).expect("resume");
    queued.requeue().expect("explicit continuation");
    assert_eq!(
        queued.dispatch(&objective, false),
        Ok(DispatchDecision::Wait)
    );
    assert_eq!(queued.delivery, DeliveryState::Queued);
    objective.observe_worker_ready(next, ts(8)).expect("ready");
    assert_eq!(
        queued.dispatch(&objective, false),
        Ok(DispatchDecision::Dispatch { generation: next })
    );
}

#[test]
fn dispatch_blocks_on_replaced_association() {
    let (mut objective, generation) = active_objective();
    let mut input =
        accept_message(&objective, acceptance(&objective), "go".into(), ts(5)).expect("accepted");
    objective.request_stop(ts(6)).expect("stop");
    objective
        .observe_stopped(generation, ts(6))
        .expect("stopped");
    objective
        .replace_session(
            session_id(3),
            ModelRef::new("p", "m").expect("model"),
            ts(7),
        )
        .expect("replace");
    assert_eq!(
        input.dispatch(&objective, false),
        Ok(DispatchDecision::Blocked {
            reason: BlockedReason::StaleAssociation
        })
    );
}
