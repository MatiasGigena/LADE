use super::*;
use crate::domain::execution::ModelRef;
use crate::domain::feedback::BlockedReason;
use crate::domain::feedback::fixtures::{FILE, acceptance, present, reference_for, save_new};
use crate::domain::inspection::ArtifactDigest;
use crate::testing::{
    active_objective, defined_objective, digest, draft_id, objective_id, session_id,
    stopped_objective, ts,
};

fn entry<'a>(draft: &'a mut Draft, current: &'a ArtifactDigest) -> BatchEntry<'a> {
    BatchEntry {
        revision: draft.revision(),
        draft,
        current: present(current, b""),
    }
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
fn messages_must_be_non_blank_and_bounded() {
    let (objective, _) = active_objective();
    let send = |text: String| accept_message(&objective, acceptance(&objective), text, ts(5));
    assert_eq!(
        send(" \n\t".into()),
        Err(DomainError::InvalidRequest("Type a message first."))
    );
    assert_eq!(
        send("x".repeat(MAX_TEXT_BYTES + 1)),
        Err(DomainError::LimitReached("The message is too long."))
    );
    let input = send("x".repeat(MAX_TEXT_BYTES)).expect("at the limit");
    assert_eq!(
        (input.mode, input.accepted_at, input.objective_id),
        (InputMode::Steer, ts(5), objective.id())
    );
    assert_eq!(input.delivery, DeliveryState::Queued);
}

#[test]
fn messages_address_the_current_association_only() {
    let (objective, _) = active_objective();
    let mut stale = acceptance(&objective);
    stale.association_revision = Revision::new(7);
    assert_eq!(
        accept_message(&objective, stale, "go".into(), ts(5)),
        Err(DomainError::StaleAssociation)
    );
    let mut elsewhere = acceptance(&objective);
    elsewhere.session_id = session_id(9);
    assert_eq!(
        accept_message(&objective, elsewhere, "go".into(), ts(5)),
        Err(DomainError::StaleAssociation)
    );
}

#[test]
fn refusals_name_what_the_lifecycle_needs() {
    let (active, _) = active_objective();
    assert_eq!(
        accept_message(
            &defined_objective(),
            acceptance(&active),
            "go".into(),
            ts(5)
        ),
        Err(DomainError::InvalidLifecycle(
            "Start the agent before sending input."
        ))
    );
    assert_eq!(
        input_refusal(Lifecycle::Approved),
        "Return the candidate for revision before sending more input."
    );
    assert_eq!(
        input_refusal(Lifecycle::Frozen),
        input_refusal(Lifecycle::Approved)
    );
    for closed in [
        Lifecycle::Integrated,
        Lifecycle::CompletedNoChanges,
        Lifecycle::Discarded,
    ] {
        assert_eq!(input_refusal(closed), CLOSED);
    }
}

#[test]
fn batch_checks_run_before_anything_is_submitted() {
    let (objective, _) = active_objective();
    let same = digest(1);
    let mut first = save_new(&objective, 1, "fn main()", "a");
    let mut twin = first.clone();
    let mut batch = [entry(&mut first, &same), entry(&mut twin, &same)];
    assert_eq!(
        accept_feedback(&objective, acceptance(&objective), &mut batch, None, ts(5)),
        Err(DomainError::InvalidRequest("draft listed twice"))
    );
    let mut draft = save_new(&objective, 2, "fn main()", "a");
    let mut batch = [entry(&mut draft, &same)];
    assert_eq!(
        accept_feedback(
            &objective,
            acceptance(&objective),
            &mut batch,
            Some("n".repeat(MAX_TEXT_BYTES + 1)),
            ts(5)
        ),
        Err(DomainError::LimitReached("The note is too long."))
    );
    assert!(draft.is_unsent());
}

#[test]
fn rendered_feedback_that_outgrows_one_input_is_refused() {
    let (objective, _) = active_objective();
    let same = digest(1);
    let body = "b".repeat(MAX_TEXT_BYTES / 2);
    let mut drafts: Vec<Draft> = (1..=2)
        .map(|n| save_new(&objective, n, "fn main()", &body))
        .collect();
    let mut batch: Vec<BatchEntry<'_>> =
        drafts.iter_mut().map(|draft| entry(draft, &same)).collect();
    assert_eq!(
        accept_feedback(&objective, acceptance(&objective), &mut batch, None, ts(5)),
        Err(DomainError::LimitReached(
            "The feedback is too long to send at once. Submit fewer drafts."
        ))
    );
    assert!(drafts.iter().all(Draft::is_unsent));
}

#[test]
fn rendering_marks_the_old_side_quotes_every_line_and_skips_a_blank_note() {
    let new_side = reference_for("\tlet é = 1;\r\n\tprintln!");
    let old_side = Reference::new(
        new_side.artifact_digest().clone(),
        new_side.path().clone(),
        new_side.version(),
        Side::Old,
        new_side.byte_range(),
        new_side.selected_text().to_string(),
        new_side.line_hint(),
    )
    .expect("old side reference");
    let item = |reference: Reference, body: &str| FeedbackItem {
        draft_id: draft_id(1),
        draft_revision: Revision::INITIAL,
        reference,
        body: body.to_string(),
    };
    let items = [item(old_side, "old"), item(new_side, "new")];
    let text = render_feedback(&items, Some("  \n"));
    let blocks: Vec<&str> = text.split("\n\n").collect();
    assert_eq!(blocks.len(), 2, "{text}");
    assert!(blocks[0].contains(", old side, bytes "), "{text}");
    assert!(!blocks[1].contains("old side"), "{text}");
    assert!(
        blocks[1].ends_with("> \tlet é = 1;\n> \tprintln!\nnew"),
        "{text}"
    );
    assert_eq!(render_feedback(&[], Some("only a note")), "only a note");
    assert_eq!(render_feedback(&[], None), "");
}

#[test]
fn stopped_agents_still_accept_queued_input_as_blocked() {
    let objective = stopped_objective();
    let input = accept_message(&objective, acceptance(&objective), "later".into(), ts(5))
        .expect("accepted");
    assert_eq!(
        input.delivery,
        DeliveryState::Blocked {
            reason: BlockedReason::ExecutionUnavailable
        }
    );
    assert_eq!(InputMode::FollowUp.code(), "follow_up");
    assert_eq!(InputMode::Prompt.code(), "prompt");
}
