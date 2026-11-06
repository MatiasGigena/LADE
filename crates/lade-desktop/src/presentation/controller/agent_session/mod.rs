//! Controller commands for an Objective's Agent Session: starting and configuring the agent,
//! session tools, profile resources and extension dialog answers.

use std::path::PathBuf;

use super::Controller;
use super::intents::Intent;
use crate::presentation::state::agent_session::{
    AgentSessionUpdate, ForkPosition, ModelChoice, ProviderStatus, Readiness, ThinkingLevel,
    UiAnswer,
};
use crate::presentation::state::model::{
    ActionAvailability, ExecutionState, ModelRef, ObjectiveId,
};
use crate::presentation::state::store::{Change, StoreUpdate};

/// When an Agent Session operation may be sent, by the agent's turn state.
#[derive(Clone, Copy)]
pub enum SessionUse {
    /// The agent is running and idle between turns.
    BetweenTurns,
    /// The agent is running, during a turn or between turns.
    Running,
    /// No turn is in progress, including while the agent is stopped.
    OutsideTurn,
}

impl SessionUse {
    /// Whether an operation of this kind may be sent in `state`, or why not.
    fn allowed_in(self, state: ExecutionState) -> Result<(), &'static str> {
        match (state, self) {
            (ExecutionState::Idle, _) | (ExecutionState::Running, Self::Running) => Ok(()),
            (ExecutionState::Running, _) => Err("Wait until the current turn has finished"),
            (_, Self::OutsideTurn) => Ok(()),
            _ => Err("Start or resume the agent first"),
        }
    }
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
            self.apply_picker_defaults();
        }
    }

    /// Chooses a default provider, model and thinking level when the current choice is not in
    /// the catalogue, writes it to the store and requests that provider's models if missing.
    fn apply_picker_defaults(&self) {
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
        when.allowed_in(detail.summary.execution.state)?;
        Ok(detail.summary.objective_id.clone())
    }

    /// How a view should offer an Agent Session operation sent under `when`, by the same rule
    /// the operation itself checks. Without a session the operation is not offered at all.
    pub fn session_availability(&self, when: SessionUse) -> ActionAvailability {
        let state = self.store.state();
        match state.selected_detail() {
            Some(detail) if detail.session.is_some() => {
                match when.allowed_in(detail.summary.execution.state) {
                    Ok(()) => ActionAvailability::Enabled,
                    Err(reason) => ActionAvailability::Disabled {
                        reason: reason.to_owned(),
                    },
                }
            }
            _ => ActionAvailability::NotOffered,
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

    pub fn answer_dialog(&self, ui_id: &str, answer: UiAnswer) -> Result<(), String> {
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
}

#[cfg(test)]
mod tests;
