use super::commands::agent_line;
use super::*;
use crate::presentation::controller::intents::RecordingSink;
use crate::presentation::state::agent_session::Readiness;
use crate::presentation::state::drafts::DraftEditor;
use crate::presentation::state::model::{
    ActionAvailability, ActionName, DraftId, ExecutionState, RepoPath, SearchResult,
    SearchResultKind,
};
use crate::presentation::state::model::{AvailableAction, ConnectionState};
use crate::presentation::state::store::ViewerMode;
use crate::presentation::state::store::{PresentationState, StoreUpdate};
use crate::presentation::terminal::TerminalStatus;
use crate::presentation::test_fixture;

/// A connected controller over the shared fixture, recording the intents it emits.
pub(super) fn setup() -> (Rc<Controller>, Rc<RecordingSink>) {
    let mut state = PresentationState::new(ConnectionState::Connected);
    for update in test_fixture::fixture_updates() {
        state.apply(update);
    }
    state.connection = ConnectionState::Connected;
    let store = Store::new(state);
    let sink = Rc::new(RecordingSink::default());
    (Controller::new(store, sink.clone()), sink)
}

#[test]
fn compact_layout_follows_width_and_explicit_tasks() {
    let (controller, _) = setup();
    let changes = Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = changes.clone();
    controller
        .store
        .subscribe(move |c| seen.borrow_mut().extend_from_slice(c));
    controller.set_layout_width(1440);
    assert!(changes.borrow().is_empty(), "already wide");
    controller.set_layout_width(800);
    assert_eq!(controller.store.state().ui.layout, LayoutClass::Compact);
    assert_eq!(changes.borrow().as_slice(), [Change::Layout]);
    controller.show_region(Region::Objectives);
    controller.set_view(MainView::Activity);
    assert_eq!(controller.store.state().ui.compact_region, Region::Main);
    controller.select_objective(&ObjectiveId::new(test_fixture::MAIN_OBJECTIVE));
    controller.set_view(MainView::Workbench);
    let reference = controller
        .store
        .state()
        .drafts
        .values()
        .flatten()
        .next()
        .map(|d| d.reference.clone())
        .expect("fixture draft");
    controller.store.update(|s| {
        s.objective_ui_mut(&ObjectiveId::new(test_fixture::MAIN_OBJECTIVE))
            .selection = Some(Ok(reference));
        ((), Vec::new())
    });
    controller
        .add_feedback(DraftId::new("compact-draft"))
        .expect("draft created");
    let state = controller.store.state();
    assert_eq!(state.ui.compact_region, Region::Inspector);
    assert_eq!(state.ui.inspector_tab, InspectorTab::Feedback);
    drop(state);
    controller.set_compact_details(true);
    assert!(controller.store.state().ui.compact_details);
    controller.set_layout_width(1280);
    assert_eq!(controller.store.state().ui.layout, LayoutClass::Wide);
}

#[test]
fn content_search_results_become_the_viewer_selection_range() {
    let (controller, _) = setup();
    let id = ObjectiveId::new(test_fixture::MAIN_OBJECTIVE);
    controller.select_objective(&id);
    let path = RepoPath::utf8("src/network/retry.ts");
    controller.open_search_result(&SearchResult {
        kind: SearchResultKind::Content,
        objective_id: Some(id.clone()),
        path: Some(path.clone()),
        line: Some(2),
        preview: "retry".into(),
        byte_start: Some(10),
        byte_end: Some(15),
    });
    let key = controller
        .store
        .state()
        .viewer
        .key
        .clone()
        .expect("file opened");
    assert_eq!((key.path.clone(), key.mode), (path, ViewerMode::FullFile));
    assert_eq!(
        controller.take_reveal(&key, 12),
        None,
        "range not loaded yet"
    );
    assert_eq!(controller.take_reveal(&key, 15), Some((10, 15)));
    assert_eq!(controller.take_reveal(&key, 15), None, "taken once");
    controller.open_search_result(&SearchResult {
        kind: SearchResultKind::Content,
        objective_id: Some(id),
        path: Some(RepoPath::utf8("src/network/retry.ts")),
        line: Some(2),
        preview: "retry".into(),
        byte_start: None,
        byte_end: None,
    });
    assert_eq!(controller.store.state().viewer.reveal, None);
}

#[test]
fn notification_navigation_only_requests_projections() {
    let (controller, sink) = setup();
    controller.navigate_to(&ObjectiveId::new(test_fixture::SECOND_OBJECTIVE), true);
    let state = controller.store.state();
    assert_eq!(
        state.ui.selected.as_ref().map(ObjectiveId::as_str),
        Some(test_fixture::SECOND_OBJECTIVE)
    );
    assert_eq!(state.ui.view, MainView::Workbench);
    assert_eq!(state.ui.inspector_tab, InspectorTab::Evidence);
    let intents = sink.intents.borrow();
    assert!(!intents.is_empty());
    assert!(
        intents.iter().all(Intent::is_query),
        "navigation never sends a command: {intents:?}"
    );
}

#[test]
fn terminal_lifecycle_in_the_controller() {
    use crate::presentation::terminal::{TerminalEnd, TerminalMode};
    let (controller, sink) = setup();
    let id = ObjectiveId::new(test_fixture::MAIN_OBJECTIVE);
    controller.select_objective(&id);
    sink.intents.borrow_mut().clear();
    assert!(!controller.terminal_input("ls\r"), "no terminal yet");
    controller.open_terminal(80, 24).unwrap();
    controller.open_terminal(80, 24).unwrap();
    assert_eq!(
        sink.intents
            .borrow()
            .iter()
            .filter(|i| matches!(i, Intent::OpenTerminal { .. }))
            .count(),
        1,
        "one open in flight"
    );
    controller.store.apply(StoreUpdate::TerminalOpened {
        objective_id: id.clone(),
        terminal_id: "00000000-0000-4000-8000-000000000042".into(),
        mode: TerminalMode::ReadOnly,
        cols: 80,
        rows: 24,
    });
    assert!(controller.terminal_input(&"x".repeat(5000)));
    let inputs = sink
        .intents
        .borrow()
        .iter()
        .filter(|i| matches!(i, Intent::TerminalInput { .. }))
        .count();
    assert_eq!(inputs, 2, "long input is chunked");
    controller.resize_terminal(100, 30);
    controller.resize_terminal(100, 30);
    assert_eq!(
        sink.intents
            .borrow()
            .iter()
            .filter(|i| matches!(
                i,
                Intent::ResizeTerminal {
                    cols: 100,
                    rows: 30,
                    ..
                }
            ))
            .count(),
        1
    );
    controller.store.apply(StoreUpdate::TerminalOutput {
        terminal_id: "00000000-0000-4000-8000-000000000042".into(),
        seq: 1,
        data: "hello".into(),
        dropped: 0,
    });
    controller.store.apply(StoreUpdate::TerminalClosed {
        terminal_id: "00000000-0000-4000-8000-000000000042".into(),
        end: TerminalEnd::ReviewFreeze,
    });
    assert!(
        !controller.terminal_input("x"),
        "an ended terminal takes no input"
    );
    controller.open_terminal(80, 24).unwrap();
    controller.store.apply(StoreUpdate::TerminalOpened {
        objective_id: id.clone(),
        terminal_id: "00000000-0000-4000-8000-000000000043".into(),
        mode: TerminalMode::ReadWrite,
        cols: 80,
        rows: 24,
    });
    controller
        .store
        .apply(StoreUpdate::Connection(ConnectionState::Reconnecting {
            attempt: 1,
        }));
    assert_eq!(
        controller
            .store
            .state()
            .terminal_for(&id)
            .map(|t| t.status.clone()),
        Some(TerminalStatus::Ended(TerminalEnd::ConnectionLost))
    );
}

#[test]
fn service_stop_is_requested_only_with_control() {
    let (controller, sink) = setup();
    assert!(controller.request_service_stop(StopReason::Exit));
    assert!(sink.intents.borrow().contains(&Intent::StopService {
        reason: StopReason::Exit
    }));
    sink.intents.borrow_mut().clear();
    controller
        .store
        .apply(StoreUpdate::Connection(ConnectionState::LeaseHeld));
    assert!(!controller.request_service_stop(StopReason::Logout));
    assert!(
        sink.intents.borrow().is_empty(),
        "no authority, nothing sent"
    );
}

#[test]
fn selecting_an_objective_requests_its_projections() {
    let (controller, sink) = setup();
    controller.select_objective(&ObjectiveId::new(test_fixture::SECOND_OBJECTIVE));
    let intents = sink.intents.borrow();
    assert!(
        intents
            .iter()
            .any(|i| matches!(i, Intent::LoadObjective { .. }))
    );
    assert!(
        intents
            .iter()
            .any(|i| matches!(i, Intent::LoadChanges { .. }))
    );
}

#[test]
fn edit_intent_sends_the_edited_revision() {
    let (controller, sink) = setup();
    controller.select_objective(&ObjectiveId::new(test_fixture::MAIN_OBJECTIVE));
    sink.intents.borrow_mut().clear();
    let (revision, mut intent, enabled) = {
        let state = controller.store.state();
        let d = state.selected_detail().unwrap();
        (
            d.intent_revision,
            d.intent.clone(),
            d.availability(ActionName::ObjectiveUpdateIntent) == ActionAvailability::Enabled,
        )
    };
    assert_eq!(
        controller.update_intent(revision, intent.clone()),
        Err("Nothing changed".into())
    );
    intent.constraints = "Keep the public API".into();
    let result = controller.update_intent(revision, intent.clone());
    if enabled {
        result.unwrap();
        assert_eq!(
            sink.intents.borrow().as_slice(),
            [Intent::UpdateObjective {
                objective_id: ObjectiveId::new(test_fixture::MAIN_OBJECTIVE),
                expected_intent_revision: revision,
                intent,
            }]
        );
    } else {
        assert!(result.is_err());
        assert!(sink.intents.borrow().is_empty());
    }
}

#[test]
fn disabled_actions_are_not_sent() {
    let (controller, sink) = setup();
    controller.select_objective(&ObjectiveId::new(test_fixture::MAIN_OBJECTIVE));
    sink.intents.borrow_mut().clear();
    let err = controller.integrate().unwrap_err();
    assert!(!err.is_empty());
    assert!(sink.intents.borrow().is_empty());
}

fn without_session(
    detail: &ObjectiveDetail,
    start: Option<(bool, Option<&str>)>,
) -> ObjectiveDetail {
    let mut d = detail.clone();
    d.session = None;
    d.summary.execution.state = ExecutionState::None;
    d.available_actions.retain(|a| {
        !matches!(
            a.action,
            ActionName::ExecutionStart | ActionName::ExecutionResume
        )
    });
    if let Some((enabled, reason)) = start {
        d.available_actions.push(AvailableAction {
            action: ActionName::ExecutionStart,
            enabled,
            reason: reason.map(str::to_owned),
        });
    }
    d
}

#[test]
fn the_agent_line_is_one_sentence_per_state_with_ids_only_in_the_details() {
    let (controller, _) = setup();
    controller.select_objective(&ObjectiveId::new(test_fixture::MAIN_OBJECTIVE));
    let detail = controller
        .store
        .state()
        .selected_detail()
        .cloned()
        .expect("fixture detail");
    let line = agent_line(&detail, &Readiness::CatalogLoading);
    let session = detail.session.clone().expect("fixture session");
    assert!(!line.sentence.contains(session.session_id.short()));
    assert!(
        line.details
            .as_deref()
            .is_some_and(|d| d.contains(session.session_id.as_str()))
    );
    let fresh = without_session(&detail, Some((true, None)));
    for (readiness, sentence) in [
        (
            Readiness::NoProviderChosen,
            "The agent hasn't started. Choose a model provider below to start it.",
        ),
        (
            Readiness::NoProviders {
                error: Some("sandbox_unavailable".into()),
            },
            "The agent hasn't started. No model provider is available to start it.",
        ),
        (
            Readiness::ModelsLoading {
                provider: "Fixture".into(),
            },
            "The agent hasn't started. Loading Fixture's models…",
        ),
        (
            Readiness::NeedsCredentials {
                provider: "Anthropic".into(),
            },
            "The agent hasn't started. Add credentials for Anthropic to start it, or choose \
             another provider below.",
        ),
    ] {
        let line = agent_line(&fresh, &readiness);
        assert_eq!(line.sentence, sentence);
        assert_eq!(line.details, None, "no session, nothing technical");
    }
    let refused = without_session(&detail, Some((false, Some("why"))));
    assert_eq!(
        agent_line(&refused, &Readiness::NoProviderChosen).sentence,
        "The agent hasn't started and can't be started right now."
    );
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
    use crate::presentation::state::model::{Draft, DraftState, SessionId};
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
    use crate::presentation::state::model::{Draft, DraftState};
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
