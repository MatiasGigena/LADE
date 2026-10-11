//! IMP-011 gestures: the model picker over the service's catalogue, Agent Session operations
//! (tree, navigate, fork/clone, import/export, compaction, reload, model and thinking), LADE-profile
//! resource approval and import, and explicit answers to extension dialogs.
//!
//! The controller only refuses what the service already projects as impossible (no live session,
//! a running turn); every decision stays with the service.

use std::path::PathBuf;

use super::compat::{ForkPosition, ModelChoice, ThinkingLevel, UiAnswerView};
use super::controller::Controller;
use super::intents::Intent;
use super::model::{ExecutionState, ModelRef, ObjectiveId};
use super::store::{Change, StoreUpdate};

impl Controller {
    fn emit_intent(&self, intent: Intent) {
        self.submit_intent(intent);
    }

    /// Loads the provider list (and the chosen provider's models when known).
    pub fn load_models(&self, provider: Option<String>) {
        self.emit_intent(Intent::LoadModels { provider });
    }

    /// Records the picker's choice; loads a newly chosen provider's models.
    pub fn choose_model(&self, choice: ModelChoice) {
        let needs_models = choice
            .provider
            .as_ref()
            .filter(|p| !self.store.state().compat.models.contains_key(*p))
            .cloned();
        self.store.apply_all(vec![StoreUpdate::Compat(
            super::compat::CompatUpdate::Choice(choice),
        )]);
        if let Some(provider) = needs_models {
            self.load_models(Some(provider));
        }
    }

    /// Selects the provider/model the picker should show first: the persisted session model, the
    /// default model, or the first available provider.
    pub fn picker_defaults(&self) {
        let (choice, missing) = {
            let state = self.store.state();
            let compat = &state.compat;
            if compat.choice.provider.is_some() {
                return;
            }
            let Some(catalog) = &compat.catalog else {
                return;
            };
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
                        .find(|p| p.status == super::compat::ProviderStatus::Available)
                        .map(|p| p.id.clone())
                });
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
                .filter(|p| !compat.models.contains_key(*p))
                .cloned();
            (choice, missing)
        };
        self.store.apply_all(vec![StoreUpdate::Compat(
            super::compat::CompatUpdate::Choice(choice),
        )]);
        if let Some(provider) = missing {
            self.load_models(Some(provider));
        }
    }

    /// The picker's resolved model, or why there is none yet.
    pub fn chosen_model(&self) -> Result<(ModelRef, ThinkingLevel), String> {
        let state = self.store.state();
        let compat = &state.compat;
        let provider = compat
            .choice
            .provider
            .as_ref()
            .ok_or("Choose a provider first")?;
        let model = compat
            .chosen_model()
            .ok_or_else(|| format!("The models of {provider} are still loading"))?;
        Ok((model.model_ref(), compat.choice.thinking))
    }

    /// The selected Objective with a live worker that can take a session operation now.
    fn live_session(&self, allow_running: bool) -> Result<ObjectiveId, String> {
        let state = self.store.state();
        let detail = state
            .selected_detail()
            .ok_or_else(|| "Select an Objective first".to_owned())?;
        if detail.session.is_none() {
            return Err("Start the agent first".to_owned());
        }
        match detail.summary.execution.state {
            ExecutionState::Idle => Ok(detail.summary.objective_id.clone()),
            ExecutionState::Running if allow_running => Ok(detail.summary.objective_id.clone()),
            ExecutionState::Running => Err("Wait until the current turn has finished".to_owned()),
            _ => Err("Start or resume the agent first".to_owned()),
        }
    }

    pub fn load_session_tree(&self) -> Result<(), String> {
        let objective_id = self.live_session(true)?;
        self.emit_intent(Intent::LoadSessionTree { objective_id });
        Ok(())
    }

    pub fn navigate_session(&self, entry_id: &str) -> Result<(), String> {
        let objective_id = self.live_session(false)?;
        self.emit_intent(Intent::NavigateSession {
            objective_id,
            entry_id: entry_id.to_owned(),
        });
        Ok(())
    }

    pub fn fork_session(&self, entry_id: &str, position: ForkPosition) -> Result<(), String> {
        let objective_id = self.live_session(false)?;
        self.emit_intent(Intent::ForkSession {
            objective_id,
            entry_id: entry_id.to_owned(),
            position,
        });
        Ok(())
    }

    pub fn import_session(&self, path: PathBuf) -> Result<(), String> {
        let objective_id = self.live_session(false)?;
        self.emit_intent(Intent::ImportSession { objective_id, path });
        Ok(())
    }

    pub fn export_session(&self) -> Result<(), String> {
        let objective_id = self.live_session(true)?;
        self.emit_intent(Intent::ExportSession { objective_id });
        Ok(())
    }

    pub fn compact_session(&self) -> Result<(), String> {
        let objective_id = self.live_session(false)?;
        self.emit_intent(Intent::CompactSession { objective_id });
        Ok(())
    }

    pub fn reload_session(&self) -> Result<(), String> {
        let objective_id = self.live_session(false)?;
        self.emit_intent(Intent::ReloadSession { objective_id });
        Ok(())
    }

    /// Applies the picker's model and thinking to the selected Objective: to the running session
    /// when it is idle, or for the next start/resume when it is stopped.
    pub fn configure_session(&self) -> Result<(), String> {
        let (model, thinking) = self.chosen_model()?;
        let objective_id = {
            let state = self.store.state();
            let detail = state
                .selected_detail()
                .ok_or_else(|| "Select an Objective first".to_owned())?;
            if detail.session.is_none() {
                return Err("Start the agent first".to_owned());
            }
            if detail.summary.execution.state == ExecutionState::Running {
                return Err("Wait until the current turn has finished".to_owned());
            }
            detail.summary.objective_id.clone()
        };
        self.emit_intent(Intent::ConfigureSession {
            objective_id,
            model,
            thinking,
        });
        Ok(())
    }

    pub fn load_resources(&self) {
        self.emit_intent(Intent::LoadResources);
    }

    pub fn decide_resource(&self, name: &str, sha256: &str, approve: bool) {
        self.emit_intent(Intent::DecideResource {
            name: name.to_owned(),
            sha256: sha256.to_owned(),
            approve,
        });
    }

    pub fn import_profile(&self, source: PathBuf) {
        self.emit_intent(Intent::ImportProfile { source });
    }

    /// Sends the developer's explicit answer to an open dialog. Closing the window without an
    /// answer is a `Cancel`, never an implicit confirmation.
    pub fn answer_dialog(&self, ui_id: &str, answer: UiAnswerView) -> Result<(), String> {
        let dialog = self
            .store
            .state()
            .compat
            .dialogs
            .iter()
            .find(|d| d.ui_id == ui_id)
            .cloned()
            .ok_or_else(|| "The dialog was already closed".to_owned())?;
        self.emit_intent(Intent::RespondDialog {
            objective_id: dialog.objective_id,
            ui_id: dialog.ui_id,
            generation: dialog.generation,
            answer,
        });
        Ok(())
    }

    /// One line describing the selected Objective's pi session (generation, thinking, reason).
    pub fn session_line(&self) -> Option<String> {
        let state = self.store.state();
        let id = state.ui.selected.as_ref()?;
        let session = state.compat.sessions.get(id)?;
        Some(format!(
            "pi session generation {} · thinking {} · {}",
            session.generation,
            session.thinking.label(),
            session.reason
        ))
    }

    /// Changes that should refresh views showing IMP-011 state for the selection.
    pub fn compat_changes_for(&self, changes: &[Change]) -> bool {
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
    use crate::presentation::compat::{
        Catalog, CompatUpdate, ModelView, ProviderStatus, ProviderView, UiDialog, UiPromptView,
    };
    use crate::presentation::intents::RecordingSink;
    use crate::presentation::model::ConnectionState;
    use crate::presentation::preview;
    use crate::presentation::store::{PresentationState, Store};

    fn setup() -> (Rc<Controller>, Rc<RecordingSink>) {
        let mut state = PresentationState::new(ConnectionState::Connected);
        for update in preview::fixture_updates() {
            state.apply(update);
        }
        let store = Store::new(state);
        let sink = Rc::new(RecordingSink::default());
        let controller = Controller::new(store, sink.clone());
        controller.select_objective(&ObjectiveId::new(preview::MAIN_OBJECTIVE));
        sink.intents.borrow_mut().clear();
        (controller, sink)
    }

    #[test]
    fn the_picker_uses_the_service_catalogue_and_loads_models_on_demand() {
        let (controller, sink) = setup();
        controller
            .store
            .apply_all(vec![StoreUpdate::Compat(CompatUpdate::Catalog(Catalog {
                sdk_version: "0.85.1".into(),
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
            }))]);
        controller.picker_defaults();
        assert_eq!(
            sink.intents.borrow().as_slice(),
            [Intent::LoadModels {
                provider: Some("local-llm".into())
            }]
        );
        assert!(controller.chosen_model().unwrap_err().contains("loading"));
        controller
            .store
            .apply_all(vec![StoreUpdate::Compat(CompatUpdate::Models {
                provider: "local-llm".into(),
                models: vec![ModelView {
                    provider: "local-llm".into(),
                    id: "scripted".into(),
                    name: "Scripted".into(),
                    reasoning: false,
                    thinking: vec![ThinkingLevel::Off],
                }],
            })]);
        let (model, thinking) = controller.chosen_model().unwrap();
        assert_eq!(model, ModelRef::new("local-llm", "scripted"));
        assert_eq!(thinking, ThinkingLevel::Off);
    }

    #[test]
    fn dialogs_are_answered_explicitly_with_their_generation() {
        let (controller, sink) = setup();
        assert!(
            controller
                .answer_dialog("missing", UiAnswerView::Cancel)
                .is_err()
        );
        controller
            .store
            .apply_all(vec![StoreUpdate::Compat(CompatUpdate::DialogOpened(
                UiDialog {
                    objective_id: ObjectiveId::new(preview::MAIN_OBJECTIVE),
                    ui_id: "00000000-0000-4000-8000-000000000042".into(),
                    generation: 3,
                    prompt: UiPromptView::Confirm {
                        title: "Proceed?".into(),
                        message: "m".into(),
                    },
                },
            ))]);
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
}
