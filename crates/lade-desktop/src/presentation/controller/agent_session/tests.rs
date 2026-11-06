use super::*;
use crate::presentation::controller::tests::main_selected;
use crate::presentation::state::agent_session::{
    AgentSessionState, AgentSessionUpdate, Catalog, ModelInfo, ProviderInfo, ProviderStatus,
    UiDialog, UiPrompt,
};
use crate::presentation::state::model::Load;
use crate::presentation::test_fixture;

#[test]
fn the_picker_uses_the_service_catalogue_and_loads_models_on_demand() {
    let (controller, sink, _) = main_selected();
    controller.store.apply_all(vec![StoreUpdate::AgentSession(
        AgentSessionUpdate::Catalog(Catalog {
            sdk_version: "1.5.0".into(),
            providers: vec![ProviderInfo {
                id: "local-llm".into(),
                name: "Fixture".into(),
                auth: vec!["no credentials needed".into()],
                status: ProviderStatus::Available,
                reason: None,
                variables: Vec::new(),
                models: 1,
            }],
            error: None,
        }),
    )]);
    assert_eq!(
        sink.intents.borrow().as_slice(),
        [Intent::LoadModels {
            provider: Some("local-llm".into())
        }]
    );
    assert_eq!(
        controller.readiness(),
        Readiness::ModelsLoading {
            provider: "Fixture".into()
        }
    );
    assert_eq!(
        controller.chosen_model().unwrap_err(),
        "Loading Fixture's models…"
    );
    controller.store.apply_all(vec![StoreUpdate::AgentSession(
        AgentSessionUpdate::Models {
            provider: "local-llm".into(),
            models: vec![ModelInfo {
                provider: "local-llm".into(),
                id: "scripted".into(),
                name: "Scripted".into(),
                reasoning: false,
                thinking: vec![ThinkingLevel::Off],
            }],
        },
    )]);
    let (model, thinking) = controller.chosen_model().unwrap();
    assert_eq!(model, ModelRef::new("local-llm", "scripted"));
    assert_eq!(thinking, ThinkingLevel::Off);
}

fn catalog(providers: Vec<ProviderInfo>, error: Option<&str>) -> StoreUpdate {
    StoreUpdate::AgentSession(AgentSessionUpdate::Catalog(Catalog {
        sdk_version: "1.5.0".into(),
        providers,
        error: error.map(str::to_owned),
    }))
}

#[test]
fn readiness_tells_apart_no_catalogue_no_provider_loading_and_no_models() {
    let (controller, sink, _) = main_selected();
    controller.store.update(|s| {
        s.agent_session = AgentSessionState::default();
        ((), Vec::new())
    });
    assert_eq!(controller.readiness(), Readiness::CatalogLoading);
    controller.store.apply(catalog(
        Vec::new(),
        Some("worker sandbox unavailable: sandbox_unavailable"),
    ));
    assert_eq!(
        controller.readiness(),
        Readiness::NoProviders {
            error: Some("worker sandbox unavailable: sandbox_unavailable".into())
        }
    );
    assert_eq!(
        controller.chosen_model().unwrap_err(),
        "LADE can't load models right now."
    );
    assert!(sink.intents.borrow().is_empty(), "nothing to load");
    controller.store.apply(catalog(
        vec![ProviderInfo {
            id: "anthropic".into(),
            name: "Anthropic".into(),
            auth: vec!["API key".into()],
            status: ProviderStatus::MissingCredentials,
            reason: Some("Set ANTHROPIC_API_KEY".into()),
            variables: vec!["ANTHROPIC_API_KEY".into()],
            models: 0,
        }],
        None,
    ));
    assert_eq!(
        controller
            .store
            .state()
            .agent_session
            .choice
            .provider
            .as_deref(),
        Some("anthropic")
    );
    controller
        .store
        .apply(StoreUpdate::AgentSession(AgentSessionUpdate::Models {
            provider: "anthropic".into(),
            models: Vec::new(),
        }));
    assert_eq!(
        controller.readiness(),
        Readiness::NeedsCredentials {
            provider: "Anthropic".into()
        }
    );
    assert_eq!(
        controller.chosen_model().unwrap_err(),
        "Add credentials for Anthropic to start the agent."
    );
    let anthropic = |status| ProviderInfo {
        id: "anthropic".into(),
        name: "Anthropic".into(),
        auth: vec!["API key".into()],
        status,
        reason: None,
        variables: vec!["ANTHROPIC_API_KEY".into()],
        models: 0,
    };
    controller
        .store
        .apply(catalog(vec![anthropic(ProviderStatus::Restricted)], None));
    assert_eq!(
        controller.readiness(),
        Readiness::Restricted {
            provider: "Anthropic".into()
        }
    );
    controller
        .store
        .apply(catalog(vec![anthropic(ProviderStatus::Available)], None));
    assert_eq!(
        controller.readiness(),
        Readiness::NoModels {
            provider: "Anthropic".into()
        }
    );
}

#[test]
fn dialogs_are_answered_explicitly_with_their_generation() {
    let (controller, sink, _) = main_selected();
    assert!(
        controller
            .answer_dialog("missing", UiAnswer::Cancel)
            .is_err()
    );
    controller.store.apply_all(vec![StoreUpdate::AgentSession(
        AgentSessionUpdate::DialogOpened(UiDialog {
            objective_id: ObjectiveId::new(test_fixture::MAIN_OBJECTIVE),
            ui_id: "00000000-0000-4000-8000-000000000042".into(),
            generation: 3,
            prompt: UiPrompt::Confirm {
                title: "Proceed?".into(),
                message: "m".into(),
            },
        }),
    )]);
    controller
        .answer_dialog(
            "00000000-0000-4000-8000-000000000042",
            UiAnswer::Confirm(true),
        )
        .unwrap();
    assert!(matches!(
        sink.intents.borrow().as_slice(),
        [Intent::RespondDialog {
            generation: 3,
            answer: UiAnswer::Confirm(true),
            ..
        }]
    ));
}

#[test]
fn session_operations_need_a_live_session() {
    let (controller, sink, _) = main_selected();
    let state = controller
        .store
        .state()
        .selected_detail()
        .map(|d| d.summary.execution.state);
    let result = controller.compact_session();
    if let Some(ExecutionState::Idle) = state {
        assert!(result.is_ok());
    } else {
        assert!(result.is_err());
        assert!(sink.intents.borrow().is_empty());
    }
}

#[test]
fn configuring_waits_only_for_a_running_turn() {
    let (controller, sink, _) = main_selected();
    controller.store.apply_all(vec![
        catalog(
            vec![ProviderInfo {
                id: "local-llm".into(),
                name: "Fixture".into(),
                auth: vec!["no credentials needed".into()],
                status: ProviderStatus::Available,
                reason: None,
                variables: Vec::new(),
                models: 1,
            }],
            None,
        ),
        StoreUpdate::AgentSession(AgentSessionUpdate::Models {
            provider: "local-llm".into(),
            models: vec![ModelInfo {
                provider: "local-llm".into(),
                id: "scripted".into(),
                name: "Scripted".into(),
                reasoning: false,
                thinking: vec![ThinkingLevel::Off],
            }],
        }),
    ]);
    let execution = |state| {
        controller.store.update(|s| {
            let id = ObjectiveId::new(test_fixture::MAIN_OBJECTIVE);
            if let Some(Load::Loaded(detail)) = s.details.get_mut(&id) {
                detail.summary.execution.state = state;
            }
            ((), Vec::new())
        });
    };
    sink.intents.borrow_mut().clear();
    execution(ExecutionState::Running);
    assert_eq!(
        controller.configure_session().unwrap_err(),
        "Wait until the current turn has finished"
    );
    assert_eq!(
        controller.session_availability(SessionUse::OutsideTurn),
        ActionAvailability::Disabled {
            reason: "Wait until the current turn has finished".into()
        }
    );
    assert_eq!(
        controller.session_availability(SessionUse::Running),
        ActionAvailability::Enabled
    );
    assert_eq!(
        controller.compact_session().unwrap_err(),
        "Wait until the current turn has finished"
    );
    assert!(controller.export_session().is_ok());
    execution(ExecutionState::Cancelled);
    assert_eq!(
        controller.session_availability(SessionUse::OutsideTurn),
        ActionAvailability::Enabled,
        "the view offers what the controller accepts"
    );
    assert_eq!(
        controller.session_availability(SessionUse::BetweenTurns),
        ActionAvailability::Disabled {
            reason: "Start or resume the agent first".into()
        }
    );
    assert!(controller.configure_session().is_ok());
    assert_eq!(
        controller.compact_session().unwrap_err(),
        "Start or resume the agent first"
    );
    assert!(matches!(
        sink.intents.borrow().as_slice(),
        [
            Intent::ExportSession { .. },
            Intent::ConfigureSession { .. }
        ]
    ));
}
