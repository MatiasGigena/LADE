use lade_core::DomainError;
use lade_core::domain::execution::ExecutionState;
use lade_core::domain::ids::{self as core_ids, GitAlgorithm, Uuid};
use lade_core::domain::inspection::{
    ArtifactDigest, ByteRange, InspectedVersion, MAX_TEXT_BYTES, Reference, RepoPath, Side,
};
use lade_core::domain::objectives::Lifecycle;
use lade_protocol::client::{self as wire, ErrorCode, Rejection, SafeAction, Side as WireSide};
use lade_protocol::types::{self as wire_types, Counter, Id};

use crate::adapters::git::{self, GitError, ObjectFormat};

pub fn wire_id(uuid: Uuid) -> Id {
    Id::parse(uuid.to_string()).expect("domain uuids are canonical v4")
}

pub fn parse_uuid(id: &Id) -> Result<Uuid, Rejection> {
    Uuid::parse(id.as_str()).map_err(|e| domain_rejection(&e))
}

pub fn counter(value: u64) -> Counter {
    Counter::new(value)
}

pub fn counter_u32(value: &Counter) -> Result<u32, Rejection> {
    u32::try_from(value.value()).map_err(|_| invalid("counter out of range"))
}

pub fn rejection(code: ErrorCode, action: SafeAction, detail: &str) -> Rejection {
    Rejection::new(code, action, detail)
}

pub fn invalid(detail: &str) -> Rejection {
    rejection(ErrorCode::InvalidRequest, SafeAction::None, detail)
}

pub fn not_found(what: &str) -> Rejection {
    rejection(
        ErrorCode::NotFound,
        SafeAction::Refresh,
        &format!("{what} not found"),
    )
}

pub fn conflict(detail: &str) -> Rejection {
    rejection(ErrorCode::Conflict, SafeAction::Refresh, detail)
}

pub fn durability(detail: &str) -> Rejection {
    rejection(
        ErrorCode::DurabilityFailure,
        SafeAction::QueryOperation,
        detail,
    )
}

pub fn domain_rejection(error: &DomainError) -> Rejection {
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
    Rejection::new(code, action, &error.detail())
}

pub fn git_rejection(error: &GitError) -> Rejection {
    match error {
        GitError::NotFound { what } => not_found(what),
        GitError::NotRepository | GitError::NotTopLevel | GitError::BareRepository => {
            invalid("path is not the top level of a non-bare Git repository")
        }
        GitError::UnsupportedObjectFormat | GitError::UnsupportedVersion { .. } => rejection(
            ErrorCode::UnsupportedContent,
            SafeAction::None,
            &error.to_string(),
        ),
        GitError::InvalidRefName | GitError::InvalidPath | GitError::InvalidId => {
            invalid(&error.to_string())
        }
        GitError::NotABlob => rejection(
            ErrorCode::UnsupportedContent,
            SafeAction::None,
            "path is not a regular file in this version",
        ),
        GitError::TargetMoved { .. } => rejection(
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
        other => rejection(
            ErrorCode::DurabilityFailure,
            SafeAction::Refresh,
            &format!("git operation failed: {}", other.code()),
        ),
    }
}

pub fn core_algorithm(format: ObjectFormat) -> GitAlgorithm {
    match format {
        ObjectFormat::Sha1 => GitAlgorithm::Sha1,
        ObjectFormat::Sha256 => GitAlgorithm::Sha256,
    }
}

pub fn adapter_format(algorithm: GitAlgorithm) -> ObjectFormat {
    match algorithm {
        GitAlgorithm::Sha1 => ObjectFormat::Sha1,
        GitAlgorithm::Sha256 => ObjectFormat::Sha256,
    }
}

pub fn core_git(id: &git::GitId) -> core_ids::GitId {
    core_ids::GitId::new(core_algorithm(id.format()), id.hex()).expect("adapter ids are full ids")
}

pub fn adapter_git(id: &core_ids::GitId) -> git::GitId {
    git::GitId::parse(adapter_format(id.algorithm()), id.hex()).expect("domain ids are full ids")
}

pub fn wire_git(id: &core_ids::GitId) -> wire_types::GitId {
    let algorithm = match id.algorithm() {
        GitAlgorithm::Sha1 => wire_types::GitAlgorithm::Sha1,
        GitAlgorithm::Sha256 => wire_types::GitAlgorithm::Sha256,
    };
    wire_types::GitId::new(algorithm, id.hex()).expect("domain ids satisfy the wire format")
}

pub fn from_wire_git(id: &wire_types::GitId) -> Result<core_ids::GitId, Rejection> {
    let algorithm = match id.algorithm() {
        wire_types::GitAlgorithm::Sha1 => GitAlgorithm::Sha1,
        wire_types::GitAlgorithm::Sha256 => GitAlgorithm::Sha256,
    };
    core_ids::GitId::new(algorithm, id.hex()).map_err(|e| domain_rejection(&e))
}

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
        byte_start: counter(range.start()),
        byte_end: counter(range.end()),
        selected_text: reference.selected_text().to_string(),
        line_hint: Some(counter(u64::from(reference.line_hint()))),
    }
}

pub fn core_reference(reference: &wire::Reference) -> Result<Reference, Rejection> {
    let domain = |e: DomainError| domain_rejection(&e);
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
        ArtifactDigest::parse(reference.artifact_digest.as_str()).map_err(domain)?,
        RepoPath::new(reference.path.as_bytes().to_vec()).map_err(domain)?,
        version,
        match reference.side {
            WireSide::Old => Side::Old,
            WireSide::New => Side::New,
        },
        ByteRange::new(reference.byte_start.value(), reference.byte_end.value()).map_err(domain)?,
        reference.selected_text.clone(),
        line_hint,
    )
    .map_err(domain)
}

pub fn repo_path(path: &wire_types::WirePath) -> Result<git::RepoPath, Rejection> {
    git::RepoPath::new(path.as_bytes().to_vec()).map_err(|_| invalid("invalid repository path"))
}

pub fn wire_path(bytes: &[u8]) -> wire_types::WirePath {
    let bounded = &bytes[..bytes.len().min(wire_types::MAX_PATH_BYTES)];
    wire_types::WirePath::from_bytes(bounded.to_vec())
        .unwrap_or_else(|_| wire_types::WirePath::from_bytes(b"?".to_vec()).expect("valid path"))
}

pub fn preview(text: &str, max: usize) -> String {
    wire_types::truncate_chars(text, max).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_codes_map_to_wire_codes() {
        let error = DomainError::StaleTarget;
        let rejection = domain_rejection(&error);
        assert_eq!(rejection.code, ErrorCode::StaleTarget);
        assert_eq!(rejection.action, SafeAction::Refresh);
    }
}
