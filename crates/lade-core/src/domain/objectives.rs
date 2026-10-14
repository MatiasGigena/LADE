use crate::domain::execution::{Execution, ExecutionGeneration, ExecutionState, ModelRef};
use crate::domain::ids::{GitId, ObjectiveId, Revision, SessionId, Timestamp, WorkspaceId};
use crate::domain::inspection::MAX_TEXT_BYTES;
use crate::error::DomainError;

pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_BRANCH_BYTES: usize = 255;

pub const CLOSED: &str = "This Objective is closed.";
pub const INTENT_CHANGED: &str = "The intent was changed elsewhere. Refresh and try again.";
pub const UNSENT_DRAFTS_NEED_CONFIRMATION: &str =
    "This Objective has unsent drafts. Confirm that they may be deleted.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Title(String);

impl Title {
    pub fn new(text: &str) -> Result<Self, DomainError> {
        let trimmed = text.trim();
        let count = trimmed.chars().count();
        if count == 0 || count > MAX_TITLE_CHARS || trimmed.chars().any(char::is_control) {
            return Err(DomainError::InvalidRequest(
                "The title must be 1 to 200 characters on one line.",
            ));
        }
        Ok(Self(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BranchName(String);

impl BranchName {
    pub fn new(name: &str) -> Result<Self, DomainError> {
        let forbidden_char = |c: char| {
            c.is_control()
                || c.is_whitespace()
                || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        };
        let valid = !name.is_empty()
            && name.len() <= MAX_BRANCH_BYTES
            && name != "@"
            && !name.starts_with('-')
            && !name.ends_with('.')
            && !name.ends_with(".lock")
            && !name.contains("..")
            && !name.contains("@{")
            && !name.chars().any(forbidden_char)
            && name
                .split('/')
                .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"));
        if !valid {
            return Err(DomainError::InvalidRequest(
                "That isn't a valid branch name.",
            ));
        }
        Ok(Self(name.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn for_objective(id: ObjectiveId) -> Self {
        Self(format!("lade/{}", id.uuid().short()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Intent {
    outcome: String,
    constraints: String,
    acceptance: String,
}

impl Intent {
    pub fn new(outcome: &str, constraints: &str, acceptance: &str) -> Result<Self, DomainError> {
        if outcome.trim().is_empty() {
            return Err(DomainError::InvalidRequest(
                "Describe the outcome you want.",
            ));
        }
        if [outcome, constraints, acceptance]
            .iter()
            .any(|field| field.len() > MAX_TEXT_BYTES)
        {
            return Err(DomainError::LimitReached("An intent field is too long."));
        }
        Ok(Self {
            outcome: outcome.to_string(),
            constraints: constraints.to_string(),
            acceptance: acceptance.to_string(),
        })
    }

    pub fn outcome(&self) -> &str {
        &self.outcome
    }

    pub fn constraints(&self) -> &str {
        &self.constraints
    }

    pub fn acceptance(&self) -> &str {
        &self.acceptance
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lifecycle {
    Defined,
    Active,
    Frozen,
    Approved,
    Integrated,
    CompletedNoChanges,
    Discarded,
}

impl Lifecycle {
    pub const ALL: [Self; 7] = [
        Self::Defined,
        Self::Active,
        Self::Frozen,
        Self::Approved,
        Self::Integrated,
        Self::CompletedNoChanges,
        Self::Discarded,
    ];

    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Integrated | Self::CompletedNoChanges | Self::Discarded
        )
    }

    pub const fn permits(self, to: Self) -> bool {
        use Lifecycle::{
            Active, Approved, CompletedNoChanges, Defined, Discarded, Frozen, Integrated,
        };
        matches!(
            (self, to),
            (Defined, Active)
                | (Active, Frozen)
                | (Frozen, Active)
                | (Frozen, Approved)
                | (Approved, Frozen)
                | (Approved, Integrated)
                | (Defined | Active | Frozen, CompletedNoChanges)
                | (Defined | Active | Frozen | Approved, Discarded)
        )
    }

    pub const fn code(self) -> &'static str {
        match self {
            Self::Defined => "defined",
            Self::Active => "active",
            Self::Frozen => "frozen",
            Self::Approved => "approved",
            Self::Integrated => "integrated",
            Self::CompletedNoChanges => "completed_no_changes",
            Self::Discarded => "discarded",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectiveRecord {
    pub id: ObjectiveId,
    pub workspace_id: WorkspaceId,
    pub title: Title,
    pub intent: Intent,
    pub intent_revision: Revision,
    pub integration_target: BranchName,
    pub start_commit: GitId,
    pub branch: BranchName,
    pub lifecycle: Lifecycle,
    pub execution: Execution,
    pub cleaned_up: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub revision: Revision,
}

#[derive(Clone, Debug)]
pub struct NewObjective {
    pub id: ObjectiveId,
    pub workspace_id: WorkspaceId,
    pub title: Title,
    pub intent: Intent,
    pub integration_target: BranchName,
    pub start_commit: GitId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Objective {
    record: ObjectiveRecord,
}

impl Objective {
    pub fn define(new: NewObjective, now: Timestamp) -> Self {
        Self {
            record: ObjectiveRecord {
                branch: BranchName::for_objective(new.id),
                id: new.id,
                workspace_id: new.workspace_id,
                title: new.title,
                intent: new.intent,
                intent_revision: Revision::INITIAL,
                integration_target: new.integration_target,
                start_commit: new.start_commit,
                lifecycle: Lifecycle::Defined,
                execution: Execution::new(),
                cleaned_up: false,
                created_at: now,
                updated_at: now,
                revision: Revision::INITIAL,
            },
        }
    }

    pub fn restore(record: ObjectiveRecord) -> Result<Self, DomainError> {
        let lifecycle = record.lifecycle;
        let state = record.execution.state();
        let valid = record.branch == BranchName::for_objective(record.id)
            && (!record.cleaned_up || lifecycle.is_terminal())
            && (lifecycle != Lifecycle::Defined || *state == ExecutionState::None)
            && (lifecycle == Lifecycle::Defined
                || lifecycle == Lifecycle::Active
                || !state.is_live())
            && record.revision.value() >= 1
            && record.intent_revision.value() >= 1;
        if !valid {
            return Err(DomainError::DurabilityFailure(
                "inconsistent stored objective",
            ));
        }
        Ok(Self { record })
    }

    pub const fn record(&self) -> &ObjectiveRecord {
        &self.record
    }

    pub fn into_record(self) -> ObjectiveRecord {
        self.record
    }

    pub const fn id(&self) -> ObjectiveId {
        self.record.id
    }

    pub const fn lifecycle(&self) -> Lifecycle {
        self.record.lifecycle
    }

    pub const fn execution(&self) -> &Execution {
        &self.record.execution
    }

    pub const fn revision(&self) -> Revision {
        self.record.revision
    }

    fn update<T>(
        &mut self,
        now: Timestamp,
        change: impl FnOnce(&mut ObjectiveRecord) -> Result<T, DomainError>,
    ) -> Result<T, DomainError> {
        let mut next = self.record.clone();
        let output = change(&mut next)?;
        next.revision = self.record.revision.advance()?;
        next.updated_at = now;
        self.record = next;
        Ok(output)
    }

    pub(crate) fn transition(
        &mut self,
        path: &[Lifecycle],
        now: Timestamp,
    ) -> Result<(), DomainError> {
        self.update(now, |record| {
            for &to in path {
                if !record.lifecycle.permits(to) {
                    return Err(DomainError::InvalidLifecycle(
                        "That isn't possible in the Objective's current state.",
                    ));
                }
                let excludes_writers = to != Lifecycle::Active;
                if excludes_writers && record.execution.state().is_live() {
                    return Err(DomainError::InvalidLifecycle("Stop the agent first."));
                }
                record.lifecycle = to;
            }
            Ok(())
        })
    }

    pub fn update_intent(
        &mut self,
        expected_intent_revision: Revision,
        intent: Intent,
        now: Timestamp,
    ) -> Result<Revision, DomainError> {
        self.update(now, |record| {
            if !matches!(
                record.lifecycle,
                Lifecycle::Defined | Lifecycle::Active | Lifecycle::Frozen
            ) {
                return Err(DomainError::InvalidLifecycle(
                    "The intent can't change after approval. Return the candidate for revision first.",
                ));
            }
            if record.intent_revision != expected_intent_revision {
                return Err(DomainError::Conflict(INTENT_CHANGED));
            }
            record.intent_revision = record.intent_revision.advance()?;
            record.intent = intent;
            Ok(record.intent_revision)
        })
    }

    pub fn start_execution(
        &mut self,
        session_id: SessionId,
        model: ModelRef,
        now: Timestamp,
    ) -> Result<ExecutionGeneration, DomainError> {
        self.update(now, |record| {
            if !matches!(record.lifecycle, Lifecycle::Defined | Lifecycle::Active) {
                return Err(DomainError::InvalidLifecycle(
                    "The agent can start only before review. Return the candidate for revision first.",
                ));
            }
            let generation = record.execution.start(session_id, model)?;
            record.lifecycle = Lifecycle::Active;
            Ok(generation)
        })
    }

    pub fn resume_execution(&mut self, now: Timestamp) -> Result<ExecutionGeneration, DomainError> {
        self.update(now, |record| {
            if record.lifecycle != Lifecycle::Active {
                return Err(DomainError::InvalidLifecycle(
                    "The agent can be resumed only while the Objective is in progress.",
                ));
            }
            record.execution.resume()
        })
    }

    pub fn request_stop(&mut self, now: Timestamp) -> Result<(), DomainError> {
        self.update(now, |record| record.execution.request_stop())
    }

    pub fn observe_worker_ready(
        &mut self,
        generation: ExecutionGeneration,
        now: Timestamp,
    ) -> Result<(), DomainError> {
        self.update(now, |record| record.execution.worker_ready(generation))
    }

    pub fn observe_turn(
        &mut self,
        generation: ExecutionGeneration,
        running: bool,
        now: Timestamp,
    ) -> Result<(), DomainError> {
        self.update(now, |record| {
            record.execution.observe_turn(generation, running)
        })
    }

    pub fn observe_stopped(
        &mut self,
        generation: ExecutionGeneration,
        now: Timestamp,
    ) -> Result<(), DomainError> {
        self.update(now, |record| record.execution.stopped(generation))
    }

    pub fn observe_failure(
        &mut self,
        generation: ExecutionGeneration,
        reason: &str,
        now: Timestamp,
    ) -> Result<(), DomainError> {
        self.update(now, |record| record.execution.fail(generation, reason))
    }

    pub fn interrupt_after_restart(&mut self, now: Timestamp) -> Result<bool, DomainError> {
        if !self.record.execution.state().is_live() {
            return Ok(false);
        }
        self.update(now, |record| Ok(record.execution.interrupt()))
    }

    pub fn rebind_live_session(
        &mut self,
        generation: ExecutionGeneration,
        session_id: SessionId,
        now: Timestamp,
    ) -> Result<Revision, DomainError> {
        self.update(now, |record| {
            if record.lifecycle != Lifecycle::Active {
                return Err(DomainError::InvalidLifecycle(
                    "The session can be replaced only while the Objective is in progress.",
                ));
            }
            record.execution.rebind_live_session(generation, session_id)
        })
    }

    pub fn select_model(&mut self, model: ModelRef, now: Timestamp) -> Result<(), DomainError> {
        self.update(now, |record| {
            if record.lifecycle.is_terminal() {
                return Err(DomainError::InvalidLifecycle(CLOSED));
            }
            record.execution.select_model(model)
        })
    }

    pub fn replace_session(
        &mut self,
        session_id: SessionId,
        model: ModelRef,
        now: Timestamp,
    ) -> Result<Revision, DomainError> {
        self.update(now, |record| {
            if record.lifecycle.is_terminal() {
                return Err(DomainError::InvalidLifecycle(CLOSED));
            }
            record.execution.replace_session(session_id, model)
        })
    }

    pub fn cleanup(
        &mut self,
        unsent_drafts: usize,
        confirm_data_loss: bool,
        now: Timestamp,
    ) -> Result<(), DomainError> {
        self.update(now, |record| {
            if !record.lifecycle.is_terminal() {
                return Err(DomainError::InvalidLifecycle(
                    "Clean up becomes available once the Objective is integrated, completed or discarded.",
                ));
            }
            if record.cleaned_up {
                return Err(DomainError::InvalidLifecycle(
                    "This Objective is already cleaned up.",
                ));
            }
            if unsent_drafts > 0 && !confirm_data_loss {
                return Err(DomainError::Conflict(UNSENT_DRAFTS_NEED_CONFIRMATION));
            }
            record.cleaned_up = true;
            Ok(())
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Attention {
    NeedsAttention,
    ReadyForReview,
    Working,
    Idle,
    Archived,
}

impl Attention {
    pub const fn code(self) -> &'static str {
        match self {
            Self::NeedsAttention => "needs_attention",
            Self::ReadyForReview => "ready_for_review",
            Self::Working => "working",
            Self::Idle => "idle",
            Self::Archived => "archived",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Badge {
    Interrupted,
    Failed,
    DeliveryUncertain,
    StaleFeedback,
    VerificationFailed,
    IntegrationConflict,
    StaleTarget,
    Cancelled,
    CleanedUp,
}

impl Badge {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Interrupted => "interrupted",
            Self::Failed => "failed",
            Self::DeliveryUncertain => "delivery_uncertain",
            Self::StaleFeedback => "stale_feedback",
            Self::VerificationFailed => "verification_failed",
            Self::IntegrationConflict => "integration_conflict",
            Self::StaleTarget => "stale_target",
            Self::Cancelled => "cancelled",
            Self::CleanedUp => "cleaned_up",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AttentionFacts {
    pub delivery_uncertain: bool,
    pub stale_feedback: bool,
    pub verification_failed: bool,
    pub integration_conflict: bool,
    pub target_moved: bool,
    pub change_set_non_empty: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttentionView {
    pub attention: Attention,
    pub badges: Vec<Badge>,
}

pub fn derive_attention(objective: &Objective, facts: &AttentionFacts) -> AttentionView {
    let record = objective.record();
    if record.lifecycle.is_terminal() {
        let badges = if record.cleaned_up {
            vec![Badge::CleanedUp]
        } else {
            Vec::new()
        };
        return AttentionView {
            attention: Attention::Archived,
            badges,
        };
    }
    let state = record.execution.state();
    let candidates = [
        (
            matches!(state, ExecutionState::Cancelled { interrupted: true }),
            Badge::Interrupted,
        ),
        (
            matches!(state, ExecutionState::Failed { .. }),
            Badge::Failed,
        ),
        (facts.delivery_uncertain, Badge::DeliveryUncertain),
        (facts.stale_feedback, Badge::StaleFeedback),
        (facts.verification_failed, Badge::VerificationFailed),
        (facts.integration_conflict, Badge::IntegrationConflict),
        (facts.target_moved, Badge::StaleTarget),
        (
            matches!(state, ExecutionState::Cancelled { interrupted: false }),
            Badge::Cancelled,
        ),
    ];
    let badges: Vec<Badge> = candidates
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, badge)| *badge)
        .collect();
    let attention = if badges.iter().any(|badge| *badge != Badge::Cancelled) {
        Attention::NeedsAttention
    } else if matches!(record.lifecycle, Lifecycle::Frozen | Lifecycle::Approved)
        || (*state == ExecutionState::Idle && facts.change_set_non_empty)
    {
        Attention::ReadyForReview
    } else if matches!(
        state,
        ExecutionState::Starting | ExecutionState::Running | ExecutionState::Stopping
    ) {
        Attention::Working
    } else {
        Attention::Idle
    };
    AttentionView { attention, badges }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{active_objective, defined_objective, model, session_id, ts};

    #[test]
    fn lifecycle_table_is_exact() {
        use Lifecycle::*;
        let allowed = [
            (Defined, Active),
            (Active, Frozen),
            (Frozen, Active),
            (Frozen, Approved),
            (Approved, Frozen),
            (Approved, Integrated),
            (Defined, CompletedNoChanges),
            (Active, CompletedNoChanges),
            (Frozen, CompletedNoChanges),
            (Defined, Discarded),
            (Active, Discarded),
            (Frozen, Discarded),
            (Approved, Discarded),
        ];
        for from in Lifecycle::ALL {
            for to in Lifecycle::ALL {
                assert_eq!(
                    from.permits(to),
                    allowed.contains(&(from, to)),
                    "{from:?} -> {to:?}"
                );
            }
        }
        for terminal in [Integrated, CompletedNoChanges, Discarded] {
            assert!(terminal.is_terminal());
            assert!(Lifecycle::ALL.iter().all(|to| !terminal.permits(*to)));
        }
    }

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

    #[test]
    fn titles_intents_and_branches_are_validated() {
        assert_eq!(
            Title::new("  Fix login  ").map(|t| t.as_str().to_string()),
            Ok("Fix login".to_string())
        );
        assert!(Title::new("   ").is_err());
        assert!(Title::new(&"é".repeat(200)).is_ok());
        assert!(Title::new(&"é".repeat(201)).is_err());
        assert!(Title::new("a\nb").is_err());
        assert!(Intent::new(" ", "", "").is_err());
        assert!(Intent::new("x", &"y".repeat(MAX_TEXT_BYTES + 1), "").is_err());
        for good in ["main", "feature/x-1", "release/v1.2"] {
            assert!(BranchName::new(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "-x",
            "a..b",
            "a b",
            "a/",
            "/a",
            "a.lock",
            "x/.hidden",
            "a@{1}",
            "@",
            "a:b",
            "a.",
            "a//b",
        ] {
            assert!(BranchName::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn attention_precedence_and_badges() {
        let defined = defined_objective();
        assert_eq!(
            derive_attention(&defined, &AttentionFacts::default()).attention,
            Attention::Idle
        );

        let (mut objective, generation) = active_objective();
        assert_eq!(
            derive_attention(&objective, &AttentionFacts::default()).attention,
            Attention::Idle
        );
        let changed = AttentionFacts {
            change_set_non_empty: true,
            ..AttentionFacts::default()
        };
        assert_eq!(
            derive_attention(&objective, &changed).attention,
            Attention::ReadyForReview
        );
        let uncertain = AttentionFacts {
            delivery_uncertain: true,
            ..changed
        };
        let view = derive_attention(&objective, &uncertain);
        assert_eq!(view.attention, Attention::NeedsAttention);
        assert_eq!(view.badges, vec![Badge::DeliveryUncertain]);

        objective
            .observe_turn(generation, true, ts(5))
            .expect("running");
        assert_eq!(
            derive_attention(&objective, &changed).attention,
            Attention::Working
        );
        objective.request_stop(ts(6)).expect("stop");
        assert_eq!(
            derive_attention(&objective, &changed).attention,
            Attention::Working
        );
        objective
            .observe_stopped(generation, ts(7))
            .expect("stopped");
        let view = derive_attention(&objective, &changed);
        assert_eq!(
            (view.attention, view.badges),
            (Attention::Idle, vec![Badge::Cancelled])
        );

        objective
            .transition(&[Lifecycle::Frozen], ts(8))
            .expect("freeze");
        assert_eq!(
            derive_attention(&objective, &AttentionFacts::default()).attention,
            Attention::ReadyForReview
        );
        let all = AttentionFacts {
            delivery_uncertain: true,
            stale_feedback: true,
            verification_failed: true,
            integration_conflict: true,
            target_moved: true,
            change_set_non_empty: true,
        };
        let view = derive_attention(&objective, &all);
        assert_eq!(view.attention, Attention::NeedsAttention);
        assert_eq!(
            view.badges,
            vec![
                Badge::DeliveryUncertain,
                Badge::StaleFeedback,
                Badge::VerificationFailed,
                Badge::IntegrationConflict,
                Badge::StaleTarget,
                Badge::Cancelled
            ]
        );

        objective
            .transition(&[Lifecycle::Discarded], ts(9))
            .expect("discard");
        assert_eq!(
            derive_attention(&objective, &all),
            AttentionView {
                attention: Attention::Archived,
                badges: vec![]
            }
        );
    }

    #[test]
    fn failed_execution_needs_attention() {
        let (mut objective, generation) = active_objective();
        objective
            .observe_failure(generation, "worker exited", ts(5))
            .expect("fail");
        let view = derive_attention(&objective, &AttentionFacts::default());
        assert_eq!(
            (view.attention, view.badges),
            (Attention::NeedsAttention, vec![Badge::Failed])
        );
    }

    #[test]
    fn board_order_sorts_by_precedence() {
        let mut board = vec![
            Attention::Archived,
            Attention::Idle,
            Attention::Working,
            Attention::ReadyForReview,
            Attention::NeedsAttention,
        ];
        board.sort();
        assert_eq!(
            board,
            vec![
                Attention::NeedsAttention,
                Attention::ReadyForReview,
                Attention::Working,
                Attention::Idle,
                Attention::Archived
            ]
        );
        assert_eq!(Attention::NeedsAttention.code(), "needs_attention");
    }
}
