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
    assert!(Execution::restore(ExecutionState::Idle, ExecutionGeneration::new(1), None).is_err());
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
fn live_rebind_needs_an_idle_current_generation_and_makes_the_old_association_stale() {
    let (mut execution, generation) = started();
    let original = execution.session().cloned().expect("bound");
    assert!(matches!(
        execution.rebind_live_session(generation, session_id(2)),
        Err(DomainError::InvalidLifecycle(_))
    ));
    execution.worker_ready(generation).expect("ready");
    assert!(matches!(
        execution.rebind_live_session(ExecutionGeneration::new(9), session_id(2)),
        Err(DomainError::StaleExecution)
    ));
    assert!(matches!(
        execution.rebind_live_session(generation, original.session_id),
        Err(DomainError::InvalidRequest(_))
    ));
    let revision = execution
        .rebind_live_session(generation, session_id(2))
        .expect("rebind");
    assert_eq!(revision, original.association_revision.advance().unwrap());
    assert_eq!(
        execution.check_association(original.session_id, original.association_revision),
        Err(DomainError::StaleAssociation)
    );
    assert!(execution.check_association(session_id(2), revision).is_ok());
    assert_eq!(execution.session().unwrap().model, original.model);
    execution.observe_turn(generation, true).expect("running");
    assert!(matches!(
        execution.rebind_live_session(generation, session_id(3)),
        Err(DomainError::InvalidLifecycle(_))
    ));
}

#[test]
fn model_selection_keeps_the_destination() {
    let mut execution = Execution::new();
    assert_eq!(
        execution.select_model(model()),
        Err(DomainError::ExecutionUnavailable)
    );
    let (mut execution, _) = started();
    let before = execution.session().cloned().unwrap();
    let other = ModelRef::new("anthropic", "some-model").unwrap();
    execution.select_model(other.clone()).expect("select");
    let after = execution.session().unwrap();
    assert_eq!(after.model, other);
    assert_eq!(after.session_id, before.session_id);
    assert_eq!(after.association_revision, before.association_revision);
}
