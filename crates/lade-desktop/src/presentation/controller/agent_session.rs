use std::path::PathBuf;

use super::Controller;
use super::intents::Intent;
use crate::presentation::state::agent_session::{
    AgentSessionUpdate, ForkPosition, ModelChoice, ProviderStatus, Readiness, ThinkingLevel,
    UiAnswerView,
};
use crate::presentation::state::model::{ExecutionState, ModelRef, ObjectiveId};
use crate::presentation::state::store::{Change, StoreUpdate};

/// When an Agent Session operation may be sent, by the agent's turn state.
#[derive(Clone, Copy)]
enum SessionUse {
    /// The agent is running and idle between turns.
    BetweenTurns,
    /// The agent is running, during a turn or between turns.
    Running,
    /// No turn is in progress, including while the agent is stopped.
    OutsideTurn,
}

impl Controller {
    pub fn load_models(&self, provider: Option<String>) {
        self.emit(Intent::LoadModels { provider });
    }

    pub fn choose_model(&self, choice: ModelChoice) {
        let needs_models = choice
            .provider
            .as_ref()
            .filter(|p| !self.store.state().agent_session.models.contains_key(*p))
            .cloned();
        self.store
            .apply_all(vec![StoreUpdate::AgentSession(AgentSessionUpdate::Choice(
                choice,
            ))]);
        if let Some(provider) = needs_models {
            self.load_models(Some(provider));
        }
    }

    pub(super) fn follow_store(&self, changes: &[Change]) {
        let relevant = changes
            .iter()
            .any(|c| matches!(c, Change::Catalog | Change::Selection | Change::Detail(_)));
        if relevant {
            self.picker_defaults();
        }
    }

    pub(super) fn picker_defaults(&self) {
        let (choice, missing) = {
            let state = self.store.state();
            let agent_session = &state.agent_session;
            let Some(catalog) = &agent_session.catalog else {
                return;
            };
            if agent_session
                .choice
                .provider
                .as_ref()
                .is_some_and(|p| catalog.provider(p).is_some())
            {
                return;
            }
            let session = state.selected_detail().and_then(|d| d.session.clone());
            let preferred = session
                .as_ref()
                .map(|s| s.model.clone())
                .or_else(|| state.settings.default_model.clone());
            let provider = preferred
                .as_ref()
                .map(|m| m.provider.clone())
                .filter(|p| catalog.provider(p).is_some())
                .or_else(|| {
                    catalog
                        .providers
                        .iter()
                        .find(|p| p.status == ProviderStatus::Available)
                        .or_else(|| catalog.providers.first())
                        .map(|p| p.id.clone())
                });
            if provider.is_none() {
                return;
            }
            let choice = ModelChoice {
                model: preferred
                    .filter(|m| Some(&m.provider) == provider.as_ref())
                    .map(|m| m.id),
                thinking: session.and_then(|s| s.thinking).unwrap_or_default(),
                provider,
            };
            let missing = choice
                .provider
                .as_ref()
                .filter(|p| !agent_session.models.contains_key(*p))
                .cloned();
            (choice, missing)
        };
        self.store
            .apply_all(vec![StoreUpdate::AgentSession(AgentSessionUpdate::Choice(
                choice,
            ))]);
        if let Some(provider) = missing {
            self.load_models(Some(provider));
        }
    }

    pub fn readiness(&self) -> Readiness {
        self.store.state().agent_session.readiness()
    }

    pub fn chosen_model(&self) -> Result<(ModelRef, ThinkingLevel), String> {
        match self.readiness() {
            Readiness::Ready { model, thinking } => Ok((model.model_ref(), thinking)),
            other => Err(other.problem().unwrap_or_default()),
        }
    }

    fn session_target(&self, when: SessionUse) -> Result<ObjectiveId, String> {
        let state = self.store.state();
        let detail = state
            .selected_detail()
            .ok_or_else(|| "Select an Objective first".to_owned())?;
        if detail.session.is_none() {
            return Err("Start the agent first".to_owned());
        }
        let target = detail.summary.objective_id.clone();
        match (detail.summary.execution.state, when) {
            (ExecutionState::Idle, _) | (ExecutionState::Running, SessionUse::Running) => {
                Ok(target)
            }
            (ExecutionState::Running, _) => {
                Err("Wait until the current turn has finished".to_owned())
            }
            (_, SessionUse::OutsideTurn) => Ok(target),
            _ => Err("Start or resume the agent first".to_owned()),
        }
    }

    pub fn load_session_tree(&self) -> Result<(), String> {
        let objective_id = self.session_target(SessionUse::Running)?;
        self.emit(Intent::LoadSessionTree { objective_id });
        Ok(())
    }

    pub fn navigate_session(&self, entry_id: &str) -> Result<(), String> {
        let objective_id = self.session_target(SessionUse::BetweenTurns)?;
        self.emit(Intent::NavigateSession {
            objective_id,
            entry_id: entry_id.to_owned(),
        });
        Ok(())
    }

    pub fn fork_session(&self, entry_id: &str, position: ForkPosition) -> Result<(), String> {
        let objective_id = self.session_target(SessionUse::BetweenTurns)?;
        self.emit(Intent::ForkSession {
            objective_id,
            entry_id: entry_id.to_owned(),
            position,
        });
        Ok(())
    }

    pub fn import_session(&self, path: PathBuf) -> Result<(), String> {
        let objective_id = self.session_target(SessionUse::BetweenTurns)?;
        self.emit(Intent::ImportSession { objective_id, path });
        Ok(())
    }

    pub fn export_session(&self) -> Result<(), String> {
        let objective_id = self.session_target(SessionUse::Running)?;
        self.emit(Intent::ExportSession { objective_id });
        Ok(())
    }

    pub fn compact_session(&self) -> Result<(), String> {
        let objective_id = self.session_target(SessionUse::BetweenTurns)?;
        self.emit(Intent::CompactSession { objective_id });
        Ok(())
    }

    pub fn reload_session(&self) -> Result<(), String> {
        let objective_id = self.session_target(SessionUse::BetweenTurns)?;
        self.emit(Intent::ReloadSession { objective_id });
        Ok(())
    }

    pub fn configure_session(&self) -> Result<(), String> {
        let (model, thinking) = self.chosen_model()?;
        let objective_id = self.session_target(SessionUse::OutsideTurn)?;
        self.emit(Intent::ConfigureSession {
            objective_id,
            model,
            thinking,
        });
        Ok(())
    }

    pub fn load_resources(&self) {
        self.emit(Intent::LoadResources);
    }

    pub fn decide_resource(&self, name: &str, sha256: &str, approve: bool) {
        self.emit(Intent::DecideResource {
            name: name.to_owned(),
            sha256: sha256.to_owned(),
            approve,
        });
    }

    pub fn import_profile(&self, source: PathBuf) {
        self.emit(Intent::ImportProfile { source });
    }

    pub fn answer_dialog(&self, ui_id: &str, answer: UiAnswerView) -> Result<(), String> {
        let dialog = self
            .store
            .state()
            .agent_session
            .dialogs
            .iter()
            .find(|d| d.ui_id == ui_id)
            .cloned()
            .ok_or_else(|| "The dialog was already closed".to_owned())?;
        self.emit(Intent::RespondDialog {
            objective_id: dialog.objective_id,
            ui_id: dialog.ui_id,
            generation: dialog.generation,
            answer,
        });
        Ok(())
    }

    pub fn session_line(&self) -> Option<String> {
        let state = self.store.state();
        let id = state.ui.selected.as_ref()?;
        let session = state.agent_session.sessions.get(id)?;
        Some(format!(
            "pi session generation {} · thinking {} · {}",
            session.generation,
            session.thinking.label(),
            session.reason
        ))
    }

    pub fn agent_session_changes_for(&self, changes: &[Change]) -> bool {
        let selected = self.store.state().ui.selected.clone();
        changes.iter().any(|c| match c {
            Change::Catalog | Change::Resources => true,
            Change::Session(id) => Some(id) == selected.as_ref(),
            _ => false,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::*;
    use crate::presentation::controller::intents::RecordingSink;
    use crate::presentation::state::agent_session::{
        AgentSessionUpdate, Catalog, ModelView, ProviderStatus, ProviderView, UiDialog,
        UiPromptView,
    };
    use crate::presentation::state::model::{ConnectionState, Load};
    use crate::presentation::state::store::{PresentationState, Store};
    use crate::presentation::test_fixture;

    fn setup() -> (Rc<Controller>, Rc<RecordingSink>) {
        let mut state = PresentationState::new(ConnectionState::Connected);
        for update in test_fixture::fixture_updates() {
            state.apply(update);
        }
        let store = Store::new(state);
        let sink = Rc::new(RecordingSink::default());
        let controller = Controller::new(store, sink.clone());
        controller.select_objective(&ObjectiveId::new(test_fixture::MAIN_OBJECTIVE));
        sink.intents.borrow_mut().clear();
        (controller, sink)
    }

    #[test]
    fn the_picker_uses_the_service_catalogue_and_loads_models_on_demand() {
        let (controller, sink) = setup();
        controller.store.apply_all(vec![StoreUpdate::AgentSession(
            AgentSessionUpdate::Catalog(Catalog {
                sdk_version: "1.5.0".into(),
                providers: vec![ProviderView {
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
                models: vec![ModelView {
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

    fn catalog(providers: Vec<ProviderView>, error: Option<&str>) -> StoreUpdate {
        StoreUpdate::AgentSession(AgentSessionUpdate::Catalog(Catalog {
            sdk_version: "1.5.0".into(),
            providers,
            error: error.map(str::to_owned),
        }))
    }

    #[test]
    fn readiness_tells_apart_no_catalogue_no_provider_loading_and_no_models() {
        let (controller, sink) = setup();
        controller.store.update(|s| {
            s.agent_session = Default::default();
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
            vec![ProviderView {
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
        let anthropic = |status| ProviderView {
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
        let (controller, sink) = setup();
        assert!(
            controller
                .answer_dialog("missing", UiAnswerView::Cancel)
                .is_err()
        );
        controller.store.apply_all(vec![StoreUpdate::AgentSession(
            AgentSessionUpdate::DialogOpened(UiDialog {
                objective_id: ObjectiveId::new(test_fixture::MAIN_OBJECTIVE),
                ui_id: "00000000-0000-4000-8000-000000000042".into(),
                generation: 3,
                prompt: UiPromptView::Confirm {
                    title: "Proceed?".into(),
                    message: "m".into(),
                },
            }),
        )]);
        controller
            .answer_dialog(
                "00000000-0000-4000-8000-000000000042",
                UiAnswerView::Confirm(true),
            )
            .unwrap();
        assert!(matches!(
            sink.intents.borrow().as_slice(),
            [Intent::RespondDialog {
                generation: 3,
                answer: UiAnswerView::Confirm(true),
                ..
            }]
        ));
    }

    #[test]
    fn session_operations_need_a_live_session() {
        let (controller, sink) = setup();
        let state = controller
            .store
            .state()
            .selected_detail()
            .map(|d| d.summary.execution.state);
        let result = controller.compact_session();
        match state {
            Some(ExecutionState::Idle) => assert!(result.is_ok()),
            _ => {
                assert!(result.is_err());
                assert!(sink.intents.borrow().is_empty());
            }
        }
    }

    #[test]
    fn configuring_waits_only_for_a_running_turn() {
        let (controller, sink) = setup();
        controller.store.apply_all(vec![
            catalog(
                vec![ProviderView {
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
                models: vec![ModelView {
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
            controller.compact_session().unwrap_err(),
            "Wait until the current turn has finished"
        );
        assert!(controller.export_session().is_ok());
        execution(ExecutionState::Cancelled);
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
}
