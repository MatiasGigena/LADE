//! Execution: the agent run attached to an Objective, its generations, and the Agent Session it is
//! associated with.

mod model;
mod state;
#[cfg(test)]
mod tests;

pub use model::{MAX_LABEL_BYTES, ModelRef};
pub use state::{ExecutionGeneration, ExecutionState, SessionAssociation};

use crate::domain::ids::{Revision, SessionId};
use crate::error::DomainError;

pub const MAX_FAILURE_REASON_BYTES: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Execution {
    state: ExecutionState,
    generation: ExecutionGeneration,
    session: Option<SessionAssociation>,
}

impl Default for Execution {
    fn default() -> Self {
        Self::new()
    }
}

impl Execution {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: ExecutionState::None,
            generation: ExecutionGeneration::NONE,
            session: None,
        }
    }

    /// Rebuilds an execution from stored parts.
    ///
    /// # Errors
    ///
    /// `DomainError::DurabilityFailure` when the parts contradict each other: a never-started
    /// generation with a state or session, or a started one without them.
    pub fn restore(
        state: ExecutionState,
        generation: ExecutionGeneration,
        session: Option<SessionAssociation>,
    ) -> Result<Self, DomainError> {
        let never_started = generation == ExecutionGeneration::NONE;
        let consistent = if never_started {
            state == ExecutionState::None && session.is_none()
        } else {
            state != ExecutionState::None && session.is_some()
        };
        if !consistent {
            return Err(DomainError::DurabilityFailure(
                "inconsistent stored execution state",
            ));
        }
        Ok(Self {
            state,
            generation,
            session,
        })
    }

    #[must_use]
    pub const fn state(&self) -> &ExecutionState {
        &self.state
    }

    #[must_use]
    pub const fn generation(&self) -> ExecutionGeneration {
        self.generation
    }

    #[must_use]
    pub const fn session(&self) -> Option<&SessionAssociation> {
        self.session.as_ref()
    }

    /// Starts the first run, bound to `session_id`, and returns its generation.
    ///
    /// # Errors
    ///
    /// - `DomainError::InvalidLifecycle` when the execution has already started.
    /// - `DomainError::LimitReached` when the generation counter is exhausted.
    pub fn start(
        &mut self,
        session_id: SessionId,
        model: ModelRef,
    ) -> Result<ExecutionGeneration, DomainError> {
        if self.state != ExecutionState::None || self.session.is_some() {
            return Err(DomainError::InvalidLifecycle(
                "The agent has already started. Use Resume instead.",
            ));
        }
        let generation = self.generation.advance()?;
        self.session = Some(SessionAssociation {
            session_id,
            association_revision: Revision::INITIAL,
            model,
        });
        self.generation = generation;
        self.state = ExecutionState::Starting;
        Ok(generation)
    }

    /// Starts the next generation of a cancelled or failed run on the same session.
    ///
    /// # Errors
    ///
    /// - `DomainError::InvalidLifecycle` unless the run is cancelled or failed.
    /// - `DomainError::LimitReached` when the generation counter is exhausted.
    pub fn resume(&mut self) -> Result<ExecutionGeneration, DomainError> {
        if !matches!(
            self.state,
            ExecutionState::Cancelled { .. } | ExecutionState::Failed { .. }
        ) {
            return Err(DomainError::InvalidLifecycle(
                "Only a stopped or failed agent can be resumed.",
            ));
        }
        let generation = self.generation.advance()?;
        self.generation = generation;
        self.state = ExecutionState::Starting;
        Ok(generation)
    }

    /// Checks that a fact reported for `generation` belongs to the current run.
    ///
    /// # Errors
    ///
    /// `DomainError::StaleExecution` when `generation` is not the current one, or nothing has
    /// started.
    pub fn check_generation(&self, generation: ExecutionGeneration) -> Result<(), DomainError> {
        if generation != self.generation || self.generation == ExecutionGeneration::NONE {
            return Err(DomainError::StaleExecution);
        }
        Ok(())
    }

    /// Checks that input addressed to `session_id` at `association_revision` still reaches the
    /// current session.
    ///
    /// # Errors
    ///
    /// - `DomainError::StaleAssociation` when the session or its association revision has changed.
    /// - `DomainError::ExecutionUnavailable` when no session is bound.
    pub fn check_association(
        &self,
        session_id: SessionId,
        association_revision: Revision,
    ) -> Result<(), DomainError> {
        match &self.session {
            Some(current)
                if current.session_id == session_id
                    && current.association_revision == association_revision =>
            {
                Ok(())
            }
            Some(_) => Err(DomainError::StaleAssociation),
            None => Err(DomainError::ExecutionUnavailable),
        }
    }

    /// Records that the worker of `generation` is ready, moving a starting run to idle.
    ///
    /// # Errors
    ///
    /// - `DomainError::StaleExecution` for a fact from another generation.
    /// - `DomainError::InvalidLifecycle` when the run is not starting.
    pub fn worker_ready(&mut self, generation: ExecutionGeneration) -> Result<(), DomainError> {
        self.check_generation(generation)?;
        match self.state {
            ExecutionState::Starting => {
                self.state = ExecutionState::Idle;
                Ok(())
            }
            _ => Err(DomainError::InvalidLifecycle(
                "worker readiness outside starting",
            )),
        }
    }

    /// Records whether the agent of `generation` is mid-turn; ignored while stopping.
    ///
    /// # Errors
    ///
    /// - `DomainError::StaleExecution` for a fact from another generation.
    /// - `DomainError::InvalidLifecycle` unless the run is running, idle or stopping.
    pub fn observe_turn(
        &mut self,
        generation: ExecutionGeneration,
        running: bool,
    ) -> Result<(), DomainError> {
        self.check_generation(generation)?;
        match self.state {
            ExecutionState::Running | ExecutionState::Idle => {
                self.state = if running {
                    ExecutionState::Running
                } else {
                    ExecutionState::Idle
                };
                Ok(())
            }
            ExecutionState::Stopping => Ok(()),
            _ => Err(DomainError::InvalidLifecycle(
                "turn state outside a ready execution",
            )),
        }
    }

    /// Asks a starting, running or idle run to stop.
    ///
    /// # Errors
    ///
    /// `DomainError::InvalidLifecycle` when the run is not live or is already stopping.
    pub fn request_stop(&mut self) -> Result<(), DomainError> {
        match self.state {
            ExecutionState::Starting | ExecutionState::Running | ExecutionState::Idle => {
                self.state = ExecutionState::Stopping;
                Ok(())
            }
            _ => Err(DomainError::InvalidLifecycle("The agent isn't running.")),
        }
    }

    /// Records that the run of `generation` stopped after a stop request.
    ///
    /// # Errors
    ///
    /// - `DomainError::StaleExecution` for a fact from another generation.
    /// - `DomainError::InvalidLifecycle` when no stop was requested.
    pub fn stopped(&mut self, generation: ExecutionGeneration) -> Result<(), DomainError> {
        self.check_generation(generation)?;
        match self.state {
            ExecutionState::Stopping => {
                self.state = ExecutionState::Cancelled { interrupted: false };
                Ok(())
            }
            _ => Err(DomainError::InvalidLifecycle(
                "stop observed without a stop request",
            )),
        }
    }

    /// Records that the live run of `generation` failed, keeping a clipped `reason`.
    ///
    /// # Errors
    ///
    /// - `DomainError::StaleExecution` for a fact from another generation.
    /// - `DomainError::InvalidLifecycle` when the run is not live.
    pub fn fail(
        &mut self,
        generation: ExecutionGeneration,
        reason: &str,
    ) -> Result<(), DomainError> {
        self.check_generation(generation)?;
        if !self.state.is_live() {
            return Err(DomainError::InvalidLifecycle("no live execution to fail"));
        }
        self.state = ExecutionState::Failed {
            reason: clip(reason, MAX_FAILURE_REASON_BYTES),
        };
        Ok(())
    }

    pub fn interrupt(&mut self) -> bool {
        if !self.state.is_live() {
            return false;
        }
        self.state = ExecutionState::Cancelled { interrupted: true };
        true
    }

    /// Moves an idle run to `session_id`, keeping its model, and returns the new association
    /// revision.
    ///
    /// # Errors
    ///
    /// - `DomainError::StaleExecution` for another generation.
    /// - `DomainError::InvalidLifecycle` unless the run is idle.
    /// - `DomainError::ExecutionUnavailable` when no session is bound.
    /// - `DomainError::InvalidRequest` when `session_id` is the current session.
    /// - `DomainError::LimitReached` when the association revision is exhausted.
    pub fn rebind_live_session(
        &mut self,
        generation: ExecutionGeneration,
        session_id: SessionId,
    ) -> Result<Revision, DomainError> {
        self.check_generation(generation)?;
        if self.state != ExecutionState::Idle {
            return Err(DomainError::InvalidLifecycle(
                "Wait until the agent is idle before replacing its session.",
            ));
        }
        let Some(current) = &self.session else {
            return Err(DomainError::ExecutionUnavailable);
        };
        if current.session_id == session_id {
            return Err(DomainError::InvalidRequest(
                "replacement session must differ",
            ));
        }
        let association_revision = current.association_revision.advance()?;
        self.session = Some(SessionAssociation {
            session_id,
            association_revision,
            model: current.model.clone(),
        });
        Ok(association_revision)
    }

    /// Changes the model of the bound session without changing the destination.
    ///
    /// # Errors
    ///
    /// `DomainError::ExecutionUnavailable` when no session is bound.
    pub fn select_model(&mut self, model: ModelRef) -> Result<(), DomainError> {
        let Some(current) = self.session.as_mut() else {
            return Err(DomainError::ExecutionUnavailable);
        };
        current.model = model;
        Ok(())
    }

    /// Binds a stopped run to another session and model, and returns the new association revision.
    ///
    /// # Errors
    ///
    /// - `DomainError::InvalidLifecycle` while the run is live.
    /// - `DomainError::ExecutionUnavailable` when no session is bound.
    /// - `DomainError::InvalidRequest` when `session_id` is the current session.
    /// - `DomainError::LimitReached` when the association revision is exhausted.
    pub fn replace_session(
        &mut self,
        session_id: SessionId,
        model: ModelRef,
    ) -> Result<Revision, DomainError> {
        if self.state.is_live() {
            return Err(DomainError::InvalidLifecycle(
                "Stop the agent before replacing its session.",
            ));
        }
        let Some(current) = &self.session else {
            return Err(DomainError::ExecutionUnavailable);
        };
        if current.session_id == session_id {
            return Err(DomainError::InvalidRequest(
                "replacement session must differ",
            ));
        }
        let association_revision = current.association_revision.advance()?;
        self.session = Some(SessionAssociation {
            session_id,
            association_revision,
            model,
        });
        Ok(association_revision)
    }
}

fn clip(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}
