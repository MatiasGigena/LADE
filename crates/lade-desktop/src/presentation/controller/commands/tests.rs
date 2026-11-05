use super::*;
use crate::presentation::controller::intents::{RecordingSink, StartPoint};
use crate::presentation::controller::tests::{main_selected, setup, without_session};
use crate::presentation::state::model::{
    ApprovalId, ApprovalState, ApprovalSummary, AvailableAction, CandidateId, ConnectionState,
    GitId, Load, Timestamp,
};
use crate::presentation::state::store::PresentationState;
use crate::presentation::state::store::Store;
use crate::presentation::state::store::StoreUpdate;
use crate::presentation::test_fixture;
use std::rc::Rc;

fn edit_detail(controller: &Controller, edit: impl FnOnce(&mut ObjectiveDetail)) {
    let id = ObjectiveId::new(test_fixture::MAIN_OBJECTIVE);
    controller.store.update(|s| {
        if let Some(Load::Loaded(detail)) = s.details.get_mut(&id) {
            edit(detail);
        }
        ((), Vec::new())
    });
}

fn offer(detail: &mut ObjectiveDetail, action: ActionName) {
    detail.available_actions.retain(|a| a.action != action);
    detail.available_actions.push(AvailableAction {
        action,
        enabled: true,
        reason: None,
    });
}

#[test]
fn edit_intent_sends_the_edited_revision() {
    let (controller, sink, _) = main_selected();
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
    let (controller, sink, _) = main_selected();
    let err = controller.integrate().unwrap_err();
    assert!(!err.is_empty());
    assert!(sink.intents.borrow().is_empty());
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
fn commands_need_a_selected_and_loaded_objective() {
    let (controller, sink) = setup();
    assert_eq!(controller.freeze(), Err("Select an Objective first".into()));
    let unknown = ObjectiveId::new("unloaded");
    controller.store.update(|s| {
        s.ui.selected = Some(unknown);
        ((), Vec::new())
    });
    assert_eq!(
        controller.discard(),
        Err("The Objective is not loaded".into())
    );
    assert_eq!(
        controller.return_for_revision(),
        Err("There is no current candidate".into())
    );
    assert!(sink.intents.borrow().is_empty());
}

#[test]
fn a_disabled_action_reports_the_service_reason_and_an_absent_one_says_so() {
    let (controller, sink, _) = main_selected();
    assert_eq!(
        controller.freeze(),
        Err("Already frozen as Candidate 2".into())
    );
    assert_eq!(
        controller.resume(),
        Err("This action is not offered".into())
    );
    assert!(sink.intents.borrow().is_empty());
    controller.discard().unwrap();
    assert_eq!(
        sink.intents.borrow().as_slice(),
        [Intent::Discard {
            objective_id: ObjectiveId::new(test_fixture::MAIN_OBJECTIVE)
        }]
    );
}

#[test]
fn messages_and_intents_must_say_something() {
    let (controller, sink, _) = main_selected();
    assert_eq!(
        controller.send_message("  \n", DeliveryMode::Prompt),
        Err("Type a message first".into())
    );
    let blank = ObjectiveIntent {
        outcome: " ".into(),
        constraints: String::new(),
        acceptance: String::new(),
    };
    assert_eq!(
        controller.update_intent(2, blank),
        Err("Describe the expected outcome".into())
    );
    assert!(sink.intents.borrow().is_empty());
}

#[test]
fn starting_with_another_model_also_makes_it_the_default() {
    let (controller, sink, _) = main_selected();
    edit_detail(&controller, |d| offer(d, ActionName::ExecutionStart));
    let default = ModelRef::new("test-provider", "test-model");
    controller.start_execution(default.clone(), None).unwrap();
    assert_eq!(sink.intents.borrow().len(), 1, "already the default");
    sink.intents.borrow_mut().clear();
    let other = ModelRef::new("anthropic", "claude");
    controller
        .start_execution(other.clone(), Some(ThinkingLevel::Off))
        .unwrap();
    let intents = sink.intents.borrow();
    assert!(matches!(
        intents.as_slice(),
        [
            Intent::StartExecution { model, thinking: Some(ThinkingLevel::Off), .. },
            Intent::UpdateSettings { default_model },
        ] if model == &other && default_model == &other
    ));
}

#[test]
fn verify_splits_the_command_line_and_rejects_bad_quoting() {
    let (controller, sink, _) = main_selected();
    assert_eq!(
        controller.verify("npm test 'unclosed"),
        Err("Unclosed single quote".into())
    );
    assert!(sink.intents.borrow().is_empty());
    controller.verify("npm test -- 'a b'").unwrap();
    let intents = sink.intents.borrow();
    let [
        Intent::Verify {
            argv, candidate_id, ..
        },
    ] = intents.as_slice()
    else {
        panic!("one verification: {intents:?}");
    };
    assert_eq!(argv, &["npm", "test", "--", "a b"]);
    assert_eq!(candidate_id.as_str(), test_fixture::CANDIDATE_2);
}

#[test]
fn approval_needs_an_override_unless_the_latest_run_of_every_check_passed() {
    let (controller, sink, _) = main_selected();
    assert!(controller.approval_needs_override(), "npm test failed");
    assert_eq!(
        controller.approve(Some("   ".into())),
        Err("Checks are missing or not all passed — give an override reason".into())
    );
    assert!(sink.intents.borrow().is_empty());
    let candidate = test_fixture::CANDIDATE_2;
    edit_detail(&controller, |d| {
        let failed = d
            .verifications
            .iter()
            .find(|v| v.candidate_id.as_str() == candidate && v.state == VerificationState::Failed)
            .cloned()
            .expect("failed check");
        let mut rerun = failed;
        rerun.state = VerificationState::Passed;
        rerun.started_at = Timestamp(rerun.started_at.0 + 1);
        d.verifications.push(rerun);
    });
    assert!(
        !controller.approval_needs_override(),
        "a later passing run replaces the failure; checks of other candidates do not count"
    );
    controller.approve(None).unwrap();
    assert!(matches!(
        sink.intents.borrow().as_slice(),
        [Intent::Approve {
            override_reason: None,
            ..
        }]
    ));
    edit_detail(&controller, |d| d.verifications.clear());
    assert!(controller.approval_needs_override(), "no checks at all");
    sink.intents.borrow_mut().clear();
    controller.approve(Some(" urgent ".into())).unwrap();
    assert!(matches!(
        sink.intents.borrow().as_slice(),
        [Intent::Approve { override_reason: Some(reason), .. }] if reason == " urgent "
    ));
}

#[test]
fn integrating_needs_a_valid_approval() {
    let (controller, sink, _) = main_selected();
    edit_detail(&controller, |d| offer(d, ActionName::ReviewIntegrate));
    assert_eq!(
        controller.integrate(),
        Err("There is no valid approval".into())
    );
    let approval = |state| ApprovalSummary {
        approval_id: ApprovalId::new("approval"),
        candidate_id: CandidateId::new(test_fixture::CANDIDATE_2),
        target_commit: GitId::sha1("4f2c9e1"),
        override_reason: None,
        state,
    };
    for spent in [ApprovalState::Consumed, ApprovalState::Invalidated] {
        edit_detail(&controller, |d| d.approval = Some(approval(spent)));
        assert!(controller.integrate().is_err(), "{spent:?}");
    }
    edit_detail(&controller, |d| {
        d.approval = Some(approval(ApprovalState::Valid));
    });
    controller.integrate().unwrap();
    assert_eq!(
        sink.intents.borrow().as_slice(),
        [Intent::Integrate {
            objective_id: ObjectiveId::new(test_fixture::MAIN_OBJECTIVE),
            approval_id: ApprovalId::new("approval"),
        }]
    );
}

#[test]
fn a_new_objective_is_validated_before_it_is_sent() {
    let (controller, sink) = setup();
    let intent = |outcome: &str| ObjectiveIntent {
        outcome: outcome.into(),
        constraints: String::new(),
        acceptance: String::new(),
    };
    let create = |title: &str, outcome: &str, start| {
        controller.create_objective(title, intent(outcome), "main", start)
    };
    let title_error = Err("Give the Objective a title of 1–200 characters".to_owned());
    assert_eq!(create("  ", "x", StartPoint::Target), title_error);
    assert_eq!(
        create(&"é".repeat(201), "x", StartPoint::Target),
        title_error
    );
    assert_eq!(
        create("Title", " ", StartPoint::Target),
        Err("Describe the expected outcome".into())
    );
    for bad in ["abc", &"g".repeat(40), &"a".repeat(41)] {
        assert!(
            create("Title", "x", StartPoint::Commit(bad.to_owned()))
                .unwrap_err()
                .starts_with("Enter the full 40- or 64-character commit id")
        );
    }
    assert!(sink.intents.borrow().is_empty());
    create(
        &"é".repeat(200),
        "x",
        StartPoint::Commit(format!(" {} ", "AB".repeat(32))),
    )
    .unwrap();
    let intents = sink.intents.borrow();
    let [
        Intent::CreateObjective {
            title,
            start,
            integration_target,
            ..
        },
    ] = intents.as_slice()
    else {
        panic!("one objective created: {intents:?}");
    };
    assert_eq!(title.chars().count(), 200);
    assert_eq!(start, &StartPoint::Commit("ab".repeat(32)));
    assert_eq!(integration_target, "main");
}

#[test]
fn a_new_objective_needs_an_open_repository() {
    let store = Store::new(PresentationState::new(ConnectionState::Connected));
    let sink = Rc::new(RecordingSink::default());
    let controller = Controller::new(store, sink.clone());
    controller
        .store
        .apply(StoreUpdate::Connection(ConnectionState::Connected));
    let intent = ObjectiveIntent {
        outcome: "x".into(),
        constraints: String::new(),
        acceptance: String::new(),
    };
    assert_eq!(
        controller.create_objective("Title", intent, "main", StartPoint::Target),
        Err("Open a Git repository first".into())
    );
    assert!(sink.intents.borrow().is_empty());
}

#[test]
fn a_stopped_agent_offers_to_resume_only_when_the_service_does() {
    let (controller, _, _) = main_selected();
    let mut detail = controller.store.state().selected_detail().cloned().unwrap();
    detail.summary.execution.state = ExecutionState::Failed;
    let readiness = Readiness::CatalogLoading;
    assert_eq!(
        agent_line(&detail, &readiness).sentence,
        "The agent stopped after an error."
    );
    offer(&mut detail, ActionName::ExecutionResume);
    assert_eq!(
        agent_line(&detail, &readiness).sentence,
        "The agent stopped after an error. Resume it to continue the same Agent Session."
    );
    detail.summary.execution.state = ExecutionState::Running;
    assert_eq!(
        agent_line(&detail, &readiness).sentence,
        "The agent is working on a turn."
    );
}
