//! Service notices, and the plain wording of rejection codes and their safe next actions.

use lade_protocol::client as wire;

use crate::presentation::state::model as view;

pub fn notice_level(l: wire::NoticeLevel) -> view::NoticeLevel {
    match l {
        wire::NoticeLevel::Info => view::NoticeLevel::Info,
        wire::NoticeLevel::Warning => view::NoticeLevel::Warning,
        wire::NoticeLevel::Error => view::NoticeLevel::Error,
    }
}

pub fn error_code(code: wire::ErrorCode) -> &'static str {
    use wire::ErrorCode as E;
    match code {
        E::Unauthorized => "unauthorized",
        E::StaleService => "stale_service",
        E::StaleAssociation => "stale_association",
        E::StaleExecution => "stale_execution",
        E::StaleReference => "stale_reference",
        E::StaleCandidate => "stale_candidate",
        E::StaleTarget => "stale_target",
        E::InvalidLifecycle => "invalid_lifecycle",
        E::UnsupportedContent => "unsupported_content",
        E::VersionMismatch => "version_mismatch",
        E::DurabilityFailure => "durability_failure",
        E::PermissionDenied => "permission_denied",
        E::LimitReached => "limit_reached",
        E::Interrupted => "interrupted",
        E::UnknownOperation => "unknown_operation",
        E::ReplayExpired => "replay_expired",
        E::DuplicateConflict => "duplicate_conflict",
        E::NotFound => "not_found",
        E::InvalidRequest => "invalid_request",
        E::Conflict => "conflict",
        E::ExecutionUnavailable => "execution_unavailable",
    }
}

pub fn advice(code: wire::ErrorCode, action: wire::SafeAction) -> Option<&'static str> {
    use wire::ErrorCode as E;
    let stale = matches!(
        code,
        E::StaleService
            | E::StaleAssociation
            | E::StaleExecution
            | E::StaleReference
            | E::StaleCandidate
            | E::StaleTarget
    );
    match action {
        wire::SafeAction::Refresh if stale => Some("Refresh and try again."),
        wire::SafeAction::InspectPrevious => Some("Inspect the previous version."),
        wire::SafeAction::RetryRead => Some("Try again."),
        _ => None,
    }
}

pub fn fallback_reason(code: wire::ErrorCode) -> &'static str {
    use wire::ErrorCode as E;
    match code {
        E::Unauthorized => "This window is not allowed to do that.",
        E::StaleService => "The service restarted.",
        E::StaleAssociation => "The Agent Session changed.",
        E::StaleExecution => "The agent was restarted.",
        E::StaleReference => "Some feedback no longer matches the current code.",
        E::StaleCandidate => "A newer candidate exists.",
        E::StaleTarget => "The Integration Target moved.",
        E::InvalidLifecycle => "That isn't possible in the Objective's current state.",
        E::UnsupportedContent => "This content isn't supported.",
        E::VersionMismatch => "This window and the service are different versions.",
        E::DurabilityFailure => "LADE couldn't record it, so nothing was changed.",
        E::PermissionDenied => "Permission was denied.",
        E::LimitReached => "A limit was reached.",
        E::Interrupted => "It was interrupted.",
        E::UnknownOperation => "The service has no record of it.",
        E::ReplayExpired => "Its outcome can no longer be checked.",
        E::DuplicateConflict => "It conflicts with an earlier request.",
        E::NotFound => "It no longer exists.",
        E::InvalidRequest => "The request was not valid.",
        E::Conflict => "Something changed in the meantime.",
        E::ExecutionUnavailable => "The agent isn't running.",
    }
}

pub fn notice(n: &wire::ServiceNotice) -> view::ServiceNotice {
    view::ServiceNotice {
        level: notice_level(n.level),
        code: n.code.as_str().to_owned(),
        message: n.message.clone(),
        scope: view::NoticeScope::General,
    }
}
