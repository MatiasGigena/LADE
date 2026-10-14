use crate::domain::ids::{ApprovalId, CandidateId, GitId, ObjectiveId, Timestamp, VerificationId};
use crate::domain::inspection::{ArtifactDigest, MAX_TEXT_BYTES};
use crate::domain::objectives::{Lifecycle, Objective};
use crate::error::DomainError;

pub const MAX_ARGV_ITEMS: usize = 256;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChangeCounts {
    pub files: u32,
    pub added: u64,
    pub removed: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateStatus {
    Current,
    Historical,
    Invalidated,
}

impl CandidateStatus {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Historical => "historical",
            Self::Invalidated => "invalidated",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub id: CandidateId,
    pub objective_id: ObjectiveId,
    pub ordinal: u32,
    pub commit: GitId,
    pub tree: GitId,
    pub base_target_commit: GitId,
    pub manifest_digest: ArtifactDigest,
    pub changes: ChangeCounts,
    pub created_at: Timestamp,
    pub status: CandidateStatus,
}

#[derive(Clone, Debug)]
pub struct NewCandidate {
    pub id: CandidateId,
    pub commit: GitId,
    pub tree: GitId,
    pub base_target_commit: GitId,
    pub manifest_digest: ArtifactDigest,
    pub changes: ChangeCounts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerificationOutcome {
    Running,
    Passed,
    Failed,
    Interrupted,
    Error,
}

impl VerificationOutcome {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provenance {
    ServiceObserved,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verification {
    pub id: VerificationId,
    pub candidate_id: CandidateId,
    pub argv: Vec<String>,
    pub cwd_label: String,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub exit_code: Option<i32>,
    pub outcome: VerificationOutcome,
    pub output_artifact: Option<ArtifactDigest>,
    pub truncated: bool,
    pub provenance: Provenance,
}

#[derive(Clone, Debug)]
pub struct NewVerification {
    pub id: VerificationId,
    pub candidate_id: CandidateId,
    pub argv: Vec<String>,
    pub cwd_label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerificationEnd {
    Exited { code: i32 },
    Interrupted,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidationReason {
    ReturnedForRevision,
    TargetMoved,
    CandidateChanged,
    ServiceRestart,
    Discarded,
}

impl InvalidationReason {
    pub const fn code(self) -> &'static str {
        match self {
            Self::ReturnedForRevision => "returned_for_revision",
            Self::TargetMoved => "target_moved",
            Self::CandidateChanged => "candidate_changed",
            Self::ServiceRestart => "service_restart",
            Self::Discarded => "discarded",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalState {
    Valid,
    Consumed,
    Invalidated { reason: InvalidationReason },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approval {
    pub id: ApprovalId,
    pub candidate_id: CandidateId,
    pub target_commit: GitId,
    pub override_reason: Option<String>,
    pub state: ApprovalState,
    pub approved_at: Timestamp,
}

#[derive(Clone, Debug)]
pub struct ApproveCandidate {
    pub approval_id: ApprovalId,
    pub candidate_id: CandidateId,
    pub target_commit: GitId,
    pub override_reason: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Evidence {
    Missing,
    Running,
    Failed,
    Passed,
}

#[derive(Clone, Debug)]
pub enum RefreshOutcome {
    Rebuilt(NewCandidate),
    Conflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshResult {
    Refreshed { candidate_id: CandidateId },
    Conflicted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntegrationPlan {
    pub approval_id: ApprovalId,
    pub candidate_id: CandidateId,
    pub candidate_commit: GitId,
    pub candidate_tree: GitId,
    pub expected_target: GitId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Review {
    objective_id: ObjectiveId,
    candidates: Vec<Candidate>,
    verifications: Vec<Verification>,
    approvals: Vec<Approval>,
}

impl Review {
    pub const fn new(objective_id: ObjectiveId) -> Self {
        Self {
            objective_id,
            candidates: Vec::new(),
            verifications: Vec::new(),
            approvals: Vec::new(),
        }
    }

    pub fn restore(
        objective_id: ObjectiveId,
        candidates: Vec<Candidate>,
        verifications: Vec<Verification>,
        approvals: Vec<Approval>,
    ) -> Result<Self, DomainError> {
        let ordered = candidates
            .windows(2)
            .all(|pair| pair[0].ordinal < pair[1].ordinal);
        let owned = candidates
            .iter()
            .all(|candidate| candidate.objective_id == objective_id);
        let known = |id: CandidateId| candidates.iter().any(|candidate| candidate.id == id);
        let current = candidates
            .iter()
            .filter(|candidate| candidate.status == CandidateStatus::Current)
            .count();
        let valid = approvals
            .iter()
            .filter(|approval| approval.state == ApprovalState::Valid)
            .count();
        let consistent = ordered
            && owned
            && current <= 1
            && valid <= 1
            && verifications
                .iter()
                .all(|verification| known(verification.candidate_id))
            && approvals
                .iter()
                .all(|approval| known(approval.candidate_id));
        if !consistent {
            return Err(DomainError::DurabilityFailure("inconsistent stored review"));
        }
        Ok(Self {
            objective_id,
            candidates,
            verifications,
            approvals,
        })
    }

    pub const fn objective_id(&self) -> ObjectiveId {
        self.objective_id
    }

    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }

    pub fn verifications(&self) -> &[Verification] {
        &self.verifications
    }

    pub fn approvals(&self) -> &[Approval] {
        &self.approvals
    }

    pub fn candidate(&self, id: CandidateId) -> Option<&Candidate> {
        self.candidates.iter().find(|candidate| candidate.id == id)
    }

    pub fn current_candidate(&self) -> Option<&Candidate> {
        self.candidates
            .iter()
            .find(|candidate| candidate.status == CandidateStatus::Current)
    }

    pub fn latest_candidate(&self) -> Option<&Candidate> {
        self.candidates.last()
    }

    pub fn valid_approval(&self) -> Option<&Approval> {
        self.approvals
            .iter()
            .find(|approval| approval.state == ApprovalState::Valid)
    }

    pub fn latest_verifications(&self, candidate_id: CandidateId) -> Vec<&Verification> {
        let mut latest: Vec<&Verification> = Vec::new();
        for verification in self
            .verifications
            .iter()
            .filter(|v| v.candidate_id == candidate_id)
        {
            match latest
                .iter_mut()
                .find(|seen| seen.argv == verification.argv)
            {
                Some(slot) => *slot = verification,
                None => latest.push(verification),
            }
        }
        latest
    }

    pub fn evidence(&self, candidate_id: CandidateId) -> Evidence {
        let latest = self.latest_verifications(candidate_id);
        if latest.is_empty() {
            Evidence::Missing
        } else if latest
            .iter()
            .any(|v| v.outcome == VerificationOutcome::Running)
        {
            Evidence::Running
        } else if latest
            .iter()
            .all(|v| v.outcome == VerificationOutcome::Passed)
        {
            Evidence::Passed
        } else {
            Evidence::Failed
        }
    }

    pub fn verification_failed(&self) -> bool {
        self.current_candidate()
            .is_some_and(|candidate| self.evidence(candidate.id) == Evidence::Failed)
    }

    pub fn target_stale(&self, current_target: &GitId) -> bool {
        self.current_candidate()
            .is_some_and(|candidate| candidate.base_target_commit != *current_target)
    }

    fn check_objective(&self, objective: &Objective) -> Result<(), DomainError> {
        if objective.id() != self.objective_id {
            return Err(DomainError::InvalidRequest(
                "review belongs to another objective",
            ));
        }
        Ok(())
    }

    fn current_named(&self, candidate_id: CandidateId) -> Result<&Candidate, DomainError> {
        let candidate = self
            .candidate(candidate_id)
            .ok_or(DomainError::NotFound("candidate"))?;
        if candidate.status != CandidateStatus::Current {
            return Err(DomainError::StaleCandidate);
        }
        Ok(candidate)
    }

    fn validate_new_candidate(&self, new: &NewCandidate) -> Result<u32, DomainError> {
        if new.changes.files == 0 {
            return Err(DomainError::InvalidRequest(
                "empty change set; complete without changes instead",
            ));
        }
        if self.candidate(new.id).is_some() {
            return Err(DomainError::Conflict("candidate id reused"));
        }
        let last = self
            .candidates
            .last()
            .map_or(0, |candidate| candidate.ordinal);
        last.checked_add(1)
            .ok_or(DomainError::LimitReached("candidate ordinal exhausted"))
    }

    fn push_candidate(&mut self, new: NewCandidate, ordinal: u32, now: Timestamp) -> CandidateId {
        for candidate in &mut self.candidates {
            if candidate.status == CandidateStatus::Current {
                candidate.status = CandidateStatus::Historical;
            }
        }
        let id = new.id;
        self.candidates.push(Candidate {
            id,
            objective_id: self.objective_id,
            ordinal,
            commit: new.commit,
            tree: new.tree,
            base_target_commit: new.base_target_commit,
            manifest_digest: new.manifest_digest,
            changes: new.changes,
            created_at: now,
            status: CandidateStatus::Current,
        });
        id
    }

    fn invalidate_valid_approval(&mut self, reason: InvalidationReason) {
        for approval in &mut self.approvals {
            if approval.state == ApprovalState::Valid {
                approval.state = ApprovalState::Invalidated { reason };
            }
        }
    }

    fn retire_current(&mut self) {
        for candidate in &mut self.candidates {
            if candidate.status != CandidateStatus::Historical {
                candidate.status = CandidateStatus::Historical;
            }
        }
    }

    fn check_no_running_verification(&self) -> Result<(), DomainError> {
        if self
            .verifications
            .iter()
            .any(|v| v.outcome == VerificationOutcome::Running)
        {
            return Err(DomainError::Conflict("stop running verification first"));
        }
        Ok(())
    }

    pub fn freeze(
        &mut self,
        objective: &mut Objective,
        new: NewCandidate,
        now: Timestamp,
    ) -> Result<CandidateId, DomainError> {
        self.check_objective(objective)?;
        if objective.lifecycle() != Lifecycle::Active {
            return Err(DomainError::InvalidLifecycle(
                "freeze requires an active objective",
            ));
        }
        let ordinal = self.validate_new_candidate(&new)?;
        objective.transition(&[Lifecycle::Frozen], now)?;
        Ok(self.push_candidate(new, ordinal, now))
    }

    pub fn return_for_revision(
        &mut self,
        objective: &mut Objective,
        candidate_id: CandidateId,
        now: Timestamp,
    ) -> Result<(), DomainError> {
        self.check_objective(objective)?;
        let path: &[Lifecycle] = match objective.lifecycle() {
            Lifecycle::Frozen => &[Lifecycle::Active],
            Lifecycle::Approved => &[Lifecycle::Frozen, Lifecycle::Active],
            _ => return Err(DomainError::InvalidLifecycle("nothing frozen to return")),
        };
        let latest = self
            .latest_candidate()
            .ok_or(DomainError::NotFound("candidate"))?;
        if latest.id != candidate_id || latest.status == CandidateStatus::Historical {
            return Err(if self.candidate(candidate_id).is_some() {
                DomainError::StaleCandidate
            } else {
                DomainError::NotFound("candidate")
            });
        }
        objective.transition(path, now)?;
        self.invalidate_valid_approval(InvalidationReason::ReturnedForRevision);
        self.retire_current();
        Ok(())
    }

    pub fn start_verification(
        &mut self,
        objective: &Objective,
        new: NewVerification,
        now: Timestamp,
    ) -> Result<(), DomainError> {
        self.check_objective(objective)?;
        if objective.lifecycle() != Lifecycle::Frozen {
            return Err(DomainError::InvalidLifecycle(
                "verification requires a frozen, unapproved candidate",
            ));
        }
        self.current_named(new.candidate_id)?;
        let argv_valid = !new.argv.is_empty()
            && new.argv.len() <= MAX_ARGV_ITEMS
            && !new.argv[0].is_empty()
            && new
                .argv
                .iter()
                .all(|arg| !arg.contains('\0') && arg.len() <= MAX_TEXT_BYTES)
            && new.cwd_label.len() <= MAX_TEXT_BYTES;
        if !argv_valid {
            return Err(DomainError::InvalidRequest("invalid verification command"));
        }
        if self.verifications.iter().any(|v| v.id == new.id) {
            return Err(DomainError::Conflict("verification id reused"));
        }
        self.verifications.push(Verification {
            id: new.id,
            candidate_id: new.candidate_id,
            argv: new.argv,
            cwd_label: new.cwd_label,
            started_at: now,
            ended_at: None,
            exit_code: None,
            outcome: VerificationOutcome::Running,
            output_artifact: None,
            truncated: false,
            provenance: Provenance::ServiceObserved,
        });
        Ok(())
    }

    pub fn finish_verification(
        &mut self,
        id: VerificationId,
        end: VerificationEnd,
        output_artifact: Option<ArtifactDigest>,
        truncated: bool,
        now: Timestamp,
    ) -> Result<VerificationOutcome, DomainError> {
        let verification = self
            .verifications
            .iter_mut()
            .find(|v| v.id == id)
            .ok_or(DomainError::NotFound("verification"))?;
        if verification.outcome != VerificationOutcome::Running {
            return Err(DomainError::Conflict("verification already finished"));
        }
        let (outcome, exit_code) = match end {
            VerificationEnd::Exited { code: 0 } => (VerificationOutcome::Passed, Some(0)),
            VerificationEnd::Exited { code } => (VerificationOutcome::Failed, Some(code)),
            VerificationEnd::Interrupted => (VerificationOutcome::Interrupted, None),
            VerificationEnd::Error => (VerificationOutcome::Error, None),
        };
        verification.outcome = outcome;
        verification.exit_code = exit_code;
        verification.ended_at = Some(now);
        verification.output_artifact = output_artifact;
        verification.truncated = truncated;
        Ok(outcome)
    }

    pub fn approve(
        &mut self,
        objective: &mut Objective,
        command: ApproveCandidate,
        current_target: &GitId,
        now: Timestamp,
    ) -> Result<ApprovalId, DomainError> {
        self.check_objective(objective)?;
        if objective.lifecycle() != Lifecycle::Frozen {
            return Err(DomainError::InvalidLifecycle(
                "approval requires a frozen candidate",
            ));
        }
        let candidate = self.current_named(command.candidate_id)?;
        if command.target_commit != *current_target
            || candidate.base_target_commit != *current_target
        {
            return Err(DomainError::StaleTarget);
        }
        let override_reason = command
            .override_reason
            .filter(|reason| !reason.trim().is_empty());
        if override_reason
            .as_ref()
            .is_some_and(|reason| reason.len() > MAX_TEXT_BYTES)
        {
            return Err(DomainError::LimitReached("override reason too large"));
        }
        match self.evidence(candidate.id) {
            Evidence::Passed => {}
            Evidence::Running => return Err(DomainError::Conflict("verification still running")),
            Evidence::Missing | Evidence::Failed if override_reason.is_some() => {}
            Evidence::Missing | Evidence::Failed => {
                return Err(DomainError::InvalidRequest(
                    "verification missing or failed; override reason required",
                ));
            }
        }
        if self
            .approvals
            .iter()
            .any(|approval| approval.id == command.approval_id)
        {
            return Err(DomainError::Conflict("approval id reused"));
        }
        objective.transition(&[Lifecycle::Approved], now)?;
        self.approvals.push(Approval {
            id: command.approval_id,
            candidate_id: command.candidate_id,
            target_commit: current_target.clone(),
            override_reason,
            state: ApprovalState::Valid,
            approved_at: now,
        });
        Ok(command.approval_id)
    }

    pub fn invalidate_approval(
        &mut self,
        objective: &mut Objective,
        reason: InvalidationReason,
        now: Timestamp,
    ) -> Result<bool, DomainError> {
        self.check_objective(objective)?;
        if self.valid_approval().is_none() {
            return Ok(false);
        }
        objective.transition(&[Lifecycle::Frozen], now)?;
        self.invalidate_valid_approval(reason);
        Ok(true)
    }

    pub fn observe_target(
        &mut self,
        objective: &mut Objective,
        current_target: &GitId,
        now: Timestamp,
    ) -> Result<bool, DomainError> {
        let stale = self.target_stale(current_target);
        if stale {
            self.invalidate_approval(objective, InvalidationReason::TargetMoved, now)?;
        }
        Ok(stale)
    }

    pub fn invalidate_candidate(
        &mut self,
        objective: &mut Objective,
        now: Timestamp,
    ) -> Result<(), DomainError> {
        self.check_objective(objective)?;
        let path: &[Lifecycle] = match objective.lifecycle() {
            Lifecycle::Frozen => &[],
            Lifecycle::Approved => &[Lifecycle::Frozen],
            _ => return Err(DomainError::InvalidLifecycle("no frozen candidate")),
        };
        if self.current_candidate().is_none() {
            return Err(DomainError::NotFound("candidate"));
        }
        objective.transition(path, now)?;
        self.invalidate_valid_approval(InvalidationReason::CandidateChanged);
        for candidate in &mut self.candidates {
            if candidate.status == CandidateStatus::Current {
                candidate.status = CandidateStatus::Invalidated;
            }
        }
        Ok(())
    }

    pub fn refresh(
        &mut self,
        objective: &mut Objective,
        candidate_id: CandidateId,
        current_target: &GitId,
        outcome: RefreshOutcome,
        now: Timestamp,
    ) -> Result<RefreshResult, DomainError> {
        self.check_objective(objective)?;
        let approved = match objective.lifecycle() {
            Lifecycle::Frozen => false,
            Lifecycle::Approved => true,
            _ => {
                return Err(DomainError::InvalidLifecycle(
                    "refresh requires a frozen candidate",
                ));
            }
        };
        let candidate = self.current_named(candidate_id)?;
        if candidate.base_target_commit == *current_target {
            return Err(DomainError::Conflict("target has not moved"));
        }
        match outcome {
            RefreshOutcome::Rebuilt(new) => {
                if new.base_target_commit != *current_target {
                    return Err(DomainError::StaleTarget);
                }
                let ordinal = self.validate_new_candidate(&new)?;
                let path: &[Lifecycle] = if approved { &[Lifecycle::Frozen] } else { &[] };
                objective.transition(path, now)?;
                self.invalidate_valid_approval(InvalidationReason::TargetMoved);
                let candidate_id = self.push_candidate(new, ordinal, now);
                Ok(RefreshResult::Refreshed { candidate_id })
            }
            RefreshOutcome::Conflict => {
                let path: &[Lifecycle] = if approved {
                    &[Lifecycle::Frozen, Lifecycle::Active]
                } else {
                    &[Lifecycle::Active]
                };
                objective.transition(path, now)?;
                self.invalidate_valid_approval(InvalidationReason::TargetMoved);
                self.retire_current();
                Ok(RefreshResult::Conflicted)
            }
        }
    }

    pub fn check_integration(
        &self,
        objective: &Objective,
        approval_id: ApprovalId,
        current_target: &GitId,
    ) -> Result<IntegrationPlan, DomainError> {
        self.check_objective(objective)?;
        if objective.lifecycle() != Lifecycle::Approved {
            return Err(DomainError::InvalidLifecycle(
                "integration requires an approved candidate",
            ));
        }
        let approval = self
            .approvals
            .iter()
            .find(|approval| approval.id == approval_id)
            .ok_or(DomainError::NotFound("approval"))?;
        if approval.state != ApprovalState::Valid {
            return Err(DomainError::Conflict("approval is no longer valid"));
        }
        let candidate = self.current_named(approval.candidate_id)?;
        if approval.target_commit != *current_target
            || candidate.base_target_commit != *current_target
        {
            return Err(DomainError::StaleTarget);
        }
        Ok(IntegrationPlan {
            approval_id,
            candidate_id: candidate.id,
            candidate_commit: candidate.commit.clone(),
            candidate_tree: candidate.tree.clone(),
            expected_target: current_target.clone(),
        })
    }

    pub fn complete_integration(
        &mut self,
        objective: &mut Objective,
        plan: &IntegrationPlan,
        now: Timestamp,
    ) -> Result<(), DomainError> {
        if self.check_integration(objective, plan.approval_id, &plan.expected_target)? != *plan {
            return Err(DomainError::StaleCandidate);
        }
        objective.transition(&[Lifecycle::Integrated], now)?;
        for approval in &mut self.approvals {
            if approval.id == plan.approval_id {
                approval.state = ApprovalState::Consumed;
            }
        }
        Ok(())
    }

    pub fn recover_after_restart(
        &mut self,
        objective: &mut Objective,
        now: Timestamp,
    ) -> Result<(), DomainError> {
        self.check_objective(objective)?;
        self.invalidate_approval(objective, InvalidationReason::ServiceRestart, now)?;
        for verification in &mut self.verifications {
            if verification.outcome == VerificationOutcome::Running {
                verification.outcome = VerificationOutcome::Interrupted;
                verification.ended_at = Some(now);
            }
        }
        Ok(())
    }

    pub fn discard(
        &mut self,
        objective: &mut Objective,
        now: Timestamp,
    ) -> Result<(), DomainError> {
        self.check_objective(objective)?;
        self.check_no_running_verification()?;
        objective.transition(&[Lifecycle::Discarded], now)?;
        self.invalidate_valid_approval(InvalidationReason::Discarded);
        Ok(())
    }

    pub fn complete_without_changes(
        &mut self,
        objective: &mut Objective,
        change_set_empty: bool,
        now: Timestamp,
    ) -> Result<(), DomainError> {
        self.check_objective(objective)?;
        if !change_set_empty {
            return Err(DomainError::Conflict("change set is not empty"));
        }
        self.check_no_running_verification()?;
        objective.transition(&[Lifecycle::CompletedNoChanges], now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{
        active_objective, approval_id, candidate_id, defined_objective, digest, git,
        stopped_objective, ts, verification_id,
    };

    fn new_candidate(id: u128, base: &GitId) -> NewCandidate {
        NewCandidate {
            id: candidate_id(id),
            commit: git(100 + id),
            tree: git(200 + id),
            base_target_commit: base.clone(),
            manifest_digest: digest(id),
            changes: ChangeCounts {
                files: 2,
                added: 10,
                removed: 3,
            },
        }
    }

    fn frozen() -> (Objective, Review, GitId) {
        let mut objective = stopped_objective();
        let mut review = Review::new(objective.id());
        let target = git(1);
        review
            .freeze(&mut objective, new_candidate(1, &target), ts(10))
            .expect("freeze");
        (objective, review, target)
    }

    fn verify(
        review: &mut Review,
        objective: &Objective,
        id: u128,
        argv: &[&str],
        end: VerificationEnd,
    ) {
        let candidate = review.current_candidate().expect("current").id;
        let command = NewVerification {
            id: verification_id(id),
            candidate_id: candidate,
            argv: argv.iter().map(ToString::to_string).collect(),
            cwd_label: "verification copy".into(),
        };
        review
            .start_verification(objective, command, ts(11))
            .expect("start");
        review
            .finish_verification(verification_id(id), end, Some(digest(50)), false, ts(12))
            .expect("finish");
    }

    fn approve_cmd(
        id: u128,
        candidate: u128,
        target: &GitId,
        reason: Option<&str>,
    ) -> ApproveCandidate {
        ApproveCandidate {
            approval_id: approval_id(id),
            candidate_id: candidate_id(candidate),
            target_commit: target.clone(),
            override_reason: reason.map(ToString::to_string),
        }
    }

    fn approved() -> (Objective, Review, GitId) {
        let (mut objective, mut review, target) = frozen();
        verify(
            &mut review,
            &objective,
            1,
            &["cargo", "test"],
            VerificationEnd::Exited { code: 0 },
        );
        review
            .approve(
                &mut objective,
                approve_cmd(1, 1, &target, None),
                &target,
                ts(13),
            )
            .expect("approve");
        (objective, review, target)
    }

    #[test]
    fn freeze_requires_active_stopped_and_non_empty() {
        let (mut live, _) = active_objective();
        let mut review = Review::new(live.id());
        assert!(matches!(
            review.freeze(&mut live, new_candidate(1, &git(1)), ts(5)),
            Err(DomainError::InvalidLifecycle(_))
        ));
        let mut defined = defined_objective();
        let mut review = Review::new(defined.id());
        assert!(
            review
                .freeze(&mut defined, new_candidate(1, &git(1)), ts(5))
                .is_err()
        );
        let mut objective = stopped_objective();
        let mut review = Review::new(objective.id());
        let mut empty = new_candidate(1, &git(1));
        empty.changes.files = 0;
        assert!(matches!(
            review.freeze(&mut objective, empty, ts(5)),
            Err(DomainError::InvalidRequest(_))
        ));
        assert_eq!(objective.lifecycle(), Lifecycle::Active);
        let id = review
            .freeze(&mut objective, new_candidate(1, &git(1)), ts(5))
            .expect("freeze");
        assert_eq!(objective.lifecycle(), Lifecycle::Frozen);
        assert_eq!(
            review.current_candidate().map(|c| (c.id, c.ordinal)),
            Some((id, 1))
        );
    }

    #[test]
    fn approval_requires_passing_latest_checks_or_override() {
        let (mut objective, mut review, target) = frozen();
        assert_eq!(review.evidence(candidate_id(1)), Evidence::Missing);
        assert!(matches!(
            review.approve(
                &mut objective,
                approve_cmd(1, 1, &target, None),
                &target,
                ts(13)
            ),
            Err(DomainError::InvalidRequest(_))
        ));
        assert!(matches!(
            review.approve(
                &mut objective,
                approve_cmd(1, 1, &target, Some("   ")),
                &target,
                ts(13)
            ),
            Err(DomainError::InvalidRequest(_))
        ));
        verify(
            &mut review,
            &objective,
            1,
            &["cargo", "test"],
            VerificationEnd::Exited { code: 1 },
        );
        verify(
            &mut review,
            &objective,
            2,
            &["git", "diff", "--check"],
            VerificationEnd::Exited { code: 0 },
        );
        assert_eq!(review.evidence(candidate_id(1)), Evidence::Failed);
        assert!(review.verification_failed());
        assert!(
            review
                .approve(
                    &mut objective,
                    approve_cmd(1, 1, &target, None),
                    &target,
                    ts(13)
                )
                .is_err()
        );
        verify(
            &mut review,
            &objective,
            3,
            &["cargo", "test"],
            VerificationEnd::Exited { code: 0 },
        );
        assert_eq!(review.latest_verifications(candidate_id(1)).len(), 2);
        assert_eq!(review.evidence(candidate_id(1)), Evidence::Passed);
        review
            .approve(
                &mut objective,
                approve_cmd(1, 1, &target, None),
                &target,
                ts(14),
            )
            .expect("approve");
        assert_eq!(objective.lifecycle(), Lifecycle::Approved);
        assert_eq!(
            review.valid_approval().map(|a| a.override_reason.clone()),
            Some(None)
        );
    }

    #[test]
    fn override_reason_permits_failed_or_missing_checks() {
        let (mut objective, mut review, target) = frozen();
        verify(
            &mut review,
            &objective,
            1,
            &["make"],
            VerificationEnd::Interrupted,
        );
        review
            .approve(
                &mut objective,
                approve_cmd(1, 1, &target, Some("flaky network test")),
                &target,
                ts(13),
            )
            .expect("override");
        assert_eq!(
            review
                .valid_approval()
                .and_then(|a| a.override_reason.as_deref()),
            Some("flaky network test")
        );
    }

    #[test]
    fn approval_rejects_running_checks_stale_candidate_and_target() {
        let (mut objective, mut review, target) = frozen();
        let command = NewVerification {
            id: verification_id(1),
            candidate_id: candidate_id(1),
            argv: vec!["cargo".into()],
            cwd_label: String::new(),
        };
        review
            .start_verification(&objective, command, ts(11))
            .expect("start");
        assert_eq!(
            review.approve(
                &mut objective,
                approve_cmd(1, 1, &target, Some("x")),
                &target,
                ts(12)
            ),
            Err(DomainError::Conflict("verification still running"))
        );
        review
            .finish_verification(
                verification_id(1),
                VerificationEnd::Exited { code: 0 },
                None,
                false,
                ts(12),
            )
            .expect("finish");
        assert!(
            review
                .finish_verification(
                    verification_id(1),
                    VerificationEnd::Error,
                    None,
                    false,
                    ts(12)
                )
                .is_err()
        );
        let moved = git(2);
        assert_eq!(
            review.approve(
                &mut objective,
                approve_cmd(1, 1, &target, None),
                &moved,
                ts(13)
            ),
            Err(DomainError::StaleTarget)
        );
        assert_eq!(
            review.approve(
                &mut objective,
                approve_cmd(1, 1, &moved, None),
                &moved,
                ts(13)
            ),
            Err(DomainError::StaleTarget)
        );
        assert_eq!(
            review.approve(
                &mut objective,
                approve_cmd(1, 9, &target, None),
                &target,
                ts(13)
            ),
            Err(DomainError::NotFound("candidate"))
        );
        assert_eq!(objective.lifecycle(), Lifecycle::Frozen);
    }

    #[test]
    fn approval_requires_frozen_lifecycle() {
        let (mut objective, mut review, target) = approved();
        assert!(matches!(
            review.approve(
                &mut objective,
                approve_cmd(2, 1, &target, Some("x")),
                &target,
                ts(20)
            ),
            Err(DomainError::InvalidLifecycle(_))
        ));
        assert!(matches!(
            review.start_verification(
                &objective,
                NewVerification {
                    id: verification_id(9),
                    candidate_id: candidate_id(1),
                    argv: vec!["x".into()],
                    cwd_label: String::new()
                },
                ts(20)
            ),
            Err(DomainError::InvalidLifecycle(_))
        ));
    }

    #[test]
    fn return_for_revision_invalidates_approval_and_keeps_history() {
        let (mut objective, mut review, _) = approved();
        assert_eq!(
            review.return_for_revision(&mut objective, candidate_id(9), ts(20)),
            Err(DomainError::NotFound("candidate"))
        );
        review
            .return_for_revision(&mut objective, candidate_id(1), ts(20))
            .expect("return");
        assert_eq!(objective.lifecycle(), Lifecycle::Active);
        assert_eq!(
            review.approvals()[0].state,
            ApprovalState::Invalidated {
                reason: InvalidationReason::ReturnedForRevision
            }
        );
        assert_eq!(review.candidates()[0].status, CandidateStatus::Historical);
        assert!(review.current_candidate().is_none());
        assert!(
            review
                .return_for_revision(&mut objective, candidate_id(1), ts(21))
                .is_err(),
            "not frozen"
        );
        review
            .freeze(&mut objective, new_candidate(2, &git(1)), ts(22))
            .expect("refreeze");
        assert_eq!(review.current_candidate().map(|c| c.ordinal), Some(2));
        assert_eq!(
            review.return_for_revision(&mut objective, candidate_id(1), ts(23)),
            Err(DomainError::StaleCandidate)
        );
    }

    #[test]
    fn integration_requires_valid_approval_and_unchanged_target() {
        let (mut objective, mut review, target) = approved();
        assert_eq!(
            review.check_integration(&objective, approval_id(9), &target),
            Err(DomainError::NotFound("approval"))
        );
        assert_eq!(
            review.check_integration(&objective, approval_id(1), &git(2)),
            Err(DomainError::StaleTarget)
        );
        let plan = review
            .check_integration(&objective, approval_id(1), &target)
            .expect("plan");
        assert_eq!(plan.candidate_commit, git(101));
        assert_eq!(plan.expected_target, target);
        review
            .complete_integration(&mut objective, &plan, ts(30))
            .expect("integrated");
        assert_eq!(objective.lifecycle(), Lifecycle::Integrated);
        assert_eq!(review.approvals()[0].state, ApprovalState::Consumed);
        assert!(
            review
                .complete_integration(&mut objective, &plan, ts(31))
                .is_err(),
            "consumed once"
        );
        assert!(review.discard(&mut objective, ts(32)).is_err(), "terminal");
    }

    #[test]
    fn integration_rejects_frozen_and_invalidated() {
        let (mut objective, mut review, target) = approved();
        assert_eq!(
            review.invalidate_approval(
                &mut objective,
                InvalidationReason::CandidateChanged,
                ts(20)
            ),
            Ok(true)
        );
        assert_eq!(
            review.invalidate_approval(
                &mut objective,
                InvalidationReason::CandidateChanged,
                ts(20)
            ),
            Ok(false)
        );
        assert!(matches!(
            review.check_integration(&objective, approval_id(1), &target),
            Err(DomainError::InvalidLifecycle(_))
        ));
        review
            .approve(
                &mut objective,
                approve_cmd(2, 1, &target, None),
                &target,
                ts(21),
            )
            .expect("re-approve");
        assert_eq!(
            review.check_integration(&objective, approval_id(1), &target),
            Err(DomainError::Conflict("approval is no longer valid"))
        );
        assert!(
            review
                .check_integration(&objective, approval_id(2), &target)
                .is_ok()
        );
    }

    #[test]
    fn target_move_invalidates_and_refresh_builds_new_candidate() {
        let (mut objective, mut review, target) = approved();
        assert_eq!(
            review.observe_target(&mut objective, &target, ts(20)),
            Ok(false)
        );
        assert_eq!(objective.lifecycle(), Lifecycle::Approved);
        let moved = git(2);
        assert_eq!(
            review.observe_target(&mut objective, &moved, ts(21)),
            Ok(true)
        );
        assert_eq!(objective.lifecycle(), Lifecycle::Frozen);
        assert_eq!(
            review.approvals()[0].state,
            ApprovalState::Invalidated {
                reason: InvalidationReason::TargetMoved
            }
        );
        assert!(review.target_stale(&moved));
        assert_eq!(
            review.refresh(
                &mut objective,
                candidate_id(1),
                &target,
                RefreshOutcome::Rebuilt(new_candidate(2, &target)),
                ts(22)
            ),
            Err(DomainError::Conflict("target has not moved"))
        );
        assert_eq!(
            review.refresh(
                &mut objective,
                candidate_id(1),
                &moved,
                RefreshOutcome::Rebuilt(new_candidate(2, &target)),
                ts(22)
            ),
            Err(DomainError::StaleTarget)
        );
        let result = review
            .refresh(
                &mut objective,
                candidate_id(1),
                &moved,
                RefreshOutcome::Rebuilt(new_candidate(2, &moved)),
                ts(23),
            )
            .expect("refresh");
        assert_eq!(
            result,
            RefreshResult::Refreshed {
                candidate_id: candidate_id(2)
            }
        );
        assert_eq!(objective.lifecycle(), Lifecycle::Frozen);
        assert_eq!(review.candidates()[0].status, CandidateStatus::Historical);
        assert_eq!(
            review.evidence(candidate_id(2)),
            Evidence::Missing,
            "refreshed candidate needs checks again"
        );
        assert!(
            review
                .approve(
                    &mut objective,
                    approve_cmd(2, 2, &moved, None),
                    &moved,
                    ts(24)
                )
                .is_err()
        );
        assert_eq!(
            review.approve(
                &mut objective,
                approve_cmd(2, 1, &moved, Some("x")),
                &moved,
                ts(24)
            ),
            Err(DomainError::StaleCandidate)
        );
    }

    #[test]
    fn refresh_from_approved_and_conflict_returns_for_revision() {
        let (mut objective, mut review, _) = approved();
        let moved = git(2);
        let result = review
            .refresh(
                &mut objective,
                candidate_id(1),
                &moved,
                RefreshOutcome::Conflict,
                ts(20),
            )
            .expect("refresh");
        assert_eq!(result, RefreshResult::Conflicted);
        assert_eq!(objective.lifecycle(), Lifecycle::Active);
        assert!(review.valid_approval().is_none());
        assert!(review.current_candidate().is_none());
        assert!(matches!(
            review.refresh(
                &mut objective,
                candidate_id(1),
                &moved,
                RefreshOutcome::Conflict,
                ts(21)
            ),
            Err(DomainError::InvalidLifecycle(_))
        ));
    }

    #[test]
    fn unexpected_change_invalidates_candidate() {
        let (mut objective, mut review, target) = approved();
        review
            .invalidate_candidate(&mut objective, ts(20))
            .expect("invalidate");
        assert_eq!(objective.lifecycle(), Lifecycle::Frozen);
        assert_eq!(review.candidates()[0].status, CandidateStatus::Invalidated);
        assert_eq!(
            review.approvals()[0].state,
            ApprovalState::Invalidated {
                reason: InvalidationReason::CandidateChanged
            }
        );
        assert_eq!(
            review.approve(
                &mut objective,
                approve_cmd(2, 1, &target, Some("x")),
                &target,
                ts(21)
            ),
            Err(DomainError::StaleCandidate)
        );
        review
            .return_for_revision(&mut objective, candidate_id(1), ts(22))
            .expect("return");
        assert_eq!(review.candidates()[0].status, CandidateStatus::Historical);
    }

    #[test]
    fn restart_expires_approval_and_interrupts_checks() {
        let (mut objective, mut review, target) = approved();
        review
            .recover_after_restart(&mut objective, ts(40))
            .expect("recover");
        assert_eq!(objective.lifecycle(), Lifecycle::Frozen);
        assert_eq!(
            review.approvals()[0].state,
            ApprovalState::Invalidated {
                reason: InvalidationReason::ServiceRestart
            }
        );
        let command = NewVerification {
            id: verification_id(5),
            candidate_id: candidate_id(1),
            argv: vec!["make".into()],
            cwd_label: String::new(),
        };
        review
            .start_verification(&objective, command, ts(41))
            .expect("start");
        assert!(
            review.discard(&mut objective, ts(41)).is_err(),
            "running verification"
        );
        review
            .recover_after_restart(&mut objective, ts(42))
            .expect("recover");
        let last = review.verifications().last().expect("verification");
        assert_eq!(
            (last.outcome, last.ended_at),
            (VerificationOutcome::Interrupted, Some(ts(42)))
        );
        assert!(
            review
                .check_integration(&objective, approval_id(1), &target)
                .is_err()
        );
    }

    #[test]
    fn discard_and_complete_without_changes() {
        let (mut objective, mut review, _) = approved();
        review
            .discard(&mut objective, ts(20))
            .expect("discard approved");
        assert_eq!(objective.lifecycle(), Lifecycle::Discarded);
        assert_eq!(
            review.approvals()[0].state,
            ApprovalState::Invalidated {
                reason: InvalidationReason::Discarded
            }
        );

        let mut defined = defined_objective();
        let mut review = Review::new(defined.id());
        assert_eq!(
            review.complete_without_changes(&mut defined, false, ts(2)),
            Err(DomainError::Conflict("change set is not empty"))
        );
        review
            .complete_without_changes(&mut defined, true, ts(2))
            .expect("complete");
        assert_eq!(defined.lifecycle(), Lifecycle::CompletedNoChanges);

        let (mut approved_objective, mut approved_review, _) = approved();
        assert!(matches!(
            approved_review.complete_without_changes(&mut approved_objective, true, ts(20)),
            Err(DomainError::InvalidLifecycle(_))
        ));
    }

    #[test]
    fn review_rejects_foreign_objective_and_restores_consistently() {
        let (objective, review, _) = approved();
        let mut other = defined_objective();
        let mut foreign = Review::new(crate::testing::objective_id(77));
        assert!(matches!(
            foreign.discard(&mut other, ts(5)),
            Err(DomainError::InvalidRequest(_))
        ));
        let restored = Review::restore(
            objective.id(),
            review.candidates().to_vec(),
            review.verifications().to_vec(),
            review.approvals().to_vec(),
        )
        .expect("restore");
        assert_eq!(restored, review);
        let mut duplicated = review.candidates().to_vec();
        duplicated.push(review.candidates()[0].clone());
        assert!(Review::restore(objective.id(), duplicated, Vec::new(), Vec::new()).is_err());
    }
}
