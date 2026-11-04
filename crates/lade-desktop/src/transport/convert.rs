use lade_protocol::client as wire;
use lade_protocol::types::{self, Counter, Id, WirePath};

use crate::presentation::state::model as view;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invalid(pub String);

impl Invalid {
    fn new(what: &str) -> Self {
        Self(format!("invalid {what}"))
    }
}

pub fn timestamp(t: &types::Timestamp) -> view::Timestamp {
    view::Timestamp(t.unix_ms())
}

pub fn counter(c: &Counter) -> u64 {
    c.value()
}

pub fn id(value: &str, what: &str) -> Result<Id, Invalid> {
    Id::parse(value).map_err(|_| Invalid::new(what))
}

pub fn git_id(g: &types::GitId) -> view::GitId {
    view::GitId {
        algorithm: match g.algorithm() {
            types::GitAlgorithm::Sha1 => view::GitAlgorithm::Sha1,
            types::GitAlgorithm::Sha256 => view::GitAlgorithm::Sha256,
        },
        hex: g.hex().to_owned(),
    }
}

pub fn to_git_id(g: &view::GitId) -> Result<types::GitId, Invalid> {
    let algorithm = match g.algorithm {
        view::GitAlgorithm::Sha1 => types::GitAlgorithm::Sha1,
        view::GitAlgorithm::Sha256 => types::GitAlgorithm::Sha256,
    };
    types::GitId::new(algorithm, g.hex.clone()).map_err(|_| Invalid::new("commit id"))
}

pub fn path(p: &WirePath) -> view::RepoPath {
    view::RepoPath {
        bytes: p.as_bytes().to_vec(),
        display: p.display().to_owned(),
    }
}

pub fn to_path(p: &view::RepoPath) -> Result<WirePath, Invalid> {
    WirePath::from_bytes(p.bytes.clone()).map_err(|_| Invalid::new("path"))
}

pub fn model(m: &types::Model) -> view::ModelRef {
    view::ModelRef::new(m.provider.clone(), m.id.clone())
}

pub fn to_model(m: &view::ModelRef) -> types::Model {
    types::Model {
        provider: m.provider.clone(),
        id: m.id.clone(),
    }
}

pub fn delivery_mode(m: types::DeliveryMode) -> view::DeliveryMode {
    match m {
        types::DeliveryMode::Prompt => view::DeliveryMode::Prompt,
        types::DeliveryMode::Steer => view::DeliveryMode::Steer,
        types::DeliveryMode::FollowUp => view::DeliveryMode::FollowUp,
    }
}

pub fn to_delivery_mode(m: view::DeliveryMode) -> types::DeliveryMode {
    match m {
        view::DeliveryMode::Prompt => types::DeliveryMode::Prompt,
        view::DeliveryMode::Steer => types::DeliveryMode::Steer,
        view::DeliveryMode::FollowUp => types::DeliveryMode::FollowUp,
    }
}

pub fn side(s: wire::Side) -> view::Side {
    match s {
        wire::Side::Old => view::Side::Old,
        wire::Side::New => view::Side::New,
    }
}

pub fn to_side(s: view::Side) -> wire::Side {
    match s {
        view::Side::Old => wire::Side::Old,
        view::Side::New => wire::Side::New,
    }
}

pub fn to_scope(s: view::SearchScope) -> wire::SearchScope {
    match s {
        view::SearchScope::Objectives => wire::SearchScope::Objectives,
        view::SearchScope::Files => wire::SearchScope::Files,
        view::SearchScope::Content => wire::SearchScope::Content,
    }
}

pub fn to_basis(b: &view::Basis) -> Result<wire::Basis, Invalid> {
    Ok(match b {
        view::Basis::Working => wire::Basis::Working,
        view::Basis::Candidate { candidate_id } => wire::Basis::Candidate {
            candidate_id: id(candidate_id.as_str(), "candidate id")?,
        },
        view::Basis::Since {
            from_candidate_id,
            to_candidate_id,
        } => wire::Basis::Since {
            from_candidate_id: id(from_candidate_id.as_str(), "candidate id")?,
            to_candidate_id: id(to_candidate_id.as_str(), "candidate id")?,
        },
    })
}

fn lifecycle(l: wire::Lifecycle) -> view::Lifecycle {
    match l {
        wire::Lifecycle::Defined => view::Lifecycle::Defined,
        wire::Lifecycle::Active => view::Lifecycle::Active,
        wire::Lifecycle::Frozen => view::Lifecycle::Frozen,
        wire::Lifecycle::Approved => view::Lifecycle::Approved,
        wire::Lifecycle::Integrated => view::Lifecycle::Integrated,
        wire::Lifecycle::CompletedNoChanges => view::Lifecycle::CompletedNoChanges,
        wire::Lifecycle::Discarded => view::Lifecycle::Discarded,
    }
}

fn execution(e: &wire::Execution) -> view::Execution {
    view::Execution {
        state: match e.state {
            wire::ExecutionState::None => view::ExecutionState::None,
            wire::ExecutionState::Starting => view::ExecutionState::Starting,
            wire::ExecutionState::Running => view::ExecutionState::Running,
            wire::ExecutionState::Idle => view::ExecutionState::Idle,
            wire::ExecutionState::Stopping => view::ExecutionState::Stopping,
            wire::ExecutionState::Cancelled => view::ExecutionState::Cancelled,
            wire::ExecutionState::Failed => view::ExecutionState::Failed,
        },
        generation: counter(&e.generation),
        reason: e.reason.clone(),
    }
}

fn attention(a: wire::Attention) -> view::Attention {
    match a {
        wire::Attention::NeedsAttention => view::Attention::NeedsAttention,
        wire::Attention::ReadyForReview => view::Attention::ReadyForReview,
        wire::Attention::Working => view::Attention::Working,
        wire::Attention::Idle => view::Attention::Idle,
        wire::Attention::Archived => view::Attention::Archived,
    }
}

fn badge(b: wire::Badge) -> view::Badge {
    match b {
        wire::Badge::Interrupted => view::Badge::Interrupted,
        wire::Badge::DeliveryUncertain => view::Badge::DeliveryUncertain,
        wire::Badge::StaleFeedback => view::Badge::StaleFeedback,
        wire::Badge::VerificationFailed => view::Badge::VerificationFailed,
        wire::Badge::VerificationMissing => view::Badge::VerificationMissing,
        wire::Badge::IntegrationConflict => view::Badge::IntegrationConflict,
        wire::Badge::StaleTarget => view::Badge::StaleTarget,
        wire::Badge::LimitReached => view::Badge::LimitReached,
        wire::Badge::CleanedUp => view::Badge::CleanedUp,
        wire::Badge::UnsentDrafts => view::Badge::UnsentDrafts,
    }
}

fn action_name(a: wire::ActionName) -> Option<view::ActionName> {
    Some(match a {
        wire::ActionName::ObjectiveUpdateIntent => view::ActionName::ObjectiveUpdateIntent,
        wire::ActionName::ExecutionStart => view::ActionName::ExecutionStart,
        wire::ActionName::ExecutionCancel => view::ActionName::ExecutionCancel,
        wire::ActionName::ExecutionResume => view::ActionName::ExecutionResume,
        wire::ActionName::MessageSend => view::ActionName::MessageSend,
        wire::ActionName::FeedbackSubmit => view::ActionName::FeedbackSubmit,
        wire::ActionName::ReviewFreeze => view::ActionName::ReviewFreeze,
        wire::ActionName::ReviewReturn => view::ActionName::ReviewReturn,
        wire::ActionName::ReviewVerify => view::ActionName::ReviewVerify,
        wire::ActionName::ReviewApprove => view::ActionName::ReviewApprove,
        wire::ActionName::ReviewRefresh => view::ActionName::ReviewRefresh,
        wire::ActionName::ReviewIntegrate => view::ActionName::ReviewIntegrate,
        wire::ActionName::ObjectiveComplete => view::ActionName::ObjectiveComplete,
        wire::ActionName::ObjectiveDiscard => view::ActionName::ObjectiveDiscard,
        wire::ActionName::ObjectiveCleanup => view::ActionName::ObjectiveCleanup,
        wire::ActionName::ObjectiveDeleteHistory => view::ActionName::ObjectiveDeleteHistory,
        wire::ActionName::TerminalOpen => return None,
    })
}

fn activity_role(r: types::ActivityRole) -> view::ActivityRole {
    match r {
        types::ActivityRole::User => view::ActivityRole::User,
        types::ActivityRole::Assistant => view::ActivityRole::Assistant,
        types::ActivityRole::Thinking => view::ActivityRole::Thinking,
        types::ActivityRole::Tool => view::ActivityRole::Tool,
        types::ActivityRole::System => view::ActivityRole::System,
    }
}

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

pub fn settings(s: &wire::Settings) -> view::Settings {
    view::Settings {
        default_model: s.default_model.as_ref().map(model),
        verification_commands: s.verification_commands.clone(),
    }
}

pub fn workspace(w: &wire::Workspace) -> view::Workspace {
    view::Workspace {
        workspace_id: view::WorkspaceId::new(w.workspace_id.as_str()),
        path: path(&w.path),
        name: w.name.clone(),
        default_branch: w.default_branch.clone(),
        branches: w.branches.clone(),
    }
}

pub fn summary(s: &wire::ObjectiveSummary) -> view::ObjectiveSummary {
    view::ObjectiveSummary {
        objective_id: view::ObjectiveId::new(s.objective_id.as_str()),
        workspace_id: view::WorkspaceId::new(s.workspace_id.as_str()),
        title: s.title.clone(),
        outcome: s.outcome.clone(),
        lifecycle: lifecycle(s.lifecycle),
        execution: execution(&s.execution),
        attention: attention(s.attention),
        badges: s.badges.iter().copied().map(badge).collect(),
        updated_at: timestamp(&s.updated_at),
        revision: counter(&s.revision),
    }
}

fn target(t: &wire::TargetRef) -> view::TargetRef {
    view::TargetRef {
        branch: t.branch.clone(),
        commit: git_id(&t.commit),
    }
}

pub fn candidate(c: &wire::CandidateSummary) -> view::CandidateSummary {
    view::CandidateSummary {
        candidate_id: view::CandidateId::new(c.candidate_id.as_str()),
        ordinal: counter(&c.ordinal),
        commit: git_id(&c.commit),
        base_target_commit: git_id(&c.base_target_commit),
        status: match c.status {
            wire::CandidateStatus::Current => view::CandidateStatus::Current,
            wire::CandidateStatus::Historical => view::CandidateStatus::Historical,
            wire::CandidateStatus::Invalidated => view::CandidateStatus::Invalidated,
        },
        files: counter(&c.files),
        added: counter(&c.added),
        removed: counter(&c.removed),
        created_at: timestamp(&c.created_at),
    }
}

pub fn verification(v: &wire::VerificationSummary) -> view::VerificationSummary {
    view::VerificationSummary {
        verification_id: view::VerificationId::new(v.verification_id.as_str()),
        objective_id: view::ObjectiveId::new(v.objective_id.as_str()),
        candidate_id: view::CandidateId::new(v.candidate_id.as_str()),
        command: v.command.clone(),
        state: match v.state {
            wire::VerificationState::Running => view::VerificationState::Running,
            wire::VerificationState::Passed => view::VerificationState::Passed,
            wire::VerificationState::Failed => view::VerificationState::Failed,
            wire::VerificationState::Interrupted => view::VerificationState::Interrupted,
            wire::VerificationState::Error => view::VerificationState::Error,
        },
        exit_code: v.exit_code.as_ref().map(counter),
        started_at: timestamp(&v.started_at),
        ended_at: v.ended_at.as_ref().map(timestamp),
        output_tail: v.output_tail.clone(),
        truncated: v.truncated,
        provenance: match v.provenance {
            wire::Provenance::ServiceObserved => view::Provenance::ServiceObserved,
        },
    }
}

pub fn approval(a: &wire::ApprovalSummary) -> view::ApprovalSummary {
    view::ApprovalSummary {
        approval_id: view::ApprovalId::new(a.approval_id.as_str()),
        candidate_id: view::CandidateId::new(a.candidate_id.as_str()),
        target_commit: git_id(&a.target_commit),
        override_reason: a.override_reason.clone(),
        state: match a.state {
            wire::ApprovalState::Valid => view::ApprovalState::Valid,
            wire::ApprovalState::Consumed => view::ApprovalState::Consumed,
            wire::ApprovalState::Invalidated => view::ApprovalState::Invalidated,
        },
    }
}

pub fn detail(d: &wire::ObjectiveDetail) -> view::ObjectiveDetail {
    view::ObjectiveDetail {
        summary: summary(&d.summary()),
        intent: view::Intent {
            outcome: d.intent.outcome.clone(),
            constraints: d.intent.constraints.clone(),
            acceptance: d.intent.acceptance.clone(),
        },
        intent_revision: counter(&d.intent_revision),
        integration_target: target(&d.integration_target),
        start_commit: git_id(&d.start_commit),
        branch: d.branch.clone(),
        session: d.session.as_ref().map(|s| view::SessionInfo {
            session_id: view::SessionId::new(s.session_id.as_str()),
            association_revision: counter(&s.association_revision),
            execution_generation: counter(&s.execution_generation),
            model: model(&s.model),
            thinking: s.thinking.map(super::agent_session::thinking),
        }),
        candidates: d.candidates.iter().map(candidate).collect(),
        approval: d.approval.as_ref().map(approval),
        verifications: d.verifications.iter().map(verification).collect(),
        change_counts: view::ChangeCounts {
            files: counter(&d.change_counts.files),
            added: counter(&d.change_counts.added),
            removed: counter(&d.change_counts.removed),
        },
        unsent_drafts: counter(&d.unsent_drafts),
        available_actions: d
            .available_actions
            .iter()
            .filter_map(|a| {
                Some(view::AvailableAction {
                    action: action_name(a.action)?,
                    enabled: a.enabled,
                    reason: a.reason.clone(),
                })
            })
            .collect(),
        terminal_open: d
            .available_actions
            .iter()
            .find(|a| a.action == wire::ActionName::TerminalOpen)
            .map(|a| view::TerminalOpenAction {
                enabled: a.enabled,
                reason: a.reason.clone(),
            }),
        history_deleted: d.history_deleted == Some(true),
    }
}

pub fn activity_item(i: &wire::ActivityItem) -> view::ActivityItem {
    view::ActivityItem {
        item_id: view::ActivityItemId::new(i.item_id.as_str()),
        seq: counter(&i.seq),
        at: timestamp(&i.at),
        role: activity_role(i.role),
        title: i.title.clone(),
        text: i.text.clone(),
        tool_name: i.tool_name.clone(),
        is_error: i.is_error,
        truncated: i.truncated,
    }
}

fn changed_file(f: &wire::ChangedFile) -> view::ChangedFile {
    view::ChangedFile {
        path: path(&f.path),
        old_path: f.old_path.as_ref().map(path),
        status: match f.status {
            wire::ChangeStatus::Added => view::ChangeStatus::Added,
            wire::ChangeStatus::Modified => view::ChangeStatus::Modified,
            wire::ChangeStatus::Deleted => view::ChangeStatus::Deleted,
            wire::ChangeStatus::Renamed => view::ChangeStatus::Renamed,
            wire::ChangeStatus::TypeChanged => view::ChangeStatus::TypeChanged,
        },
        added: counter(&f.added),
        removed: counter(&f.removed),
        binary: f.binary,
        filtered: f.filtered == Some(true),
    }
}

pub fn change_list(
    objective_id: &view::ObjectiveId,
    requested: &view::Basis,
    l: &wire::ChangeList,
) -> view::ChangeList {
    view::ChangeList {
        objective_id: objective_id.clone(),
        basis: requested.clone(),
        basis_label: l.basis_label.clone(),
        target: target(&l.target),
        capture_id: l
            .capture_id
            .as_ref()
            .map(|c| view::CaptureId::new(c.as_str())),
        candidate_id: l
            .candidate_id
            .as_ref()
            .map(|c| view::CandidateId::new(c.as_str())),
        files: l.files.iter().map(changed_file).collect(),
        truncated: l.truncated,
        total_added: counter(&l.total_added),
        total_removed: counter(&l.total_removed),
        notices: l
            .notices
            .iter()
            .flatten()
            .map(|n| view::ContentNotice {
                kind: match n.kind {
                    wire::ContentNoticeKind::NestedRepository => {
                        view::ContentNoticeKind::NestedRepository
                    }
                    wire::ContentNoticeKind::FilteredContent => {
                        view::ContentNoticeKind::FilteredContent
                    }
                },
                message: n.message.clone(),
                path: n.path.as_ref().map(path),
            })
            .collect(),
    }
}

fn digest(d: &types::Digest) -> view::ArtifactDigest {
    view::ArtifactDigest::new(d.as_str())
}

pub fn file_diff(
    objective_id: &view::ObjectiveId,
    requested: &view::Basis,
    file: &view::RepoPath,
    d: &wire::FileDiff,
) -> view::FileDiff {
    view::FileDiff {
        objective_id: objective_id.clone(),
        basis: requested.clone(),
        path: file.clone(),
        old_digest: d.old_digest.as_ref().map(digest),
        new_digest: d.new_digest.as_ref().map(digest),
        binary: d.binary,
        hunks: d
            .hunks
            .iter()
            .map(|h| view::DiffHunk {
                old_start: counter(&h.old_start),
                old_lines: counter(&h.old_lines),
                new_start: counter(&h.new_start),
                new_lines: counter(&h.new_lines),
                lines: h
                    .lines
                    .iter()
                    .map(|l| view::DiffLine {
                        no_newline: l.no_newline,
                        kind: match l.kind {
                            wire::DiffLineKind::Context => view::DiffLineKind::Context,
                            wire::DiffLineKind::Add => view::DiffLineKind::Add,
                            wire::DiffLineKind::Del => view::DiffLineKind::Del,
                        },
                        old_no: l.old_no.as_ref().map(counter),
                        new_no: l.new_no.as_ref().map(counter),
                        text: l.text.clone(),
                        new_byte_start: l.new_byte_start.as_ref().map(counter),
                        old_byte_start: l.old_byte_start.as_ref().map(counter),
                    })
                    .collect(),
            })
            .collect(),
        truncated: d.truncated,
    }
}

pub fn file_page(
    objective_id: &view::ObjectiveId,
    requested: &view::Basis,
    file: &view::RepoPath,
    requested_side: view::Side,
    c: &wire::FileContent,
) -> view::FilePage {
    view::FilePage {
        objective_id: objective_id.clone(),
        basis: requested.clone(),
        path: file.clone(),
        side: requested_side,
        artifact_digest: digest(&c.artifact_digest),
        total_bytes: counter(&c.total_bytes),
        offset: counter(&c.offset),
        length: counter(&c.length),
        encoding: match c.encoding {
            wire::FileEncoding::Utf8 => view::FileEncoding::Utf8,
            wire::FileEncoding::Binary => view::FileEncoding::Binary,
        },
        text: c.text.clone(),
        eof: c.eof,
        version_label: c.version_label.clone(),
        chunked: c.chunked == Some(true),
    }
}

fn reference_version(v: &wire::ReferenceVersion) -> view::ReferenceVersion {
    match v {
        wire::ReferenceVersion::Working { capture_id } => view::ReferenceVersion::Working {
            capture_id: view::CaptureId::new(capture_id.as_str()),
        },
        wire::ReferenceVersion::Candidate { candidate_id } => view::ReferenceVersion::Candidate {
            candidate_id: view::CandidateId::new(candidate_id.as_str()),
        },
    }
}

pub fn reference(r: &wire::Reference) -> view::Reference {
    view::Reference {
        artifact_digest: digest(&r.artifact_digest),
        path: path(&r.path),
        version: reference_version(&r.version),
        side: side(r.side),
        byte_start: counter(&r.byte_start),
        byte_end: counter(&r.byte_end),
        selected_text: r.selected_text.clone(),
        line_hint: r.line_hint.as_ref().map(counter),
    }
}

pub fn to_reference(r: &view::Reference) -> Result<wire::Reference, Invalid> {
    Ok(wire::Reference {
        artifact_digest: types::Digest::parse(r.artifact_digest.as_str())
            .map_err(|_| Invalid::new("artifact digest"))?,
        path: to_path(&r.path)?,
        version: match &r.version {
            view::ReferenceVersion::Working { capture_id } => wire::ReferenceVersion::Working {
                capture_id: id(capture_id.as_str(), "capture id")?,
            },
            view::ReferenceVersion::Candidate { candidate_id } => {
                wire::ReferenceVersion::Candidate {
                    candidate_id: id(candidate_id.as_str(), "candidate id")?,
                }
            }
        },
        side: to_side(r.side),
        byte_start: Counter::new(r.byte_start),
        byte_end: Counter::new(r.byte_end),
        selected_text: r.selected_text.clone(),
        line_hint: r.line_hint.map(Counter::new),
    })
}

pub fn draft(d: &wire::Draft) -> view::Draft {
    view::Draft {
        draft_id: view::DraftId::new(d.draft_id.as_str()),
        objective_id: view::ObjectiveId::new(d.objective_id.as_str()),
        destination_session_id: view::SessionId::new(d.destination_session_id.as_str()),
        reference: reference(&d.reference),
        body: d.body.clone(),
        revision: counter(&d.revision),
        state: match d.state {
            wire::DraftState::Unsent => view::DraftState::Unsent,
            wire::DraftState::Submitted => view::DraftState::Submitted,
        },
        stale: d.stale,
        input_id: d.input_id.as_ref().map(|i| view::InputId::new(i.as_str())),
    }
}

pub fn input(i: &wire::InputSummary) -> view::InputSummary {
    view::InputSummary {
        input_id: view::InputId::new(i.input_id.as_str()),
        objective_id: view::ObjectiveId::new(i.objective_id.as_str()),
        operation_id: view::OperationId::new(i.operation_id.as_str()),
        mode: delivery_mode(i.mode),
        text_preview: i.text_preview.clone(),
        accepted_at: timestamp(&i.accepted_at),
        delivery: match i.delivery {
            wire::DeliveryState::Queued => view::DeliveryState::Queued,
            wire::DeliveryState::Dispatched => view::DeliveryState::Dispatched,
            wire::DeliveryState::Received => view::DeliveryState::Received,
            wire::DeliveryState::Uncertain => view::DeliveryState::Uncertain,
            wire::DeliveryState::Blocked => view::DeliveryState::Blocked,
        },
        blocked_reason: i.blocked_reason.clone(),
    }
}

pub fn search_result(r: &wire::SearchResult) -> view::SearchResult {
    view::SearchResult {
        kind: match r.kind {
            wire::SearchResultKind::Objective => view::SearchResultKind::Objective,
            wire::SearchResultKind::File => view::SearchResultKind::File,
            wire::SearchResultKind::Content => view::SearchResultKind::Content,
        },
        objective_id: r
            .objective_id
            .as_ref()
            .map(|o| view::ObjectiveId::new(o.as_str())),
        path: r.path.as_ref().map(path),
        line: r.line.as_ref().map(counter),
        preview: r.preview.clone(),
        byte_start: r.byte_start.as_ref().map(counter),
        byte_end: r.byte_end.as_ref().map(counter),
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

pub fn terminal_end(c: &wire::TerminalClosed) -> crate::presentation::terminal::TerminalEnd {
    use crate::presentation::terminal::TerminalEnd as E;
    match c.reason {
        wire::TerminalCloseReason::Exited => E::Exited {
            code: c.exit_code.as_ref().map(Counter::value),
            signal: c.signal.as_ref().map(Counter::value),
        },
        wire::TerminalCloseReason::Closed => E::Closed,
        wire::TerminalCloseReason::ReviewFreeze => E::ReviewFreeze,
        wire::TerminalCloseReason::Lifecycle => E::Lifecycle,
        wire::TerminalCloseReason::Failed => E::Failed,
    }
}
