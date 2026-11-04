use super::*;
use crate::presentation::controller::intents::RecordingSink;
use crate::presentation::state::model::{
    ActionName, AvailableAction, ConnectionState, DraftId, ExecutionState,
};
use crate::presentation::state::store::{PresentationState, StoreUpdate};
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

/// [`setup`] with the fixture's main Objective selected and the selection's intents cleared.
pub(super) fn main_selected() -> (Rc<Controller>, Rc<RecordingSink>, ObjectiveId) {
    let (controller, sink) = setup();
    let id = ObjectiveId::new(test_fixture::MAIN_OBJECTIVE);
    controller.select_objective(&id);
    sink.intents.borrow_mut().clear();
    (controller, sink, id)
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

/// The detail without an Agent Session, optionally offering `execution.start`.
pub(super) fn without_session(
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
