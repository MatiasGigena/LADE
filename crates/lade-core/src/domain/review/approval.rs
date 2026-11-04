//! Approval of the current Candidate against the Integration Target, its invalidation, and the
//! integration plan an approval authorizes.

use super::{Evidence, Review};
use crate::domain::ids::{ApprovalId, CandidateId, GitId, Timestamp};
use crate::domain::inspection::MAX_TEXT_BYTES;
use crate::domain::objectives::{Lifecycle, Objective};
use crate::error::DomainError;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntegrationPlan {
    pub approval_id: ApprovalId,
    pub candidate_id: CandidateId,
    pub candidate_commit: GitId,
    pub candidate_tree: GitId,
    pub expected_target: GitId,
}

impl Review {
    pub fn valid_approval(&self) -> Option<&Approval> {
        self.approvals
            .iter()
            .find(|approval| approval.state == ApprovalState::Valid)
    }

    pub fn target_stale(&self, current_target: &GitId) -> bool {
        self.current_candidate()
            .is_some_and(|candidate| candidate.base_target_commit != *current_target)
    }

    pub(super) fn invalidate_valid_approval(&mut self, reason: InvalidationReason) {
        for approval in &mut self.approvals {
            if approval.state == ApprovalState::Valid {
                approval.state = ApprovalState::Invalidated { reason };
            }
        }
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
                "Freeze the work for review before approving.",
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
            return Err(DomainError::LimitReached(
                "The override reason is too long.",
            ));
        }
        match self.evidence(candidate.id) {
            Evidence::Passed => {}
            Evidence::Running => {
                return Err(DomainError::Conflict(
                    "Wait for the running check to finish.",
                ));
            }
            Evidence::Missing | Evidence::Failed if override_reason.is_some() => {}
            Evidence::Missing | Evidence::Failed => {
                return Err(DomainError::InvalidRequest(
                    "Checks are missing or failed. Give a reason to approve anyway.",
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

    pub fn check_integration(
        &self,
        objective: &Objective,
        approval_id: ApprovalId,
        current_target: &GitId,
    ) -> Result<IntegrationPlan, DomainError> {
        self.check_objective(objective)?;
        if objective.lifecycle() != Lifecycle::Approved {
            return Err(DomainError::InvalidLifecycle(
                "Approve the candidate before integrating.",
            ));
        }
        let approval = self
            .approvals
            .iter()
            .find(|approval| approval.id == approval_id)
            .ok_or(DomainError::NotFound("approval"))?;
        if approval.state != ApprovalState::Valid {
            return Err(DomainError::Conflict(
                "The approval is no longer valid. Approve again.",
            ));
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
}
