//! Typed domain rejections. Every variant maps onto exactly one protocol `ErrorCode`.

use core::fmt;

use crate::domain::ids::DraftId;

/// A rejected domain transition or use case.
///
/// The protocol codes `version_mismatch` and `replay_expired` are transport/admission concerns and are never
/// produced by the domain, so they have no variant here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DomainError {
    Unauthorized,
    StaleService,
    /// The destination session or its association revision is not the current one.
    StaleAssociation,
    /// A worker fact or command names an execution generation that is not current.
    StaleExecution,
    /// At least one feedback item no longer matches the current content; the whole batch is blocked.
    StaleReference {
        drafts: Vec<DraftId>,
    },
    /// The named candidate is not the current candidate.
    StaleCandidate,
    /// The Integration Target moved away from the commit a candidate or approval is bound to.
    StaleTarget,
    InvalidLifecycle(&'static str),
    UnsupportedContent,
    /// Stored state is corrupt or could not be recorded; nothing was accepted.
    DurabilityFailure(&'static str),
    PermissionDenied,
    LimitReached(&'static str),
    Interrupted,
    UnknownOperation,
    DuplicateConflict,
    NotFound(&'static str),
    InvalidRequest(&'static str),
    /// Optimistic revision mismatch or another state conflict.
    Conflict(&'static str),
    ExecutionUnavailable,
}

impl DomainError {
    /// Stable snake_case protocol error code.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::StaleService => "stale_service",
            Self::StaleAssociation => "stale_association",
            Self::StaleExecution => "stale_execution",
            Self::StaleReference { .. } => "stale_reference",
            Self::StaleCandidate => "stale_candidate",
            Self::StaleTarget => "stale_target",
            Self::InvalidLifecycle(_) => "invalid_lifecycle",
            Self::UnsupportedContent => "unsupported_content",
            Self::DurabilityFailure(_) => "durability_failure",
            Self::PermissionDenied => "permission_denied",
            Self::LimitReached(_) => "limit_reached",
            Self::Interrupted => "interrupted",
            Self::UnknownOperation => "unknown_operation",
            Self::DuplicateConflict => "duplicate_conflict",
            Self::NotFound(_) => "not_found",
            Self::InvalidRequest(_) => "invalid_request",
            Self::Conflict(_) => "conflict",
            Self::ExecutionUnavailable => "execution_unavailable",
        }
    }

    /// Short, source-free explanation suitable for a reply `detail` (always far below 512 characters for
    /// static reasons; stale draft lists are the caller's to bound).
    pub fn detail(&self) -> String {
        match self {
            Self::StaleReference { drafts } => {
                let ids: Vec<String> = drafts.iter().map(ToString::to_string).collect();
                format!("stale drafts: {}", ids.join(","))
            }
            Self::InvalidLifecycle(reason)
            | Self::DurabilityFailure(reason)
            | Self::LimitReached(reason)
            | Self::NotFound(reason)
            | Self::InvalidRequest(reason)
            | Self::Conflict(reason) => (*reason).to_string(),
            other => other.code().to_string(),
        }
    }
}

impl fmt::Display for DomainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code(), self.detail())
    }
}

impl core::error::Error for DomainError {}

#[cfg(test)]
mod tests {
    use super::DomainError;
    use crate::testing::draft_id;

    #[test]
    fn codes_are_stable_and_unique() {
        let all = [
            DomainError::Unauthorized,
            DomainError::StaleService,
            DomainError::StaleAssociation,
            DomainError::StaleExecution,
            DomainError::StaleReference { drafts: Vec::new() },
            DomainError::StaleCandidate,
            DomainError::StaleTarget,
            DomainError::InvalidLifecycle("x"),
            DomainError::UnsupportedContent,
            DomainError::DurabilityFailure("x"),
            DomainError::PermissionDenied,
            DomainError::LimitReached("x"),
            DomainError::Interrupted,
            DomainError::UnknownOperation,
            DomainError::DuplicateConflict,
            DomainError::NotFound("x"),
            DomainError::InvalidRequest("x"),
            DomainError::Conflict("x"),
            DomainError::ExecutionUnavailable,
        ];
        let codes: Vec<&str> = all.iter().map(DomainError::code).collect();
        assert_eq!(
            codes,
            [
                "unauthorized",
                "stale_service",
                "stale_association",
                "stale_execution",
                "stale_reference",
                "stale_candidate",
                "stale_target",
                "invalid_lifecycle",
                "unsupported_content",
                "durability_failure",
                "permission_denied",
                "limit_reached",
                "interrupted",
                "unknown_operation",
                "duplicate_conflict",
                "not_found",
                "invalid_request",
                "conflict",
                "execution_unavailable",
            ]
        );
    }

    #[test]
    fn stale_reference_detail_names_drafts() {
        let error = DomainError::StaleReference {
            drafts: vec![draft_id(1), draft_id(2)],
        };
        let detail = error.detail();
        assert!(detail.starts_with("stale drafts: "));
        assert!(detail.contains(&draft_id(1).to_string()));
        assert!(detail.contains(&draft_id(2).to_string()));
        assert_eq!(
            DomainError::Conflict("revision mismatch").to_string(),
            "conflict: revision mismatch"
        );
    }
}
