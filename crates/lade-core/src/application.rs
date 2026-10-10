//! Small use cases over the ports. The service may equally call domain functions inside its own transactions;
//! these exist for the Objective-only flows and for restart recovery, which spans several aggregates.

use crate::domain::execution::{ExecutionGeneration, ModelRef};
use crate::domain::feedback::AcceptedInput;
use crate::domain::ids::{GitId, ObjectiveId, Revision, SessionId, Timestamp, WorkspaceId};
use crate::domain::objectives::{BranchName, Intent, NewObjective, Objective, Title};
use crate::domain::review::Review;
use crate::error::DomainError;
use crate::ports::{Clock, IdGenerator, ObjectiveStore};

#[derive(Clone, Debug)]
pub struct CreateObjective {
    pub workspace_id: WorkspaceId,
    pub title: Title,
    pub intent: Intent,
    pub integration_target: BranchName,
    /// Resolved by the Git adapter (target tip or an explicit commit).
    pub start_commit: GitId,
}

/// Objective use cases bound to their ports by explicit construction.
#[derive(Debug)]
pub struct Objectives<S, C, G> {
    pub store: S,
    pub clock: C,
    pub ids: G,
}

impl<S: ObjectiveStore, C: Clock, G: IdGenerator> Objectives<S, C, G> {
    pub const fn new(store: S, clock: C, ids: G) -> Self {
        Self { store, clock, ids }
    }

    pub fn create(&mut self, request: CreateObjective) -> Result<Objective, DomainError> {
        let new = NewObjective {
            id: ObjectiveId::new(self.ids.uuid()),
            workspace_id: request.workspace_id,
            title: request.title,
            intent: request.intent,
            integration_target: request.integration_target,
            start_commit: request.start_commit,
        };
        let objective = Objective::define(new, self.clock.now());
        self.store.insert(&objective)?;
        Ok(objective)
    }

    /// Load, apply a domain change, and save with the loaded revision as the optimistic expectation.
    pub fn mutate<T>(
        &mut self,
        id: ObjectiveId,
        change: impl FnOnce(&mut Objective, Timestamp) -> Result<T, DomainError>,
    ) -> Result<(Objective, T), DomainError> {
        let mut objective = self.store.load(id)?;
        let expected = objective.revision();
        let output = change(&mut objective, self.clock.now())?;
        self.store.update(&objective, expected)?;
        Ok((objective, output))
    }

    pub fn update_intent(
        &mut self,
        id: ObjectiveId,
        expected_intent_revision: Revision,
        intent: Intent,
    ) -> Result<Objective, DomainError> {
        self.mutate(id, |objective, now| {
            objective.update_intent(expected_intent_revision, intent, now)
        })
        .map(|(objective, _)| objective)
    }

    /// First start: generates the LADE session identity and binds it as the destination.
    pub fn start_execution(
        &mut self,
        id: ObjectiveId,
        model: ModelRef,
    ) -> Result<(Objective, ExecutionGeneration), DomainError> {
        let session_id = SessionId::new(self.ids.uuid());
        self.mutate(id, |objective, now| {
            objective.start_execution(session_id, model, now)
        })
    }

    pub fn resume_execution(
        &mut self,
        id: ObjectiveId,
    ) -> Result<(Objective, ExecutionGeneration), DomainError> {
        self.mutate(id, Objective::resume_execution)
    }

    pub fn request_stop(&mut self, id: ObjectiveId) -> Result<Objective, DomainError> {
        self.mutate(id, Objective::request_stop)
            .map(|(objective, ())| objective)
    }
}

/// What restart recovery changed for one Objective.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RestartRecovery {
    pub execution_interrupted: bool,
    pub inputs_uncertain: usize,
}

/// Service restart for one Objective: live execution → cancelled/interrupted, unconsumed approval expires,
/// running verifications are interrupted, dispatched input becomes uncertain. Nothing resumes or redispatches.
pub fn recover_after_restart(
    objective: &mut Objective,
    review: &mut Review,
    inputs: &mut [AcceptedInput],
    now: Timestamp,
) -> Result<RestartRecovery, DomainError> {
    let execution_interrupted = objective.interrupt_after_restart(now)?;
    review.recover_after_restart(objective, now)?;
    let inputs_uncertain = inputs
        .iter_mut()
        .filter(|input| input.objective_id == objective.id())
        .map(AcceptedInput::mark_worker_lost)
        .filter(|changed| *changed)
        .count();
    Ok(RestartRecovery {
        execution_interrupted,
        inputs_uncertain,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::execution::ExecutionState;
    use crate::domain::feedback::{Acceptance, DeliveryState, InputMode, accept_message};
    use crate::domain::ids::Uuid;
    use crate::domain::objectives::Lifecycle;
    use crate::testing::{
        active_objective, git, input_id, model, operation_id, ts, uuid, workspace_id,
    };

    #[derive(Default)]
    struct MemoryStore {
        objectives: Vec<Objective>,
    }

    impl ObjectiveStore for MemoryStore {
        fn load(&self, id: ObjectiveId) -> Result<Objective, DomainError> {
            self.objectives
                .iter()
                .find(|o| o.id() == id)
                .cloned()
                .ok_or(DomainError::NotFound("objective"))
        }

        fn insert(&mut self, objective: &Objective) -> Result<(), DomainError> {
            if self.objectives.iter().any(|o| o.id() == objective.id()) {
                return Err(DomainError::Conflict("objective exists"));
            }
            self.objectives.push(objective.clone());
            Ok(())
        }

        fn update(&mut self, objective: &Objective, expected: Revision) -> Result<(), DomainError> {
            let slot = self
                .objectives
                .iter_mut()
                .find(|o| o.id() == objective.id())
                .ok_or(DomainError::NotFound("objective"))?;
            if slot.revision() != expected {
                return Err(DomainError::Conflict("objective revision mismatch"));
            }
            *slot = objective.clone();
            Ok(())
        }
    }

    struct FixedClock(i64);

    impl Clock for FixedClock {
        fn now(&self) -> Timestamp {
            ts(self.0)
        }
    }

    struct Sequence(u128);

    impl IdGenerator for Sequence {
        fn uuid(&mut self) -> Uuid {
            self.0 += 1;
            uuid(self.0)
        }
    }

    fn service() -> Objectives<MemoryStore, FixedClock, Sequence> {
        Objectives::new(MemoryStore::default(), FixedClock(100), Sequence(0))
    }

    fn create(service: &mut Objectives<MemoryStore, FixedClock, Sequence>) -> Objective {
        service
            .create(CreateObjective {
                workspace_id: workspace_id(1),
                title: Title::new("Add search").expect("title"),
                intent: Intent::new("Search works", "", "").expect("intent"),
                integration_target: BranchName::new("main").expect("branch"),
                start_commit: git(1),
            })
            .expect("create")
    }

    #[test]
    fn create_start_stop_resume_round_trip_through_store() {
        let mut service = service();
        let objective = create(&mut service);
        assert_eq!(objective.record().created_at, ts(100));
        let (started, generation) = service
            .start_execution(objective.id(), model())
            .expect("start");
        assert_eq!(started.lifecycle(), Lifecycle::Active);
        assert_eq!(
            started.execution().session().map(|s| s.session_id.uuid()),
            Some(uuid(2))
        );
        assert_eq!(service.store.load(objective.id()), Ok(started));
        service.request_stop(objective.id()).expect("stop");
        service
            .mutate(objective.id(), |o, now| o.observe_stopped(generation, now))
            .expect("stopped");
        let (resumed, next) = service.resume_execution(objective.id()).expect("resume");
        assert_eq!(next.value(), 2);
        assert_eq!(resumed.execution().state(), &ExecutionState::Starting);
    }

    #[test]
    fn rejected_changes_are_not_saved_and_concurrent_writes_conflict() {
        let mut service = service();
        let objective = create(&mut service);
        let intent = Intent::new("changed", "", "").expect("intent");
        assert!(
            service
                .update_intent(objective.id(), Revision::new(5), intent.clone())
                .is_err()
        );
        assert_eq!(service.store.load(objective.id()), Ok(objective.clone()));
        let concurrent = service
            .update_intent(objective.id(), Revision::INITIAL, intent.clone())
            .expect("update");
        // A writer holding the older revision is rejected by the store.
        let mut stale = objective.clone();
        stale
            .update_intent(Revision::INITIAL, intent, ts(101))
            .expect("domain ok");
        assert_eq!(
            service.store.update(&stale, objective.revision()),
            Err(DomainError::Conflict("objective revision mismatch"))
        );
        assert_eq!(service.store.load(objective.id()), Ok(concurrent));
        assert_eq!(
            service
                .resume_execution(crate::testing::objective_id(42))
                .map(|_| ()),
            Err(DomainError::NotFound("objective"))
        );
    }

    #[test]
    fn restart_recovery_interrupts_and_marks_dispatched_input_uncertain() {
        let (mut objective, _) = active_objective();
        let session = objective.execution().session().expect("session").clone();
        let acceptance = Acceptance {
            input_id: input_id(1),
            operation_id: operation_id(1),
            session_id: session.session_id,
            association_revision: session.association_revision,
            mode: InputMode::Prompt,
        };
        let mut dispatched =
            accept_message(&objective, acceptance, "go".into(), ts(5)).expect("accept");
        dispatched.dispatch(&objective, false).expect("dispatch");
        let queued = accept_message(
            &objective,
            Acceptance {
                input_id: input_id(2),
                ..acceptance
            },
            "next".into(),
            ts(5),
        )
        .expect("accept");
        let mut inputs = [dispatched, queued];
        let mut review = Review::new(objective.id());
        let recovery = recover_after_restart(&mut objective, &mut review, &mut inputs, ts(9))
            .expect("recover");
        assert_eq!(
            recovery,
            RestartRecovery {
                execution_interrupted: true,
                inputs_uncertain: 1
            }
        );
        assert!(matches!(
            inputs[0].delivery,
            DeliveryState::Uncertain { .. }
        ));
        assert_eq!(inputs[1].delivery, DeliveryState::Queued);
        assert_eq!(
            objective.execution().state(),
            &ExecutionState::Cancelled { interrupted: true }
        );
    }
}
