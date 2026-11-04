//! Objectives: summaries, details, candidates, checks, approvals and settings.

use lade_protocol::client as wire;
use lade_protocol::types::Counter;

use super::{git_id, model, path, timestamp};
use crate::presentation::state::model as view;
use crate::transport::agent_session;

pub(super) fn lifecycle(l: wire::Lifecycle) -> view::Lifecycle {
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

pub(super) fn execution(e: &wire::Execution) -> view::Execution {
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
        generation: e.generation.value(),
        reason: e.reason.clone(),
    }
}

pub(super) fn attention(a: wire::Attention) -> view::Attention {
    match a {
        wire::Attention::NeedsAttention => view::Attention::NeedsAttention,
        wire::Attention::ReadyForReview => view::Attention::ReadyForReview,
        wire::Attention::Working => view::Attention::Working,
        wire::Attention::Idle => view::Attention::Idle,
        wire::Attention::Archived => view::Attention::Archived,
    }
}

pub(super) fn badge(b: wire::Badge) -> view::Badge {
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

pub(super) fn action_name(a: wire::ActionName) -> Option<view::ActionName> {
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
        revision: s.revision.value(),
    }
}

pub(super) fn target(t: &wire::TargetRef) -> view::TargetRef {
    view::TargetRef {
        branch: t.branch.clone(),
        commit: git_id(&t.commit),
    }
}

pub fn candidate(c: &wire::CandidateSummary) -> view::CandidateSummary {
    view::CandidateSummary {
        candidate_id: view::CandidateId::new(c.candidate_id.as_str()),
        ordinal: c.ordinal.value(),
        commit: git_id(&c.commit),
        base_target_commit: git_id(&c.base_target_commit),
        status: match c.status {
            wire::CandidateStatus::Current => view::CandidateStatus::Current,
            wire::CandidateStatus::Historical => view::CandidateStatus::Historical,
            wire::CandidateStatus::Invalidated => view::CandidateStatus::Invalidated,
        },
        files: c.files.value(),
        added: c.added.value(),
        removed: c.removed.value(),
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
        exit_code: v.exit_code.as_ref().map(Counter::value),
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
        intent_revision: d.intent_revision.value(),
        integration_target: target(&d.integration_target),
        start_commit: git_id(&d.start_commit),
        branch: d.branch.clone(),
        session: d.session.as_ref().map(|s| view::SessionInfo {
            session_id: view::SessionId::new(s.session_id.as_str()),
            association_revision: s.association_revision.value(),
            execution_generation: s.execution_generation.value(),
            model: model(&s.model),
            thinking: s.thinking.map(agent_session::thinking),
        }),
        candidates: d.candidates.iter().map(candidate).collect(),
        approval: d.approval.as_ref().map(approval),
        verifications: d.verifications.iter().map(verification).collect(),
        change_counts: view::ChangeCounts {
            files: d.change_counts.files.value(),
            added: d.change_counts.added.value(),
            removed: d.change_counts.removed.value(),
        },
        unsent_drafts: d.unsent_drafts.value(),
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
