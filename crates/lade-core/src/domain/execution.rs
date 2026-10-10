//! Agent Session association and execution generations.
//!
//! These are pure execution state-machine rules. Lifecycle preconditions (for example "no mutating execution
//! while frozen") are enforced by [`crate::domain::objectives::Objective`], which owns the only mutable
//! [`Execution`].

use crate::domain::ids::{Revision, SessionId};
use crate::error::DomainError;

/// Bound for provider/model labels and failure reasons.
pub const MAX_LABEL_BYTES: usize = 256;
/// Failure reasons are clipped to the protocol's reply-detail bound.
pub const MAX_FAILURE_REASON_BYTES: usize = 512;

/// Selected provider/model pair.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ModelRef {
    provider: String,
    id: String,
}

impl ModelRef {
    pub fn new(provider: &str, id: &str) -> Result<Self, DomainError> {
        let valid = |text: &str| {
            !text.is_empty()
                && text.len() <= MAX_LABEL_BYTES
                && !text.chars().any(|c| c.is_control() || c.is_whitespace())
        };
        if !valid(provider) || !valid(id) {
            return Err(DomainError::InvalidRequest(
                "model provider/id must be non-empty labels",
            ));
        }
        Ok(Self {
            provider: provider.to_string(),
            id: id.to_string(),
        })
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

/// Monotonic execution generation; 0 means never started. Each start or resume advances it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExecutionGeneration(u64);

impl ExecutionGeneration {
    pub const NONE: Self = Self(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }

    fn advance(self) -> Result<Self, DomainError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(DomainError::LimitReached("execution generation exhausted"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionState {
    None,
    Starting,
    Running,
    Idle,
    Stopping,
    /// Reversible stop. `interrupted` marks a stop caused by service restart/supervisor loss rather than by the
    /// developer.
    Cancelled {
        interrupted: bool,
    },
    Failed {
        reason: String,
    },
}

impl ExecutionState {
    /// A worker process may exist (and may write) in these states.
    pub const fn is_live(&self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Running | Self::Idle | Self::Stopping
        )
    }

    /// Accepted input may be handed to the worker in these states.
    pub const fn accepts_dispatch(&self) -> bool {
        matches!(self, Self::Running | Self::Idle)
    }

    pub const fn code(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Idle => "idle",
            Self::Stopping => "stopping",
            Self::Cancelled { .. } => "cancelled",
            Self::Failed { .. } => "failed",
        }
    }
}

/// The Objective's destination Agent Session. `association_revision` advances whenever the destination is
/// replaced, so input addressed to an older association can be detected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionAssociation {
    pub session_id: SessionId,
    pub association_revision: Revision,
    pub model: ModelRef,
}

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
    /// Never started: no session, generation 0.
    pub const fn new() -> Self {
        Self {
            state: ExecutionState::None,
            generation: ExecutionGeneration::NONE,
            session: None,
        }
    }

    /// Rebuilds stored execution state, rejecting combinations no transition can produce.
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

    pub const fn state(&self) -> &ExecutionState {
        &self.state
    }

    pub const fn generation(&self) -> ExecutionGeneration {
        self.generation
    }

    pub const fn session(&self) -> Option<&SessionAssociation> {
        self.session.as_ref()
    }

    /// First start: binds the destination session (association revision 1) and starts generation 1.
    pub fn start(
        &mut self,
        session_id: SessionId,
        model: ModelRef,
    ) -> Result<ExecutionGeneration, DomainError> {
        if self.state != ExecutionState::None || self.session.is_some() {
            return Err(DomainError::InvalidLifecycle(
                "execution already started; resume instead",
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

    /// Explicit continuation after cancellation, interruption or failure: a new generation, same session.
    pub fn resume(&mut self) -> Result<ExecutionGeneration, DomainError> {
        if !matches!(
            self.state,
            ExecutionState::Cancelled { .. } | ExecutionState::Failed { .. }
        ) {
            return Err(DomainError::InvalidLifecycle(
                "only a cancelled or failed execution can resume",
            ));
        }
        let generation = self.generation.advance()?;
        self.generation = generation;
        self.state = ExecutionState::Starting;
        Ok(generation)
    }

    pub fn check_generation(&self, generation: ExecutionGeneration) -> Result<(), DomainError> {
        if generation != self.generation || self.generation == ExecutionGeneration::NONE {
            return Err(DomainError::StaleExecution);
        }
        Ok(())
    }

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

    /// Worker reported readiness for the current generation.
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

    /// Worker turn state. Ignored (no change) while stopping: a stop request is never undone by the worker.
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

    /// Developer cancellation request (or a service-initiated stop, e.g. before freeze).
    pub fn request_stop(&mut self) -> Result<(), DomainError> {
        match self.state {
            ExecutionState::Starting | ExecutionState::Running | ExecutionState::Idle => {
                self.state = ExecutionState::Stopping;
                Ok(())
            }
            _ => Err(DomainError::InvalidLifecycle("no live execution to stop")),
        }
    }

    /// The supervisor proved the worker and its descendants stopped after a stop request.
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

    /// The worker of the current generation failed or exited unexpectedly.
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

    /// Service restart or supervisor loss: every live execution becomes cancelled/interrupted. Nothing resumes
    /// automatically. Returns whether anything changed.
    pub fn interrupt(&mut self) -> bool {
        if !self.state.is_live() {
            return false;
        }
        self.state = ExecutionState::Cancelled { interrupted: true };
        true
    }

    /// Explicitly replaces the destination session; input addressed to the old association becomes stale.
    pub fn replace_session(
        &mut self,
        session_id: SessionId,
        model: ModelRef,
    ) -> Result<Revision, DomainError> {
        if self.state.is_live() {
            return Err(DomainError::InvalidLifecycle(
                "stop execution before replacing the session",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{model, session_id};

    fn started() -> (Execution, ExecutionGeneration) {
        let mut execution = Execution::new();
        let generation = execution.start(session_id(1), model()).expect("start");
        (execution, generation)
    }

    #[test]
    fn start_binds_session_and_first_generation() {
        let (execution, generation) = started();
        assert_eq!(generation, ExecutionGeneration::new(1));
        assert_eq!(execution.state(), &ExecutionState::Starting);
        let session = execution.session().expect("bound");
        assert_eq!(session.session_id, session_id(1));
        assert_eq!(session.association_revision, Revision::INITIAL);
    }

    #[test]
    fn second_start_is_rejected() {
        let (mut execution, _) = started();
        assert!(matches!(
            execution.start(session_id(2), model()),
            Err(DomainError::InvalidLifecycle(_))
        ));
    }

    #[test]
    fn full_turn_cycle_and_cancellation_is_reversible() {
        let (mut execution, generation) = started();
        execution.worker_ready(generation).expect("ready");
        execution.observe_turn(generation, true).expect("running");
        assert_eq!(execution.state(), &ExecutionState::Running);
        execution.observe_turn(generation, false).expect("idle");
        assert_eq!(execution.state(), &ExecutionState::Idle);
        execution.request_stop().expect("stop");
        execution
            .observe_turn(generation, true)
            .expect("ignored while stopping");
        assert_eq!(execution.state(), &ExecutionState::Stopping);
        execution.stopped(generation).expect("stopped");
        assert_eq!(
            execution.state(),
            &ExecutionState::Cancelled { interrupted: false }
        );
        let resumed = execution.resume().expect("resume");
        assert_eq!(resumed, ExecutionGeneration::new(2));
        assert_eq!(execution.state(), &ExecutionState::Starting);
        assert_eq!(
            execution.session().map(|s| s.session_id),
            Some(session_id(1))
        );
    }

    #[test]
    fn stale_generation_facts_are_rejected() {
        let (mut execution, generation) = started();
        execution.fail(generation, "boom").expect("fail");
        let next = execution.resume().expect("resume");
        assert_eq!(
            execution.worker_ready(generation),
            Err(DomainError::StaleExecution)
        );
        assert_eq!(
            execution.observe_turn(generation, true),
            Err(DomainError::StaleExecution)
        );
        assert_eq!(
            execution.fail(generation, "late"),
            Err(DomainError::StaleExecution)
        );
        execution.worker_ready(next).expect("current generation");
        assert_eq!(
            Execution::new().check_generation(ExecutionGeneration::NONE),
            Err(DomainError::StaleExecution)
        );
    }

    #[test]
    fn invalid_execution_transitions_are_rejected() {
        let mut never = Execution::new();
        assert!(never.resume().is_err());
        assert!(never.request_stop().is_err());
        let (mut execution, generation) = started();
        assert!(execution.resume().is_err(), "live execution cannot resume");
        assert!(
            execution.observe_turn(generation, true).is_err(),
            "turn before ready"
        );
        assert!(
            execution.stopped(generation).is_err(),
            "stopped without request"
        );
        execution.worker_ready(generation).expect("ready");
        assert!(execution.worker_ready(generation).is_err(), "ready twice");
        execution.request_stop().expect("stop");
        assert!(execution.request_stop().is_err(), "already stopping");
        execution.stopped(generation).expect("stopped");
        assert!(execution.fail(generation, "x").is_err(), "not live");
    }

    #[test]
    fn restart_interrupts_only_live_executions() {
        for prepare in [0, 1, 2, 3] {
            let (mut execution, generation) = started();
            if prepare >= 1 {
                execution.worker_ready(generation).expect("ready");
            }
            if prepare >= 2 {
                execution.observe_turn(generation, true).expect("running");
            }
            if prepare >= 3 {
                execution.request_stop().expect("stopping");
            }
            assert!(execution.interrupt());
            assert_eq!(
                execution.state(),
                &ExecutionState::Cancelled { interrupted: true }
            );
            assert!(!execution.interrupt(), "idempotent");
        }
        assert!(!Execution::new().interrupt());
    }

    #[test]
    fn failure_reasons_are_clipped_on_char_boundaries() {
        let (mut execution, generation) = started();
        let long = "é".repeat(400);
        execution.fail(generation, &long).expect("fail");
        let ExecutionState::Failed { reason } = execution.state() else {
            panic!("failed")
        };
        assert!(reason.len() <= MAX_FAILURE_REASON_BYTES);
        assert!(reason.chars().all(|c| c == 'é'));
    }

    #[test]
    fn replacing_session_advances_association_revision() {
        let (mut execution, generation) = started();
        assert!(
            execution.replace_session(session_id(2), model()).is_err(),
            "live"
        );
        execution.request_stop().expect("stop");
        execution.stopped(generation).expect("stopped");
        assert!(
            execution.replace_session(session_id(1), model()).is_err(),
            "same session"
        );
        assert_eq!(
            execution.replace_session(session_id(2), model()),
            Ok(Revision::new(2))
        );
        assert_eq!(
            execution.check_association(session_id(1), Revision::new(1)),
            Err(DomainError::StaleAssociation)
        );
        assert_eq!(
            execution.check_association(session_id(2), Revision::new(1)),
            Err(DomainError::StaleAssociation)
        );
        assert_eq!(
            execution.check_association(session_id(2), Revision::new(2)),
            Ok(())
        );
        assert_eq!(
            Execution::new().check_association(session_id(2), Revision::new(2)),
            Err(DomainError::ExecutionUnavailable)
        );
        assert_eq!(
            Execution::new().replace_session(session_id(2), model()),
            Err(DomainError::ExecutionUnavailable)
        );
    }

    #[test]
    fn restore_rejects_impossible_states() {
        assert!(Execution::restore(ExecutionState::None, ExecutionGeneration::NONE, None).is_ok());
        assert!(Execution::restore(ExecutionState::Idle, ExecutionGeneration::NONE, None).is_err());
        assert!(
            Execution::restore(ExecutionState::Idle, ExecutionGeneration::new(1), None).is_err()
        );
        let (execution, _) = started();
        let session = execution.session().cloned();
        assert!(
            Execution::restore(
                ExecutionState::Idle,
                ExecutionGeneration::new(1),
                session.clone()
            )
            .is_ok()
        );
        assert!(
            Execution::restore(ExecutionState::None, ExecutionGeneration::new(1), session).is_err()
        );
    }

    #[test]
    fn model_labels_are_validated() {
        assert!(ModelRef::new("lade-fixture", "scripted-1").is_ok());
        assert!(ModelRef::new("", "x").is_err());
        assert!(ModelRef::new("a b", "x").is_err());
        assert!(ModelRef::new("a", &"x".repeat(MAX_LABEL_BYTES + 1)).is_err());
    }
}
