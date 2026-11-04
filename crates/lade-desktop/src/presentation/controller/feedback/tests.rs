use super::*;
use crate::presentation::controller::tests::{setup, without_session};
use crate::presentation::state::model::{
    ConnectionState, Draft, DraftState, Load, ObjectiveId, SessionId,
};
use crate::presentation::state::store::{MainView, StoreUpdate};
use crate::presentation::test_fixture;

/// Enables `feedback.submit` and leaves exactly the saved fixture draft, made current.
fn submittable(controller: &Controller) -> ObjectiveId {
    let id = ObjectiveId::new(test_fixture::MAIN_OBJECTIVE);
    controller.select_objective(&id);
    controller.store.update(|s| {
        let editors = s.drafts.get_mut(&id).expect("fixture drafts");
        editors.retain(|e| e.draft_id.as_str() == test_fixture::SAVED_DRAFT);
        for editor in editors.iter_mut() {
            editor.stale = false;
        }
        if let Some(Load::Loaded(detail)) = s.details.get_mut(&id) {
            for available in &mut detail.available_actions {
                if available.action == ActionName::FeedbackSubmit {
                    available.enabled = true;
                    available.reason = None;
                }
            }
        }
        ((), Vec::new())
    });
    id
}

#[test]
fn feedback_without_a_session_states_that_once() {
    let (controller, _) = setup();
    let id = ObjectiveId::new(test_fixture::MAIN_OBJECTIVE);
    controller.select_objective(&id);
    controller.store.update(|s| {
        if let Some(Load::Loaded(detail)) = s.details.get_mut(&id) {
            *detail = without_session(detail, None);
            for available in &mut detail.available_actions {
                if available.action == ActionName::FeedbackSubmit {
                    available.enabled = false;
                    available.reason = Some("input requires an active objective".into());
                }
            }
        }
        ((), Vec::new())
    });
    let batch = controller.feedback_batch().unwrap();
    assert!(
        batch
            .blockers
            .iter()
            .any(|b| b.contains("no Agent Session")),
        "{:?}",
        batch.blockers
    );
    assert!(
        !batch
            .blockers
            .iter()
            .any(|b| b == "input requires an active objective"),
        "the service reason would only restate it: {:?}",
        batch.blockers
    );
}

#[test]
fn stale_item_blocks_the_whole_batch() {
    let (controller, sink) = setup();
    controller.select_objective(&ObjectiveId::new(test_fixture::MAIN_OBJECTIVE));
    sink.intents.borrow_mut().clear();
    let batch = controller.feedback_batch().unwrap();
    assert!(batch.items.iter().any(|i| i.stale));
    assert!(batch.blockers.iter().any(|b| b.contains("stale")));
    assert!(controller.submit_feedback(&batch).is_err());
    assert!(sink.intents.borrow().is_empty());
}

#[test]
fn a_feedback_batch_takes_at_most_the_wire_bound_and_defers_the_rest() {
    let (controller, sink) = setup();
    let id = ObjectiveId::new(test_fixture::MAIN_OBJECTIVE);
    controller.select_objective(&id);
    sink.intents.borrow_mut().clear();
    controller.store.update(|s| {
        let template = s.drafts[&id][0].reference.clone();
        let session = s
            .selected_detail()
            .and_then(|d| d.session.clone())
            .map_or_else(|| SessionId::new("s"), |d| d.session_id);
        let editors = (0..MAX_FEEDBACK_BATCH + 44)
            .map(|n| {
                DraftEditor::from_durable(&Draft {
                    draft_id: DraftId::new(format!("draft-{n}")),
                    objective_id: id.clone(),
                    destination_session_id: session.clone(),
                    reference: template.clone(),
                    body: format!("note {n}"),
                    revision: 1,
                    state: DraftState::Unsent,
                    stale: false,
                    input_id: None,
                })
            })
            .collect();
        s.drafts.insert(id.clone(), editors);
        if let Some(Load::Loaded(detail)) = s.details.get_mut(&id) {
            for available in &mut detail.available_actions {
                if available.action == ActionName::FeedbackSubmit {
                    available.enabled = true;
                    available.reason = None;
                }
            }
        }
        ((), Vec::new())
    });
    let batch = controller.feedback_batch().unwrap();
    assert_eq!(
        (batch.items.len(), batch.deferred),
        (MAX_FEEDBACK_BATCH, 44)
    );
    assert_eq!(batch.blockers, Vec::<String>::new());
    controller.submit_feedback(&batch).unwrap();
    let intents = sink.intents.borrow();
    let Some(Intent::SubmitFeedback { drafts, .. }) = intents.last() else {
        panic!("feedback submitted");
    };
    assert_eq!(drafts.len(), MAX_FEEDBACK_BATCH);
}

#[test]
fn closing_flushes_drafts_and_lists_what_is_not_yet_saved() {
    let (controller, sink) = setup();
    controller.select_objective(&ObjectiveId::new(test_fixture::MAIN_OBJECTIVE));
    sink.intents.borrow_mut().clear();
    let draft = DraftId::new(test_fixture::SAVED_DRAFT);
    let before = controller.unsaved_drafts().len();
    controller.edit_draft(&draft, "Closing right after typing.", 1_000);
    let unsaved = controller.unsaved_drafts();
    assert_eq!(unsaved.len(), before + 1);
    assert!(
        unsaved
            .iter()
            .any(|d| d.draft_id == draft && d.status == "Unsaved")
    );
    controller.flush_drafts(1_010);
    assert!(
        sink.intents
            .borrow()
            .iter()
            .any(|i| matches!(i, Intent::SaveDraft { draft_id, .. } if draft_id == &draft)),
        "the flush does not wait for the quiet period"
    );
    assert!(
        controller
            .unsaved_drafts()
            .iter()
            .any(|d| d.draft_id == draft && d.status == "Saving…"),
        "in flight is still unsaved until the service acknowledges it"
    );
}

#[test]
fn typing_in_a_draft_schedules_a_save_intent() {
    let (controller, sink) = setup();
    controller.select_objective(&ObjectiveId::new(test_fixture::MAIN_OBJECTIVE));
    sink.intents.borrow_mut().clear();
    let draft = DraftId::new(test_fixture::SAVED_DRAFT);
    controller.edit_draft(&draft, "Use a typed TimeoutError here.", 1_000);
    assert_eq!(controller.poll_drafts(1_100), Some(1_300));
    assert!(sink.intents.borrow().is_empty());
    controller.poll_drafts(1_300);
    let intents = sink.intents.borrow();
    assert!(matches!(
        intents.as_slice(),
        [Intent::SaveDraft {
            expected_revision: 3,
            ..
        }]
    ));
}

#[test]
fn draft_removal_waits_for_its_first_save_to_prevent_resurrection() {
    let (controller, sink) = setup();
    let objective_id = ObjectiveId::new(test_fixture::MAIN_OBJECTIVE);
    let draft_id = DraftId::new("new-draft");
    controller.select_objective(&objective_id);
    controller.store.update(|state| {
        let template = state.drafts[&objective_id][0].clone();
        let editor = DraftEditor::new_local(
            draft_id.clone(),
            objective_id.clone(),
            template.destination_session_id,
            template.reference,
        );
        state.drafts.get_mut(&objective_id).unwrap().push(editor);
        ((), Vec::new())
    });
    controller.edit_draft(&draft_id, "Do not resurrect this draft", 1_000);
    controller.poll_drafts(1_300);
    let saved = {
        let state = controller.store.state();
        let editor = state.drafts[&objective_id]
            .iter()
            .find(|draft| draft.draft_id == draft_id)
            .unwrap();
        Draft {
            draft_id: draft_id.clone(),
            objective_id: objective_id.clone(),
            destination_session_id: editor.destination_session_id.clone(),
            reference: editor.reference.clone(),
            body: editor.body.clone(),
            revision: 1,
            state: DraftState::Unsent,
            stale: false,
            input_id: None,
        }
    };
    sink.intents.borrow_mut().clear();
    assert!(controller.delete_draft(&draft_id).is_err());
    assert_eq!(
        controller.store.state().drafts[&objective_id]
            .iter()
            .filter(|draft| draft.draft_id == draft_id)
            .map(|draft| draft.body.clone())
            .collect::<Vec<_>>(),
        ["Do not resurrect this draft"],
        "a refused removal preserves the editor and its text"
    );
    controller.store.apply(StoreUpdate::DraftChanged(saved));
    assert_eq!(
        controller.store.state().drafts[&objective_id]
            .iter()
            .filter(|draft| draft.draft_id == draft_id)
            .map(|draft| draft.body.clone())
            .collect::<Vec<_>>(),
        ["Do not resurrect this draft"],
        "the acknowledgement keeps exactly one editor"
    );
    assert!(sink.intents.borrow().is_empty());
    controller
        .delete_draft(&draft_id)
        .expect("save acknowledged");
    assert_eq!(
        sink.intents.borrow().as_slice(),
        [Intent::DeleteDraft {
            draft_id,
            expected_revision: 1,
        }]
    );
}

#[test]
fn feedback_is_anchored_to_the_current_viewer_selection() {
    let (controller, _) = setup();
    assert_eq!(
        controller.add_feedback(DraftId::new("n")),
        Err("Select an Objective first".into())
    );
    let id = ObjectiveId::new(test_fixture::MAIN_OBJECTIVE);
    controller.select_objective(&id);
    assert_eq!(
        controller.add_feedback(DraftId::new("n")),
        Err(SelectionError::Empty.message().into())
    );
    let set_selection = |selection| {
        controller.store.update(|s| {
            s.objective_ui_mut(&id).selection = selection;
            ((), Vec::new())
        });
    };
    set_selection(Some(Err("spans hunks".into())));
    assert_eq!(
        controller.add_feedback(DraftId::new("n")),
        Err("spans hunks".into())
    );
    let reference = controller.store.state().drafts[&id][0].reference.clone();
    set_selection(Some(Ok(reference.clone())));
    let drafts_before = controller.store.state().drafts[&id].len();
    assert_eq!(
        controller.add_feedback(DraftId::new("n")),
        Ok(DraftId::new("n"))
    );
    let state = controller.store.state();
    let first = &state.drafts[&id][0];
    assert_eq!(
        first.draft_id,
        DraftId::new("n"),
        "the newest draft comes first"
    );
    assert_eq!(first.reference, reference);
    assert_eq!(state.drafts[&id].len(), drafts_before + 1);
    assert_eq!(state.ui.inspector_tab, InspectorTab::Feedback);
    assert_eq!(state.ui.view, MainView::Workbench);
}

#[test]
fn feedback_needs_an_agent_session_to_address() {
    let (controller, _) = setup();
    let id = ObjectiveId::new(test_fixture::MAIN_OBJECTIVE);
    controller.select_objective(&id);
    controller.store.update(|s| {
        let reference = s.drafts[&id][0].reference.clone();
        s.objective_ui_mut(&id).selection = Some(Ok(reference));
        if let Some(Load::Loaded(detail)) = s.details.get_mut(&id) {
            *detail = without_session(detail, None);
        }
        ((), Vec::new())
    });
    assert_eq!(
        controller.add_feedback(DraftId::new("n")),
        Err("No Agent Session to address — start the agent first".into())
    );
}

#[test]
fn a_ready_batch_is_submitted_to_its_session_in_the_chosen_mode() {
    let (controller, sink) = setup();
    let id = submittable(&controller);
    controller.set_feedback_mode(DeliveryMode::FollowUp);
    sink.intents.borrow_mut().clear();
    let batch = controller.feedback_batch().unwrap();
    assert_eq!(batch.blockers, Vec::<String>::new());
    assert_eq!((batch.mode, batch.deferred), (DeliveryMode::FollowUp, 0));
    let destination = batch.destination.clone().expect("fixture session");
    controller.submit_feedback(&batch).unwrap();
    assert_eq!(
        sink.intents.borrow().as_slice(),
        [Intent::SubmitFeedback {
            objective_id: id,
            destination_session_id: destination.session_id,
            association_revision: destination.association_revision,
            drafts: batch
                .items
                .iter()
                .map(|i| DraftRevisionRef {
                    draft_id: i.draft_id.clone(),
                    revision: i.revision,
                })
                .collect(),
            mode: DeliveryMode::FollowUp,
            note: None,
        }]
    );
}

#[test]
fn a_batch_waits_for_the_connection_and_for_unsaved_text() {
    let (controller, sink) = setup();
    submittable(&controller);
    controller.edit_draft(&DraftId::new(test_fixture::SAVED_DRAFT), "changed", 1_000);
    let batch = controller.feedback_batch().unwrap();
    assert_eq!(batch.blockers, ["Wait until every draft is saved"]);
    controller
        .store
        .apply(StoreUpdate::Connection(ConnectionState::Reconnecting {
            attempt: 1,
        }));
    let batch = controller.feedback_batch().unwrap();
    assert_eq!(batch.blockers[0], "Not connected to the service");
    sink.intents.borrow_mut().clear();
    assert_eq!(
        controller.submit_feedback(&batch),
        Err("Not connected to the service".into())
    );
    assert!(sink.intents.borrow().is_empty());
}

#[test]
fn an_empty_batch_says_so_instead_of_the_service_reason() {
    let (controller, _) = setup();
    let id = submittable(&controller);
    controller.store.update(|s| {
        s.drafts.insert(id.clone(), Vec::new());
        ((), Vec::new())
    });
    let batch = controller.feedback_batch().unwrap();
    assert_eq!(batch.blockers, ["No drafts to submit"]);
    let (controller, _) = setup();
    assert_eq!(controller.feedback_batch(), None, "nothing selected");
}

#[test]
fn a_never_saved_draft_is_removed_locally_without_asking_the_service() {
    let (controller, sink) = setup();
    let id = ObjectiveId::new(test_fixture::MAIN_OBJECTIVE);
    controller.select_objective(&id);
    controller.store.update(|s| {
        let template = s.drafts[&id][0].clone();
        s.drafts.get_mut(&id).unwrap().push(DraftEditor::new_local(
            DraftId::new("local"),
            id.clone(),
            template.destination_session_id,
            template.reference,
        ));
        ((), Vec::new())
    });
    sink.intents.borrow_mut().clear();
    controller.delete_draft(&DraftId::new("local")).unwrap();
    assert!(sink.intents.borrow().is_empty());
    assert!(
        !controller.store.state().drafts[&id]
            .iter()
            .any(|d| d.draft_id.as_str() == "local")
    );
    controller.delete_draft(&DraftId::new("unknown")).unwrap();
    assert!(
        sink.intents.borrow().is_empty(),
        "an unknown draft is a no-op"
    );
}

#[test]
fn unsaved_drafts_name_their_objective_and_sort_by_draft_id() {
    let (controller, _) = setup();
    let id = ObjectiveId::new(test_fixture::MAIN_OBJECTIVE);
    controller.select_objective(&id);
    let ids: Vec<DraftId> = controller.store.state().drafts[&id]
        .iter()
        .map(|d| d.draft_id.clone())
        .collect();
    for draft in ids.iter().rev() {
        controller.edit_draft(draft, "edited", 1_000);
    }
    let unsaved = controller.unsaved_drafts();
    let mut sorted = unsaved.clone();
    sorted.sort_by(|a, b| a.draft_id.as_str().cmp(b.draft_id.as_str()));
    assert_eq!(unsaved, sorted);
    let title = controller
        .store
        .state()
        .objective(&id)
        .unwrap()
        .title
        .clone();
    assert!(unsaved.iter().all(|d| d.objective == title && d.pending));
}
