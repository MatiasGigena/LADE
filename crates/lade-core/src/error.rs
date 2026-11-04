use core::fmt;

use crate::domain::ids::DraftId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DomainError {
    Unauthorized,
    StaleService,
    StaleAssociation,
    StaleExecution,
    StaleReference { drafts: Vec<DraftId> },
    StaleCandidate,
    StaleTarget,
    InvalidLifecycle(&'static str),
    UnsupportedContent,
    DurabilityFailure(&'static str),
    PermissionDenied,
    LimitReached(&'static str),
    Interrupted,
    UnknownOperation,
    DuplicateConflict,
    NotFound(&'static str),
    InvalidRequest(&'static str),
    Conflict(&'static str),
    ExecutionUnavailable,
}

impl DomainError {
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

    /// Human-readable reason, empty when the code says it all. Structured data such as the
    /// stale draft ids stays on the variant.
    pub fn detail(&self) -> String {
        match self {
            Self::StaleReference { drafts } => match drafts.len() {
                1 => "a feedback draft refers to code that changed".to_owned(),
                n => format!("{n} feedback drafts refer to code that changed"),
            },
            Self::NotFound(what) => format!("{what} not found"),
            Self::InvalidLifecycle(reason)
            | Self::DurabilityFailure(reason)
            | Self::LimitReached(reason)
            | Self::InvalidRequest(reason)
            | Self::Conflict(reason) => (*reason).to_owned(),
            _ => String::new(),
        }
    }
}

impl fmt::Display for DomainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let detail = self.detail();
        if detail.is_empty() {
            formatter.write_str(self.code())
        } else {
            write!(formatter, "{}: {detail}", self.code())
        }
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
    fn details_are_prose_and_never_repeat_codes_or_ids() {
        let error = DomainError::StaleReference {
            drafts: vec![draft_id(1), draft_id(2)],
        };
        assert_eq!(
            error.detail(),
            "2 feedback drafts refer to code that changed"
        );
        assert!(!error.detail().contains(&draft_id(1).to_string()));
        assert_eq!(DomainError::NotFound("draft").detail(), "draft not found");
        assert_eq!(DomainError::StaleTarget.detail(), "");
        assert_eq!(DomainError::StaleTarget.to_string(), "stale_target");
        assert_eq!(
            DomainError::Conflict("revision mismatch").to_string(),
            "conflict: revision mismatch"
        );
    }
}
