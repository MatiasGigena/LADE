//! Review of an Objective's frozen work: Candidates, the verification evidence recorded for them,
//! approval against the Integration Target, and integration itself.

mod approval;
mod candidate;
#[cfg(test)]
mod tests;
mod verification;

pub use approval::{
    Approval, ApprovalState, ApproveCandidate, IntegrationPlan, InvalidationReason,
};
pub use candidate::{
    Candidate, CandidateStatus, ChangeCounts, NewCandidate, RefreshOutcome, RefreshResult,
};
pub use verification::{
    Evidence, MAX_ARGV_ITEMS, NewVerification, Provenance, Verification, VerificationEnd,
    VerificationOutcome,
};

use crate::domain::ids::{CandidateId, ObjectiveId, Timestamp};
use crate::domain::objectives::{Lifecycle, Objective};
use crate::error::DomainError;

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

    fn check_objective(&self, objective: &Objective) -> Result<(), DomainError> {
        if objective.id() != self.objective_id {
            return Err(DomainError::InvalidRequest(
                "review belongs to another objective",
            ));
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
            return Err(DomainError::Conflict(
                "There are changes. Freeze them for review, or discard the Objective.",
            ));
        }
        self.check_no_running_verification()?;
        objective.transition(&[Lifecycle::CompletedNoChanges], now)
    }
}
