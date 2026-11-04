//! Candidates: the frozen, content-addressed snapshots of an Objective's changes, and how they are
//! frozen, returned for revision, invalidated and refreshed onto a moved Integration Target.

use super::{InvalidationReason, Review};
use crate::domain::ids::{CandidateId, GitId, ObjectiveId, Timestamp};
use crate::domain::inspection::ArtifactDigest;
use crate::domain::objectives::{Lifecycle, Objective};
use crate::error::DomainError;

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
    #[must_use]
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

impl Review {
    pub(super) fn current_named(
        &self,
        candidate_id: CandidateId,
    ) -> Result<&Candidate, DomainError> {
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
                "There are no changes to freeze. Complete without changes instead.",
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

    pub(super) fn retire_current(&mut self) {
        for candidate in &mut self.candidates {
            if candidate.status != CandidateStatus::Historical {
                candidate.status = CandidateStatus::Historical;
            }
        }
    }

    /// Freezes the stopped work of an active Objective as the next current candidate.
    ///
    /// # Errors
    ///
    /// - `DomainError::InvalidRequest` for another Objective or an empty change set.
    /// - `DomainError::InvalidLifecycle` unless the Objective is active with its agent stopped.
    /// - `DomainError::Conflict` when the candidate id was used before.
    /// - `DomainError::LimitReached` when the candidate ordinal or the Objective's revision is
    ///   exhausted.
    pub fn freeze(
        &mut self,
        objective: &mut Objective,
        new: NewCandidate,
        now: Timestamp,
    ) -> Result<CandidateId, DomainError> {
        self.check_objective(objective)?;
        if objective.lifecycle() != Lifecycle::Active {
            return Err(DomainError::InvalidLifecycle(
                "Only an Objective in progress can be frozen for review.",
            ));
        }
        let ordinal = self.validate_new_candidate(&new)?;
        objective.transition(&[Lifecycle::Frozen], now)?;
        Ok(self.push_candidate(new, ordinal, now))
    }

    /// Returns the latest candidate for more work, invalidating its approval.
    ///
    /// # Errors
    ///
    /// - `DomainError::InvalidRequest` for another Objective.
    /// - `DomainError::InvalidLifecycle` unless the Objective is frozen or approved.
    /// - `DomainError::NotFound` when the candidate does not exist.
    /// - `DomainError::StaleCandidate` when it is not the latest live candidate.
    /// - `DomainError::LimitReached` when the Objective's revision is exhausted.
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
            _ => {
                return Err(DomainError::InvalidLifecycle(
                    "There is no frozen candidate to return.",
                ));
            }
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

    /// Invalidates the current candidate after its content changed unexpectedly.
    ///
    /// # Errors
    ///
    /// - `DomainError::InvalidRequest` for another Objective.
    /// - `DomainError::InvalidLifecycle` unless the Objective is frozen or approved.
    /// - `DomainError::NotFound` when there is no current candidate.
    /// - `DomainError::LimitReached` when the Objective's revision is exhausted.
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

    /// Applies a refresh onto the moved Integration Target: a rebuilt candidate replaces the
    /// current one, and a conflict returns the Objective for revision.
    ///
    /// # Errors
    ///
    /// - `DomainError::InvalidRequest` for another Objective or an empty rebuilt change set.
    /// - `DomainError::InvalidLifecycle` unless the Objective is frozen or approved.
    /// - `DomainError::NotFound` or `DomainError::StaleCandidate` when `candidate_id` is not
    ///   current.
    /// - `DomainError::Conflict` when the target has not moved or the rebuilt candidate id was used
    ///   before.
    /// - `DomainError::StaleTarget` when the rebuild is based on another target commit.
    /// - `DomainError::LimitReached` when the ordinal or the Objective's revision is exhausted.
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
                    "Only a frozen candidate can be refreshed.",
                ));
            }
        };
        let candidate = self.current_named(candidate_id)?;
        if candidate.base_target_commit == *current_target {
            return Err(DomainError::Conflict(
                "The Integration Target hasn't moved, so there is nothing to refresh.",
            ));
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
}

#[cfg(test)]
mod tests;
