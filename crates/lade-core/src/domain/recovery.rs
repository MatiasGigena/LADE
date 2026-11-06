//! What a service restart does to an Objective: an execution that was live is interrupted
//! (never resumed automatically), review state is reconciled, and input that may have reached
//! the lost worker becomes uncertain.

use crate::domain::feedback::AcceptedInput;
use crate::domain::ids::Timestamp;
use crate::domain::objectives::Objective;
use crate::domain::review::Review;
use crate::error::DomainError;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RestartRecovery {
    pub execution_interrupted: bool,
    pub inputs_uncertain: usize,
}

/// Applies a service restart to one Objective, its review and its accepted inputs.
///
/// # Errors
///
/// Fails when the Objective's or the review's restart transition does, for a review of another
/// Objective or an exhausted revision.
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
    use crate::testing::{active_objective, input_id, operation_id, ts};

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
