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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::review::fixtures::{approve_cmd, check, frozen, verify};
    use crate::testing::{candidate_id, digest, ts, verification_id};

    #[test]
    fn a_newer_passing_run_does_not_hide_an_older_running_check() {
        let (mut objective, mut review, target) = frozen();
        review
            .start_verification(&objective, check(1, &["cargo", "test"]), ts(11))
            .expect("first check starts");
        verify(
            &mut review,
            &objective,
            2,
            &["cargo", "test"],
            VerificationEnd::Exited { code: 0 },
        );
        assert_eq!(review.evidence(candidate_id(1)), Evidence::Running);
        let before = objective.clone();
        assert_eq!(
            review.approve(
                &mut objective,
                approve_cmd(1, 1, &target, Some("override")),
                &target,
                ts(13),
            ),
            Err(DomainError::Conflict(
                "Wait for the running check to finish."
            ))
        );
        assert_eq!(objective, before);
        assert!(review.approvals().is_empty());
        review
            .finish_verification(
                verification_id(1),
                VerificationEnd::Exited { code: 1 },
                None,
                false,
                ts(14),
            )
            .expect("older check finishes");
        assert_eq!(review.evidence(candidate_id(1)), Evidence::Passed);
        review
            .approve(
                &mut objective,
                approve_cmd(1, 1, &target, None),
                &target,
                ts(15),
            )
            .expect("latest completed run permits approval");
    }

    #[test]
    fn command_lines_and_labels_are_validated_before_a_check_starts() {
        let (objective, mut review, _) = frozen();
        let too_many: Vec<&str> = vec!["x"; MAX_ARGV_ITEMS + 1];
        let long_arg = "a".repeat(MAX_TEXT_BYTES + 1);
        for argv in [
            &[][..],
            &[""],
            &["make", "a\0b"],
            &too_many,
            &["make", long_arg.as_str()],
        ] {
            assert_eq!(
                review.start_verification(&objective, check(1, argv), ts(11)),
                Err(DomainError::InvalidRequest("Enter a valid check command.")),
                "{:?}",
                argv.len()
            );
        }
        let mut long_label = check(1, &["make"]);
        long_label.cwd_label = "c".repeat(MAX_TEXT_BYTES + 1);
        assert!(
            review
                .start_verification(&objective, long_label, ts(11))
                .is_err()
        );
        let full: Vec<&str> = vec!["x"; MAX_ARGV_ITEMS];
        review
            .start_verification(&objective, check(1, &full), ts(11))
            .expect("at the item limit");
        assert_eq!(review.verifications().len(), 1);
    }

    #[test]
    fn checks_need_the_current_candidate_and_a_fresh_id() {
        let (objective, mut review, _) = frozen();
        let mut other = check(1, &["make"]);
        other.candidate_id = candidate_id(9);
        assert_eq!(
            review.start_verification(&objective, other, ts(11)),
            Err(DomainError::NotFound("candidate"))
        );
        review
            .start_verification(&objective, check(1, &["make"]), ts(11))
            .expect("start");
        assert_eq!(
            review.start_verification(&objective, check(1, &["make"]), ts(12)),
            Err(DomainError::Conflict("verification id reused"))
        );
        let started = &review.verifications()[0];
        assert_eq!(
            (started.outcome, started.started_at, started.ended_at),
            (VerificationOutcome::Running, ts(11), None)
        );
        assert_eq!(started.provenance, Provenance::ServiceObserved);
    }

    #[test]
    fn each_way_a_check_ends_maps_to_one_outcome() {
        let (objective, mut review, _) = frozen();
        for (n, end, outcome, exit_code) in [
            (
                1,
                VerificationEnd::Exited { code: 0 },
                VerificationOutcome::Passed,
                Some(0),
            ),
            (
                2,
                VerificationEnd::Exited { code: -9 },
                VerificationOutcome::Failed,
                Some(-9),
            ),
            (
                3,
                VerificationEnd::Interrupted,
                VerificationOutcome::Interrupted,
                None,
            ),
            (4, VerificationEnd::Error, VerificationOutcome::Error, None),
        ] {
            review
                .start_verification(&objective, check(n, &["make"]), ts(11))
                .expect("start");
            assert_eq!(
                review.finish_verification(verification_id(n), end, Some(digest(n)), true, ts(12)),
                Ok(outcome)
            );
            let finished = review.verifications().last().expect("verification");
            assert_eq!(
                (finished.exit_code, finished.ended_at, finished.truncated),
                (exit_code, Some(ts(12)), true)
            );
            assert_eq!(finished.output_artifact, Some(digest(n)));
        }
        assert_eq!(
            review.finish_verification(
                verification_id(9),
                VerificationEnd::Error,
                None,
                false,
                ts(13)
            ),
            Err(DomainError::NotFound("verification"))
        );
    }

    #[test]
    fn evidence_is_the_latest_run_of_each_command_for_that_candidate() {
        let (objective, mut review, _) = frozen();
        assert_eq!(review.evidence(candidate_id(1)), Evidence::Missing);
        assert!(!review.verification_failed());
        verify(
            &mut review,
            &objective,
            1,
            &["lint"],
            VerificationEnd::Exited { code: 0 },
        );
        verify(
            &mut review,
            &objective,
            2,
            &["test"],
            VerificationEnd::Exited { code: 3 },
        );
        verify(
            &mut review,
            &objective,
            3,
            &["lint"],
            VerificationEnd::Error,
        );
        let latest: Vec<VerificationId> = review
            .latest_verifications(candidate_id(1))
            .iter()
            .map(|verification| verification.id)
            .collect();
        assert_eq!(latest, [verification_id(3), verification_id(2)]);
        assert_eq!(review.evidence(candidate_id(1)), Evidence::Failed);
        assert!(review.verification_failed());
        assert_eq!(review.evidence(candidate_id(2)), Evidence::Missing);
        assert!(review.latest_verifications(candidate_id(2)).is_empty());
    }

    #[test]
    fn outcome_codes_are_stable() {
        assert_eq!(
            [
                VerificationOutcome::Running,
                VerificationOutcome::Passed,
                VerificationOutcome::Failed,
                VerificationOutcome::Interrupted,
                VerificationOutcome::Error,
            ]
            .map(VerificationOutcome::code),
            ["running", "passed", "failed", "interrupted", "error"]
        );
    }
}
