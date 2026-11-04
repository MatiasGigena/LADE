//! Objective lifecycle and review commands, checked against the actions the service offers.

use super::intents::{Intent, StartPoint};
use crate::presentation::state::agent_session::{Readiness, ThinkingLevel};
use crate::presentation::state::model::{
    ActionAvailability, ActionName, ApprovalState, CandidateSummary, DeliveryMode, ExecutionState,
    Intent as ObjectiveIntent, ModelRef, ObjectiveDetail, ObjectiveId, VerificationState,
    VerificationSummary,
};
use crate::presentation::text::format;

use super::Controller;

impl Controller {
    fn command(&self, action: ActionName, intent: Intent) -> Result<(), String> {
        let availability = self
            .with_detail(|d| d.availability(action))
            .ok_or("The Objective is not loaded")?;
        match availability {
            ActionAvailability::Enabled => {
                self.emit(intent);
                Ok(())
            }
            ActionAvailability::Disabled { reason } => Err(reason),
            ActionAvailability::NotOffered => Err("This action is not offered".to_owned()),
        }
    }

    pub(super) fn selected_or_err(&self) -> Result<ObjectiveId, String> {
        self.selected()
            .ok_or_else(|| "Select an Objective first".to_owned())
    }

    fn current_candidate(&self) -> Result<CandidateSummary, String> {
        self.with_detail(|d| d.current_candidate().cloned())
            .flatten()
            .ok_or_else(|| "There is no current candidate".to_owned())
    }

    pub fn update_intent(
        &self,
        expected_intent_revision: u64,
        intent: ObjectiveIntent,
    ) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        if intent.outcome.trim().is_empty() {
            return Err("Describe the expected outcome".to_owned());
        }
        let unchanged = self
            .with_detail(|d| d.intent == intent)
            .ok_or("The Objective is not loaded")?;
        if unchanged {
            return Err("Nothing changed".to_owned());
        }
        self.command(
            ActionName::ObjectiveUpdateIntent,
            Intent::UpdateObjective {
                objective_id,
                expected_intent_revision,
                intent,
            },
        )
    }

    pub fn start_execution(
        &self,
        model: ModelRef,
        thinking: Option<ThinkingLevel>,
    ) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        let is_default = self.store.state().settings.default_model.as_ref() == Some(&model);
        self.command(
            ActionName::ExecutionStart,
            Intent::StartExecution {
                objective_id,
                model: model.clone(),
                thinking,
            },
        )?;
        if !is_default {
            self.emit(Intent::UpdateSettings {
                default_model: model,
            });
        }
        Ok(())
    }

    pub fn cancel(&self) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        self.command(ActionName::ExecutionCancel, Intent::Cancel { objective_id })
    }

    pub fn resume(&self) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        self.command(ActionName::ExecutionResume, Intent::Resume { objective_id })
    }

    pub fn set_composer(&self, text: &str, mode: DeliveryMode) {
        let Some(id) = self.selected() else { return };
        self.store.update(|s| {
            let ui = s.objective_ui_mut(&id);
            text.clone_into(&mut ui.composer_text);
            ui.composer_mode = mode;
            ((), Vec::new())
        });
    }

    pub fn send_message(&self, text: &str, mode: DeliveryMode) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        if text.trim().is_empty() {
            return Err("Type a message first".to_owned());
        }
        self.command(
            ActionName::MessageSend,
            Intent::SendMessage {
                objective_id,
                text: text.to_owned(),
                mode,
            },
        )
    }

    pub fn freeze(&self) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        self.command(ActionName::ReviewFreeze, Intent::Freeze { objective_id })
    }

    pub fn return_for_revision(&self) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        let candidate = self.current_candidate()?;
        self.command(
            ActionName::ReviewReturn,
            Intent::ReturnForRevision {
                objective_id,
                candidate_id: candidate.candidate_id,
            },
        )
    }

    pub fn verify(&self, command: &str) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        let argv = format::split_argv(command).map_err(str::to_owned)?;
        let candidate = self.current_candidate()?;
        self.command(
            ActionName::ReviewVerify,
            Intent::Verify {
                objective_id,
                candidate_id: candidate.candidate_id,
                argv,
            },
        )
    }

    pub fn approval_needs_override(&self) -> bool {
        self.with_detail(|d| {
            let Some(candidate) = d.current_candidate() else {
                return true;
            };
            let mut latest: Vec<&VerificationSummary> = Vec::new();
            for v in d
                .verifications
                .iter()
                .filter(|v| v.candidate_id == candidate.candidate_id)
            {
                match latest.iter_mut().find(|l| l.command == v.command) {
                    Some(l) if l.started_at < v.started_at => *l = v,
                    Some(_) => {}
                    None => latest.push(v),
                }
            }
            latest.is_empty() || latest.iter().any(|v| v.state != VerificationState::Passed)
        })
        .unwrap_or(true)
    }

    pub fn approve(&self, override_reason: Option<String>) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        let candidate = self.current_candidate()?;
        let target_commit = self
            .with_detail(|d| d.integration_target.commit.clone())
            .ok_or("The Objective is not loaded")?;
        let override_reason = override_reason.filter(|r| !r.trim().is_empty());
        if self.approval_needs_override() && override_reason.is_none() {
            return Err(
                "Checks are missing or not all passed — give an override reason".to_owned(),
            );
        }
        self.command(
            ActionName::ReviewApprove,
            Intent::Approve {
                objective_id,
                candidate_id: candidate.candidate_id,
                target_commit,
                override_reason,
            },
        )
    }

    pub fn refresh_candidate(&self) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        let candidate = self.current_candidate()?;
        self.command(
            ActionName::ReviewRefresh,
            Intent::Refresh {
                objective_id,
                candidate_id: candidate.candidate_id,
            },
        )
    }

    pub fn integrate(&self) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        let approval_id = self
            .with_detail(|d| {
                d.approval
                    .as_ref()
                    .filter(|a| a.state == ApprovalState::Valid)
                    .map(|a| a.approval_id.clone())
            })
            .flatten()
            .ok_or("There is no valid approval")?;
        self.command(
            ActionName::ReviewIntegrate,
            Intent::Integrate {
                objective_id,
                approval_id,
            },
        )
    }

    pub fn complete(&self) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        self.command(
            ActionName::ObjectiveComplete,
            Intent::Complete { objective_id },
        )
    }

    pub fn discard(&self) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        self.command(
            ActionName::ObjectiveDiscard,
            Intent::Discard { objective_id },
        )
    }

    pub fn cleanup(&self, confirm_data_loss: bool) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        self.command(
            ActionName::ObjectiveCleanup,
            Intent::Cleanup {
                objective_id,
                confirm_data_loss,
            },
        )
    }

    pub fn delete_history(&self) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        self.command(
            ActionName::ObjectiveDeleteHistory,
            Intent::DeleteHistory { objective_id },
        )
    }

    pub fn create_objective(
        &self,
        title: &str,
        intent: ObjectiveIntent,
        integration_target: &str,
        start: StartPoint,
    ) -> Result<(), String> {
        let title = title.trim();
        if title.is_empty() || title.chars().count() > 200 {
            return Err("Give the Objective a title of 1–200 characters".to_owned());
        }
        if intent.outcome.trim().is_empty() {
            return Err("Describe the expected outcome".to_owned());
        }
        let start = match start {
            StartPoint::Target => StartPoint::Target,
            StartPoint::Commit(hex) => {
                let hex = hex.trim().to_ascii_lowercase();
                if !matches!(hex.len(), 40 | 64) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(
                        "Enter the full 40- or 64-character commit id, or leave the start point empty"
                            .to_owned(),
                    );
                }
                StartPoint::Commit(hex)
            }
        };
        let workspace_id = self
            .store
            .state()
            .active_workspace
            .clone()
            .ok_or("Open a Git repository first")?;
        self.emit(Intent::CreateObjective {
            workspace_id,
            title: title.to_owned(),
            intent,
            integration_target: integration_target.to_owned(),
            start,
        });
        Ok(())
    }

    pub fn agent_line(&self) -> Option<AgentLine> {
        let readiness = self.readiness();
        self.with_detail(|d| agent_line(d, &readiness))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentLine {
    pub sentence: String,
    pub details: Option<String>,
}

pub(super) fn agent_line(d: &ObjectiveDetail, readiness: &Readiness) -> AgentLine {
    let resumable = d.availability(ActionName::ExecutionResume) == ActionAvailability::Enabled;
    let resume = |what: &str| {
        if resumable {
            format!("{what} Resume it to continue the same Agent Session.")
        } else {
            what.to_owned()
        }
    };
    let sentence = match (&d.session, d.summary.execution.state) {
        (_, ExecutionState::Starting) => "The agent is starting.".to_owned(),
        (_, ExecutionState::Running) => "The agent is working on a turn.".to_owned(),
        (_, ExecutionState::Idle) => "The agent is idle and waiting for your message.".to_owned(),
        (_, ExecutionState::Stopping) => "The agent is stopping.".to_owned(),
        (_, ExecutionState::Cancelled) => resume("The agent was cancelled."),
        (_, ExecutionState::Failed) => resume("The agent stopped after an error."),
        (Some(_), ExecutionState::None) => resume("The agent isn't running."),
        (None, ExecutionState::None) => match d.availability(ActionName::ExecutionStart) {
            ActionAvailability::NotOffered => "The agent hasn't started.".to_owned(),
            ActionAvailability::Disabled { .. } => {
                "The agent hasn't started and can't be started right now.".to_owned()
            }
            ActionAvailability::Enabled => format!(
                "The agent hasn't started. {}",
                match readiness {
                    Readiness::Ready { model, .. } =>
                        format!("Start it below with {}.", model.name),
                    Readiness::CatalogLoading => "Loading the models it can use…".to_owned(),
                    Readiness::NoProviders { .. } => {
                        "No model provider is available to start it.".to_owned()
                    }
                    Readiness::NoProviderChosen => {
                        "Choose a model provider below to start it.".to_owned()
                    }
                    Readiness::ModelsLoading { provider } => {
                        format!("Loading {provider}'s models…")
                    }
                    Readiness::NoModels { provider } => {
                        format!("{provider} has no models; choose another provider below.")
                    }
                    Readiness::NeedsCredentials { provider } => format!(
                        "Add credentials for {provider} to start it, or choose another provider \
                         below."
                    ),
                    Readiness::Restricted { provider } =>
                        format!("{provider} can't be used in LADE; choose another provider below."),
                }
            ),
        },
    };
    let details = d.session.as_ref().map(|s| {
        let mut details = format!(
            "Agent Session {} · association r{} · execution generation {} · model {}",
            s.session_id.as_str(),
            s.association_revision,
            s.execution_generation,
            s.model.label(),
        );
        if let Some(thinking) = s.thinking {
            details.push_str(&format!(" · thinking {}", thinking.label()));
        }
        details
    });
    AgentLine { sentence, details }
}
