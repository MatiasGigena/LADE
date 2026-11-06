use super::*;

use crate::testing::{active_objective, defined_objective, model, session_id, ts};

#[test]
fn define_derives_branch_and_initial_revisions() {
    let objective = defined_objective();
    let record = objective.record();
    assert_eq!(record.lifecycle, Lifecycle::Defined);
    assert_eq!(
        record.branch.as_str(),
        format!("lade/{}", record.id.uuid().short())
    );
    assert_eq!(record.revision, Revision::INITIAL);
    assert_eq!(record.intent_revision, Revision::INITIAL);
    assert_eq!(Objective::restore(record.clone()).as_ref(), Ok(&objective));
}

#[test]
fn first_execution_activates_and_frozen_cannot_resume() {
    let mut objective = defined_objective();
    let generation = objective
        .start_execution(session_id(1), model(), ts(2))
        .expect("start");
    assert_eq!(generation.value(), 1);
    assert_eq!(objective.lifecycle(), Lifecycle::Active);
    assert_eq!(objective.revision(), Revision::new(2));
    assert!(
        objective
            .start_execution(session_id(1), model(), ts(3))
            .is_err()
    );
    objective.request_stop(ts(3)).expect("stop");
    objective
        .observe_stopped(generation, ts(4))
        .expect("stopped");
    objective
        .transition(&[Lifecycle::Frozen], ts(5))
        .expect("freeze");
    let before = objective.clone();
    assert!(matches!(
        objective.resume_execution(ts(6)),
        Err(DomainError::InvalidLifecycle(_))
    ));
    assert_eq!(objective, before, "rejected calls do not mutate");
}

#[test]
fn stopped_execution_resumes_with_the_next_generation_and_session() {
    let mut objective = defined_objective();
    let first = objective
        .start_execution(session_id(1), model(), ts(2))
        .expect("start");
    objective.request_stop(ts(3)).expect("stop");
    objective.observe_stopped(first, ts(4)).expect("stopped");
    let next = objective.resume_execution(ts(5)).expect("resume");
    assert_eq!(next.value(), first.value() + 1);
    assert_eq!(objective.execution().state(), &ExecutionState::Starting);
    assert_eq!(
        objective.execution().session().map(|s| s.session_id),
        Some(session_id(1))
    );
}

#[test]
fn freezing_or_closing_requires_quiescent_execution() {
    let (mut objective, _) = active_objective();
    for path in [
        &[Lifecycle::Frozen][..],
        &[Lifecycle::Discarded],
        &[Lifecycle::CompletedNoChanges],
    ] {
        assert!(matches!(
            objective.transition(path, ts(9)),
            Err(DomainError::InvalidLifecycle(_))
        ));
    }
    assert_eq!(objective.lifecycle(), Lifecycle::Active);
}

#[test]
fn intent_updates_use_optimistic_revision() {
    let mut objective = defined_objective();
    let intent = Intent::new("new outcome", "", "").expect("intent");
    assert_eq!(
        objective.update_intent(Revision::new(2), intent.clone(), ts(2)),
        Err(DomainError::Conflict(INTENT_CHANGED))
    );
    assert_eq!(
        objective.update_intent(Revision::INITIAL, intent.clone(), ts(2)),
        Ok(Revision::new(2))
    );
    assert_eq!(objective.record().intent.outcome(), "new outcome");
    assert!(
        objective
            .update_intent(Revision::INITIAL, intent, ts(3))
            .is_err(),
        "stale expected revision"
    );
}

#[test]
fn intent_is_frozen_after_approval_and_close() {
    let mut objective = defined_objective();
    objective
        .transition(&[Lifecycle::Discarded], ts(2))
        .expect("discard");
    let intent = Intent::new("x", "", "").expect("intent");
    assert!(matches!(
        objective.update_intent(Revision::INITIAL, intent, ts(3)),
        Err(DomainError::InvalidLifecycle(_))
    ));
}

#[test]
fn revision_exhaustion_rejects_without_mutation() {
    let mut record = defined_objective().into_record();
    record.revision = Revision::new(u64::MAX);
    let mut objective = Objective::restore(record).expect("restore");
    let before = objective.clone();
    assert!(matches!(
        objective.start_execution(session_id(1), model(), ts(2)),
        Err(DomainError::LimitReached(_))
    ));
    assert_eq!(objective, before);
}

#[test]
fn cleanup_only_after_close_and_with_confirmation() {
    let mut objective = defined_objective();
    assert!(objective.cleanup(0, false, ts(2)).is_err(), "not terminal");
    objective
        .transition(&[Lifecycle::Discarded], ts(2))
        .expect("discard");
    assert_eq!(
        objective.cleanup(1, false, ts(3)),
        Err(DomainError::Conflict(UNSENT_DRAFTS_NEED_CONFIRMATION))
    );
    objective.cleanup(1, true, ts(3)).expect("confirmed");
    assert!(objective.record().cleaned_up);
    assert!(objective.cleanup(0, false, ts(4)).is_err(), "twice");
}

#[test]
fn restart_interrupts_without_resuming() {
    let (mut objective, _) = active_objective();
    assert_eq!(objective.interrupt_after_restart(ts(10)), Ok(true));
    assert_eq!(
        objective.execution().state(),
        &ExecutionState::Cancelled { interrupted: true }
    );
    let revision = objective.revision();
    assert_eq!(objective.interrupt_after_restart(ts(11)), Ok(false));
    assert_eq!(objective.revision(), revision, "no-op does not bump");
    let view = derive_attention(&objective, &AttentionFacts::default());
    assert_eq!(view.attention, Attention::NeedsAttention);
    assert_eq!(view.badges, vec![Badge::Interrupted]);
}

#[test]
fn restore_rejects_inconsistent_records() {
    let mut record = defined_objective().into_record();
    record.cleaned_up = true;
    assert!(Objective::restore(record).is_err());
    let mut record = defined_objective().into_record();
    record.branch = BranchName::new("lade/other").expect("branch");
    assert!(Objective::restore(record).is_err());
    let (objective, _) = active_objective();
    let mut record = objective.into_record();
    record.lifecycle = Lifecycle::Frozen;
    assert!(
        Objective::restore(record).is_err(),
        "frozen with live execution"
    );
}
