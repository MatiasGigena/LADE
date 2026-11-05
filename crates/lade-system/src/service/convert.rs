use lade_core::DomainError;
use lade_core::domain::execution::ExecutionState;
use lade_core::domain::feedback::{AcceptedInput, DeliveryState, Draft, DraftState};
use lade_core::domain::ids::{self as core_ids, GitAlgorithm, Uuid};
use lade_core::domain::inspection::{
    ArtifactDigest, ByteRange, InspectedVersion, MAX_TEXT_BYTES, Reference, RepoPath, Side,
};
use lade_core::domain::objectives::{Attention, Badge, Lifecycle};
use lade_core::domain::review::{
    Approval, ApprovalState, Candidate, CandidateStatus, Verification, VerificationOutcome,
};
use lade_protocol::client::{
    self as wire, ApprovalSummary, CandidateSummary, DeliveryState as WireDelivery,
    DraftState as WireDraftState, ErrorCode, InputSummary, Rejection, SafeAction, Side as WireSide,
    VerificationState, VerificationSummary,
};
use lade_protocol::types::{self as wire_types, Counter, DeliveryMode, Id};

use super::state::ObjState;
use crate::adapters::git::{self, GitError};
use crate::support::wire_ts;

#[must_use]
#[expect(
    clippy::expect_used,
    reason = "lade-protocol offers only a validating constructor for Id; the domain invariant \
              guarantees it accepts every domain UUID"
)]
pub fn wire_id(uuid: Uuid) -> Id {
    Id::parse(uuid.to_string()).expect("domain uuids are canonical v4")
}

/// The domain UUID of a wire identifier.
///
/// # Errors
///
/// Returns an `invalid_request` Rejection when `id` is not a lowercase v4 UUID.
pub fn parse_uuid(id: &Id) -> Result<Uuid, Rejection> {
    Uuid::parse(id.as_str()).map_err(domain_rejection)
}

/// A wire counter as a `u32`.
///
/// # Errors
///
/// Returns an `invalid_request` Rejection when the value does not fit.
pub fn counter_u32(value: &Counter) -> Result<u32, Rejection> {
    u32::try_from(value.value()).map_err(|_| invalid("counter out of range"))
}

#[must_use]
pub fn invalid(detail: &str) -> Rejection {
    Rejection::new(ErrorCode::InvalidRequest, SafeAction::None, detail)
}

#[must_use]
pub fn not_found(what: &str) -> Rejection {
    Rejection::new(
        ErrorCode::NotFound,
        SafeAction::Refresh,
        &format!("{what} not found"),
    )
}

#[must_use]
pub fn conflict(detail: &str) -> Rejection {
    Rejection::new(ErrorCode::Conflict, SafeAction::Refresh, detail)
}

#[must_use]
pub fn durability(detail: &str) -> Rejection {
    Rejection::new(
        ErrorCode::DurabilityFailure,
        SafeAction::QueryOperation,
        detail,
    )
}

#[must_use]
pub fn domain_rejection(error: DomainError) -> Rejection {
    let (code, action) = match error {
        DomainError::Unauthorized => (ErrorCode::Unauthorized, SafeAction::None),
        DomainError::StaleService => (ErrorCode::StaleService, SafeAction::Refresh),
        DomainError::StaleAssociation => (ErrorCode::StaleAssociation, SafeAction::Refresh),
        DomainError::StaleExecution => (ErrorCode::StaleExecution, SafeAction::Refresh),
        DomainError::StaleReference { .. } => (ErrorCode::StaleReference, SafeAction::Refresh),
        DomainError::StaleCandidate => (ErrorCode::StaleCandidate, SafeAction::Refresh),
        DomainError::StaleTarget => (ErrorCode::StaleTarget, SafeAction::Refresh),
        DomainError::InvalidLifecycle(_) => (ErrorCode::InvalidLifecycle, SafeAction::Refresh),
        DomainError::UnsupportedContent => (ErrorCode::UnsupportedContent, SafeAction::None),
        DomainError::DurabilityFailure(_) => {
            (ErrorCode::DurabilityFailure, SafeAction::QueryOperation)
        }
        DomainError::PermissionDenied => (ErrorCode::PermissionDenied, SafeAction::None),
        DomainError::LimitReached(_) => (ErrorCode::LimitReached, SafeAction::None),
        DomainError::Interrupted => (ErrorCode::Interrupted, SafeAction::Refresh),
        DomainError::UnknownOperation => (ErrorCode::UnknownOperation, SafeAction::None),
        DomainError::DuplicateConflict => (ErrorCode::DuplicateConflict, SafeAction::None),
        DomainError::NotFound(_) => (ErrorCode::NotFound, SafeAction::Refresh),
        DomainError::InvalidRequest(_) => (ErrorCode::InvalidRequest, SafeAction::None),
        DomainError::Conflict(_) => (ErrorCode::Conflict, SafeAction::Refresh),
        DomainError::ExecutionUnavailable => (
            ErrorCode::ExecutionUnavailable,
            SafeAction::ExplicitContinue,
        ),
    };
    let rejection = Rejection::new(code, action, &error.detail());
    match error {
        DomainError::StaleReference { drafts } => {
            rejection.with_stale_drafts(drafts.into_iter().map(|d| wire_id(d.uuid())).collect())
        }
        _ => rejection,
    }
}

pub fn git_rejection(error: GitError) -> Rejection {
    match error {
        GitError::NotFound { what } => not_found(what),
        GitError::NotRepository | GitError::NotTopLevel | GitError::BareRepository => {
            invalid("path is not the top level of a non-bare Git repository")
        }
        GitError::UnsupportedObjectFormat | GitError::UnsupportedVersion { .. } => Rejection::new(
            ErrorCode::UnsupportedContent,
            SafeAction::None,
            &error.to_string(),
        ),
        GitError::InvalidRefName | GitError::InvalidPath | GitError::InvalidId => {
            invalid(&error.to_string())
        }
        GitError::NotABlob => Rejection::new(
            ErrorCode::UnsupportedContent,
            SafeAction::None,
            "path is not a regular file in this version",
        ),
        GitError::TargetMoved { .. } => Rejection::new(
            ErrorCode::StaleTarget,
            SafeAction::Refresh,
            "integration target moved",
        ),
        GitError::TargetDirty { .. } => {
            conflict("integration target checkout has uncommitted changes; target left untouched")
        }
        GitError::TargetBusy { .. } => {
            conflict("integration target checkout is busy (operation in progress or locked)")
        }
        GitError::TargetWorktreeUnavailable { .. } => {
            conflict("integration target checkout is missing on disk")
        }
        GitError::TargetUntracked { paths, .. } => {
            let names: Vec<String> = paths.iter().take(8).map(git::RepoPath::display).collect();
            let more = if paths.len() > names.len() {
                ", …"
            } else {
                ""
            };
            let detail = if names.is_empty() {
                "integration target checkout cannot take the candidate (untracked files would be \
                 overwritten); target left untouched"
                    .to_string()
            } else {
                format!(
                    "untracked files in the target checkout would be overwritten: {}{more}; move \
                     or remove them — target left untouched",
                    names.join(", ")
                )
            };
            conflict(&preview(&detail, 512))
        }
        GitError::CheckoutFailed { reverted: true, .. } => {
            conflict("target checkout could not be updated; target ref restored")
        }
        GitError::CheckoutFailed {
            reverted: false, ..
        } => durability(
            "target checkout update failed and the ref could not be restored; inspect the target",
        ),
        GitError::StaleRef { .. } => conflict("reference changed concurrently"),
        GitError::WorktreeDirty => conflict("worktree has uncaptured changes"),
        other => Rejection::new(
            ErrorCode::DurabilityFailure,
            SafeAction::Refresh,
            &format!("git operation failed: {}", other.code()),
        ),
    }
}

/// The wire form of a domain object id.
///
/// # Panics
///
/// Never in practice: domain ids are full lowercase object ids, which the wire format accepts.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "lade-protocol offers only a validating constructor for GitId; domain ids satisfy it"
)]
pub fn wire_git(id: &core_ids::GitId) -> wire_types::GitId {
    let algorithm = match id.algorithm() {
        GitAlgorithm::Sha1 => wire_types::GitAlgorithm::Sha1,
        GitAlgorithm::Sha256 => wire_types::GitAlgorithm::Sha256,
    };
    wire_types::GitId::new(algorithm, id.hex()).expect("domain ids satisfy the wire format")
}

/// The domain form of a wire object id.
///
/// # Errors
///
/// Returns an `invalid_request` Rejection when it is not a full, non-zero lowercase object id.
pub fn from_wire_git(id: &wire_types::GitId) -> Result<core_ids::GitId, Rejection> {
    let algorithm = match id.algorithm() {
        wire_types::GitAlgorithm::Sha1 => GitAlgorithm::Sha1,
        wire_types::GitAlgorithm::Sha256 => GitAlgorithm::Sha256,
    };
    core_ids::GitId::new(algorithm, id.hex()).map_err(domain_rejection)
}

#[must_use]
pub fn wire_lifecycle(lifecycle: Lifecycle) -> wire::Lifecycle {
    match lifecycle {
        Lifecycle::Defined => wire::Lifecycle::Defined,
        Lifecycle::Active => wire::Lifecycle::Active,
        Lifecycle::Frozen => wire::Lifecycle::Frozen,
        Lifecycle::Approved => wire::Lifecycle::Approved,
        Lifecycle::Integrated => wire::Lifecycle::Integrated,
        Lifecycle::CompletedNoChanges => wire::Lifecycle::CompletedNoChanges,
        Lifecycle::Discarded => wire::Lifecycle::Discarded,
    }
}

#[must_use]
pub fn wire_attention(attention: Attention) -> wire::Attention {
    match attention {
        Attention::NeedsAttention => wire::Attention::NeedsAttention,
        Attention::ReadyForReview => wire::Attention::ReadyForReview,
        Attention::Working => wire::Attention::Working,
        Attention::Idle => wire::Attention::Idle,
        Attention::Archived => wire::Attention::Archived,
    }
}

/// The wire badge for a domain badge. `Failed` and `Cancelled` have none: the execution state
/// already says so, and a failure at a limit gets `LimitReached` from the summary instead.
#[must_use]
pub fn wire_badge(badge: Badge) -> Option<wire::Badge> {
    Some(match badge {
        Badge::Interrupted => wire::Badge::Interrupted,
        Badge::DeliveryUncertain => wire::Badge::DeliveryUncertain,
        Badge::StaleFeedback => wire::Badge::StaleFeedback,
        Badge::VerificationFailed => wire::Badge::VerificationFailed,
        Badge::IntegrationConflict => wire::Badge::IntegrationConflict,
        Badge::StaleTarget => wire::Badge::StaleTarget,
        Badge::CleanedUp => wire::Badge::CleanedUp,
        Badge::Failed | Badge::Cancelled => return None,
    })
}

#[must_use]
pub fn wire_execution_state(state: &ExecutionState) -> (wire::ExecutionState, Option<String>) {
    match state {
        ExecutionState::None => (wire::ExecutionState::None, None),
        ExecutionState::Starting => (wire::ExecutionState::Starting, None),
        ExecutionState::Running => (wire::ExecutionState::Running, None),
        ExecutionState::Idle => (wire::ExecutionState::Idle, None),
        ExecutionState::Stopping => (wire::ExecutionState::Stopping, None),
        ExecutionState::Cancelled { interrupted } => (
            wire::ExecutionState::Cancelled,
            interrupted.then(|| "interrupted".to_string()),
        ),
        ExecutionState::Failed { reason } => (
            wire::ExecutionState::Failed,
            Some(wire_types::truncate_chars(reason, 512).to_string()),
        ),
    }
}

/// The wire form of a domain Reference.
///
/// # Panics
///
/// Never in practice: a domain Reference holds a sha256 hex digest and a non-empty path within
/// the wire path bound, both checked when it was built.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "lade-protocol offers only validating constructors for Digest and WirePath; a \
              domain Reference already satisfies both"
)]
pub fn wire_reference(reference: &Reference) -> wire::Reference {
    let version = match reference.version() {
        InspectedVersion::Working { capture_id } => wire::ReferenceVersion::Working {
            capture_id: wire_id(capture_id.uuid()),
        },
        InspectedVersion::Candidate { candidate_id } => wire::ReferenceVersion::Candidate {
            candidate_id: wire_id(candidate_id.uuid()),
        },
    };
    let range = reference.byte_range();
    wire::Reference {
        artifact_digest: wire_types::Digest::parse(reference.artifact_digest().hex())
            .expect("domain digests are sha256 hex"),
        path: wire_types::WirePath::from_bytes(reference.path().as_bytes().to_vec())
            .expect("domain paths satisfy the wire bound"),
        version,
        side: match reference.side() {
            Side::Old => WireSide::Old,
            Side::New => WireSide::New,
        },
        byte_start: Counter::new(range.start()),
        byte_end: Counter::new(range.end()),
        selected_text: reference.selected_text().to_string(),
        line_hint: Some(Counter::new(u64::from(reference.line_hint()))),
    }
}

/// The domain form of a wire Reference.
///
/// # Errors
///
/// Returns an `invalid_request` Rejection when an identifier, the digest, the path, the byte range
/// or the line hint is invalid, or the selected text is larger than the domain bound.
pub fn core_reference(reference: &wire::Reference) -> Result<Reference, Rejection> {
    let version = match &reference.version {
        wire::ReferenceVersion::Working { capture_id } => InspectedVersion::Working {
            capture_id: core_ids::CaptureId::new(parse_uuid(capture_id)?),
        },
        wire::ReferenceVersion::Candidate { candidate_id } => InspectedVersion::Candidate {
            candidate_id: core_ids::CandidateId::new(parse_uuid(candidate_id)?),
        },
    };
    if reference.selected_text.len() > MAX_TEXT_BYTES {
        return Err(invalid("selected text too large"));
    }
    let line_hint = match &reference.line_hint {
        Some(hint) => counter_u32(hint)?.max(1),
        None => 1,
    };
    Reference::new(
        ArtifactDigest::parse(reference.artifact_digest.as_str()).map_err(domain_rejection)?,
        RepoPath::new(reference.path.as_bytes().to_vec()).map_err(domain_rejection)?,
        version,
        match reference.side {
            WireSide::Old => Side::Old,
            WireSide::New => Side::New,
        },
        ByteRange::new(reference.byte_start.value(), reference.byte_end.value())
            .map_err(domain_rejection)?,
        reference.selected_text.clone(),
        line_hint,
    )
    .map_err(domain_rejection)
}

/// The Git adapter form of a wire path.
///
/// # Errors
///
/// Returns an `invalid_request` Rejection when it is not a valid repository-relative path.
pub fn repo_path(path: &wire_types::WirePath) -> Result<git::RepoPath, Rejection> {
    git::RepoPath::new(path.as_bytes().to_vec()).map_err(|_| invalid("invalid repository path"))
}

/// The wire form of a repository path, cut to the wire path bound; an empty path shows as `?`.
///
/// # Panics
///
/// Never: the fallback `?` is a valid, non-empty path within the bound.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "the literal fallback path `?` always satisfies the wire path bound"
)]
pub fn wire_path(bytes: &[u8]) -> wire_types::WirePath {
    let bounded = &bytes[..bytes.len().min(wire_types::MAX_PATH_BYTES)];
    wire_types::WirePath::from_bytes(bounded.to_vec())
        .unwrap_or_else(|_| wire_types::WirePath::from_bytes(b"?".to_vec()).expect("valid path"))
}

#[must_use]
pub fn preview(text: &str, max: usize) -> String {
    wire_types::truncate_chars(text, max).to_string()
}

fn blocked_reason(reason: lade_core::domain::feedback::BlockedReason) -> &'static str {
    use lade_core::domain::feedback::BlockedReason;
    match reason {
        BlockedReason::ExecutionUnavailable => {
            "The agent isn't running. Resume it to deliver this."
        }
        BlockedReason::StaleAssociation => {
            "The Agent Session was replaced, so this was not delivered to it."
        }
        BlockedReason::StaleReference => {
            "The code this refers to changed, so it was not delivered."
        }
        BlockedReason::InvalidLifecycle => {
            "The Objective is no longer in progress, so this was not delivered."
        }
    }
}

#[must_use]
pub fn wire_mode(mode: lade_core::domain::feedback::InputMode) -> DeliveryMode {
    use lade_core::domain::feedback::InputMode;
    match mode {
        InputMode::Prompt => DeliveryMode::Prompt,
        InputMode::Steer => DeliveryMode::Steer,
        InputMode::FollowUp => DeliveryMode::FollowUp,
    }
}

#[must_use]
pub fn core_mode(mode: DeliveryMode) -> lade_core::domain::feedback::InputMode {
    use lade_core::domain::feedback::InputMode;
    match mode {
        DeliveryMode::Prompt => InputMode::Prompt,
        DeliveryMode::Steer => InputMode::Steer,
        DeliveryMode::FollowUp => InputMode::FollowUp,
    }
}

#[must_use]
pub fn wire_input(input: &AcceptedInput) -> InputSummary {
    let (delivery, blocked_reason) = match input.delivery {
        DeliveryState::Queued => (WireDelivery::Queued, None),
        DeliveryState::Dispatched { .. } => (WireDelivery::Dispatched, None),
        DeliveryState::Received { .. } => (WireDelivery::Received, None),
        DeliveryState::Uncertain { .. } => (WireDelivery::Uncertain, None),
        DeliveryState::Blocked { reason } => (
            WireDelivery::Blocked,
            Some(blocked_reason(reason).to_string()),
        ),
    };
    InputSummary {
        input_id: wire_id(input.id.uuid()),
        objective_id: wire_id(input.objective_id.uuid()),
        operation_id: wire_id(input.operation_id.uuid()),
        mode: wire_mode(input.mode),
        text_preview: preview(&input.rendered_text, 1024),
        accepted_at: wire_ts(input.accepted_at.unix_ms()),
        delivery,
        blocked_reason,
    }
}

#[must_use]
pub fn wire_draft(st: &ObjState, draft: &Draft) -> wire::Draft {
    let record = draft.record();
    let (state, input_id) = match record.state {
        DraftState::Unsent => (WireDraftState::Unsent, None),
        DraftState::Submitted { input_id } => {
            (WireDraftState::Submitted, Some(wire_id(input_id.uuid())))
        }
    };
    wire::Draft {
        draft_id: wire_id(record.id.uuid()),
        objective_id: wire_id(record.objective_id.uuid()),
        destination_session_id: wire_id(record.destination_session_id.uuid()),
        reference: wire_reference(&record.reference),
        body: record.body.clone(),
        revision: Counter::new(record.revision.value()),
        state,
        stale: draft.is_unsent() && st.draft_stale.get(&record.id).copied().unwrap_or(false),
        input_id,
    }
}

#[must_use]
pub fn wire_candidate(candidate: &Candidate) -> CandidateSummary {
    CandidateSummary {
        candidate_id: wire_id(candidate.id.uuid()),
        ordinal: Counter::new(u64::from(candidate.ordinal)),
        commit: wire_git(&candidate.commit),
        base_target_commit: wire_git(&candidate.base_target_commit),
        status: match candidate.status {
            CandidateStatus::Current => wire::CandidateStatus::Current,
            CandidateStatus::Historical => wire::CandidateStatus::Historical,
            CandidateStatus::Invalidated => wire::CandidateStatus::Invalidated,
        },
        files: Counter::new(u64::from(candidate.changes.files)),
        added: Counter::new(candidate.changes.added),
        removed: Counter::new(candidate.changes.removed),
        created_at: wire_ts(candidate.created_at.unix_ms()),
    }
}

pub fn wire_verification(st: &ObjState, verification: &Verification) -> VerificationSummary {
    super::projection::budget::budget_verification(VerificationSummary {
        verification_id: wire_id(verification.id.uuid()),
        objective_id: wire_id(st.objective().id().uuid()),
        candidate_id: wire_id(verification.candidate_id.uuid()),
        command: verification
            .argv
            .iter()
            .take(256)
            .map(|a| preview(a, 4096))
            .collect(),
        state: match verification.outcome {
            VerificationOutcome::Running => VerificationState::Running,
            VerificationOutcome::Passed => VerificationState::Passed,
            VerificationOutcome::Failed => VerificationState::Failed,
            VerificationOutcome::Interrupted => VerificationState::Interrupted,
            VerificationOutcome::Error => VerificationState::Error,
        },
        started_at: wire_ts(verification.started_at.unix_ms()),
        output_tail: st.tails.get(&verification.id).cloned().unwrap_or_default(),
        truncated: verification.truncated,
        provenance: wire::Provenance::ServiceObserved,
        exit_code: verification
            .exit_code
            .and_then(|c| u64::try_from(c).ok())
            .map(Counter::new),
        ended_at: verification.ended_at.map(|t| wire_ts(t.unix_ms())),
    })
}

#[must_use]
pub fn wire_approval(approval: &Approval) -> ApprovalSummary {
    ApprovalSummary {
        approval_id: wire_id(approval.id.uuid()),
        candidate_id: wire_id(approval.candidate_id.uuid()),
        target_commit: wire_git(&approval.target_commit),
        state: match approval.state {
            ApprovalState::Valid => wire::ApprovalState::Valid,
            ApprovalState::Consumed => wire::ApprovalState::Consumed,
            ApprovalState::Invalidated { .. } => wire::ApprovalState::Invalidated,
        },
        override_reason: approval
            .override_reason
            .as_ref()
            .map(|r| preview(r, 2048))
            .filter(|r| !r.is_empty()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_codes_map_to_wire_codes() {
        let error = DomainError::StaleTarget;
        let rejection = domain_rejection(error);
        assert_eq!(rejection.code, ErrorCode::StaleTarget);
        assert_eq!(rejection.action, SafeAction::Refresh);
    }
}
