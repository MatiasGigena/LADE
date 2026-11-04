//! Verification runs observed by the service against a Candidate, and the evidence they add up to.

use super::Review;
use crate::domain::ids::{CandidateId, Timestamp, VerificationId};
use crate::domain::inspection::{ArtifactDigest, MAX_TEXT_BYTES};
use crate::domain::objectives::{Lifecycle, Objective};
use crate::error::DomainError;

pub const MAX_ARGV_ITEMS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerificationOutcome {
    Running,
    Passed,
    Failed,
    Interrupted,
    Error,
}

impl VerificationOutcome {
    #[must_use]
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
pub enum Evidence {
    Missing,
    Running,
    Failed,
    Passed,
}

impl Review {
    #[must_use]
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

    #[must_use]
    pub fn evidence(&self, candidate_id: CandidateId) -> Evidence {
        if self.verifications.iter().any(|verification| {
            verification.candidate_id == candidate_id
                && verification.outcome == VerificationOutcome::Running
        }) {
            return Evidence::Running;
        }
        let latest = self.latest_verifications(candidate_id);
        if latest.is_empty() {
            Evidence::Missing
        } else if latest
            .iter()
            .all(|v| v.outcome == VerificationOutcome::Passed)
        {
            Evidence::Passed
        } else {
            Evidence::Failed
        }
    }

    #[must_use]
    pub fn verification_failed(&self) -> bool {
        self.current_candidate()
            .is_some_and(|candidate| self.evidence(candidate.id) == Evidence::Failed)
    }

    pub(super) fn check_no_running_verification(&self) -> Result<(), DomainError> {
        if self
            .verifications
            .iter()
            .any(|v| v.outcome == VerificationOutcome::Running)
        {
            return Err(DomainError::Conflict(
                "Wait for the running check to finish.",
            ));
        }
        Ok(())
    }

    /// Starts a check of the current candidate of a frozen Objective.
    ///
    /// # Errors
    ///
    /// - `DomainError::InvalidRequest` for another Objective or an invalid command line.
    /// - `DomainError::InvalidLifecycle` unless the Objective is frozen.
    /// - `DomainError::NotFound` or `DomainError::StaleCandidate` when the candidate is not
    ///   current.
    /// - `DomainError::Conflict` when the verification id was used before.
    pub fn start_verification(
        &mut self,
        objective: &Objective,
        new: NewVerification,
        now: Timestamp,
    ) -> Result<(), DomainError> {
        self.check_objective(objective)?;
        if objective.lifecycle() != Lifecycle::Frozen {
            return Err(DomainError::InvalidLifecycle(
                "Checks run only on a frozen candidate that isn't approved yet.",
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
            return Err(DomainError::InvalidRequest("Enter a valid check command."));
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

    /// Records how a running check ended and returns its outcome.
    ///
    /// # Errors
    ///
    /// - `DomainError::NotFound` for an unknown verification.
    /// - `DomainError::Conflict` when it already finished.
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
}
