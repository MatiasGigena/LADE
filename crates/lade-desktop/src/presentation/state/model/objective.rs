//! Objectives: their summaries and details, candidates, checks, approvals and the actions
//! the service offers on them.

use super::ids::{
    ApprovalId, CandidateId, GitId, ModelRef, ObjectiveId, RepoPath, SessionId, Timestamp,
    VerificationId, WorkspaceId,
};
use super::service::ReportCode;
use crate::presentation::state::agent_session::ThinkingLevel;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub path: RepoPath,
    pub name: String,
    pub default_branch: Option<String>,
    pub branches: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lifecycle {
    Defined,
    Active,
    Frozen,
    Approved,
    Integrated,
    CompletedNoChanges,
    Discarded,
}

impl ObjectiveSummary {
    pub fn is_archived(&self) -> bool {
        self.attention == Attention::Archived || self.lifecycle.is_archived()
    }

    pub fn badge_text(&self) -> String {
        self.badges
            .iter()
            .map(|b| b.label())
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

impl Lifecycle {
    pub fn is_archived(self) -> bool {
        matches!(
            self,
            Self::Integrated | Self::CompletedNoChanges | Self::Discarded
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExecutionState {
    None,
    Starting,
    Running,
    Idle,
    Stopping,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Execution {
    pub state: ExecutionState,
    pub generation: u64,
    pub reason: Option<String>,
}

impl Execution {
    #[cfg(test)]
    pub fn new(state: ExecutionState, generation: u64) -> Self {
        Self {
            state,
            generation,
            reason: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[expect(
    clippy::enum_variant_names,
    reason = "mirrors the protocol's `needs_attention` attention value"
)]
pub enum Attention {
    NeedsAttention,
    ReadyForReview,
    Working,
    Idle,
    Archived,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Badge {
    Interrupted,
    DeliveryUncertain,
    StaleFeedback,
    VerificationFailed,
    VerificationMissing,
    IntegrationConflict,
    StaleTarget,
    LimitReached,
    CleanedUp,
    UnsentDrafts,
}

impl Badge {
    pub fn label(self) -> &'static str {
        match self {
            Self::Interrupted => "interrupted",
            Self::DeliveryUncertain => "delivery uncertain",
            Self::StaleFeedback => "stale feedback",
            Self::VerificationFailed => "check failed",
            Self::VerificationMissing => "checks missing",
            Self::IntegrationConflict => "integration conflict",
            Self::StaleTarget => "target moved",
            Self::LimitReached => "limit reached",
            Self::CleanedUp => "cleaned up",
            Self::UnsentDrafts => "unsent drafts",
        }
    }
}

impl Attention {
    pub const BOARD_ORDER: [Attention; 4] = [
        Attention::NeedsAttention,
        Attention::ReadyForReview,
        Attention::Working,
        Attention::Idle,
    ];
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectiveSummary {
    pub objective_id: ObjectiveId,
    pub workspace_id: WorkspaceId,
    pub title: String,
    pub outcome: String,
    pub lifecycle: Lifecycle,
    pub execution: Execution,
    pub attention: Attention,
    pub badges: Vec<Badge>,
    pub updated_at: Timestamp,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Intent {
    pub outcome: String,
    pub constraints: String,
    pub acceptance: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetRef {
    pub branch: String,
    pub commit: GitId,
}

impl TargetRef {
    pub fn label(&self) -> String {
        format!("{}@{}", self.branch, self.commit.short())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    pub session_id: SessionId,
    pub association_revision: u64,
    pub execution_generation: u64,
    pub model: ModelRef,
    pub thinking: Option<ThinkingLevel>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CandidateStatus {
    Current,
    Historical,
    Invalidated,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateSummary {
    pub candidate_id: CandidateId,
    pub ordinal: u64,
    pub commit: GitId,
    pub base_target_commit: GitId,
    pub status: CandidateStatus,
    pub files: u64,
    pub added: u64,
    pub removed: u64,
    pub created_at: Timestamp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VerificationState {
    Running,
    Passed,
    Failed,
    Interrupted,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Provenance {
    ServiceObserved,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerificationSummary {
    pub verification_id: VerificationId,
    pub objective_id: ObjectiveId,
    pub candidate_id: CandidateId,
    pub command: Vec<String>,
    pub state: VerificationState,
    pub exit_code: Option<u64>,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub output_tail: String,
    pub truncated: bool,
    pub provenance: Provenance,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalState {
    Valid,
    Consumed,
    Invalidated,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalSummary {
    pub approval_id: ApprovalId,
    pub candidate_id: CandidateId,
    pub target_commit: GitId,
    pub override_reason: Option<String>,
    pub state: ApprovalState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ChangeCounts {
    pub files: u64,
    pub added: u64,
    pub removed: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ActionName {
    ObjectiveUpdateIntent,
    ExecutionStart,
    ExecutionCancel,
    ExecutionResume,
    MessageSend,
    FeedbackSubmit,
    ReviewFreeze,
    ReviewReturn,
    ReviewVerify,
    ReviewApprove,
    ReviewRefresh,
    ReviewIntegrate,
    ObjectiveComplete,
    ObjectiveDiscard,
    ObjectiveCleanup,
    ObjectiveDeleteHistory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvailableAction {
    pub action: ActionName,
    pub enabled: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionAvailability {
    Enabled,
    Disabled { reason: String },
    NotOffered,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalOpenAction {
    pub enabled: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ActionSurface {
    Intent,
    Agent,
    Session,
    Send,
    Drafts,
    Feedback,
    Review,
    Checks,
    Closing,
    Terminal,
    Dialog,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub code: ReportCode,
    pub message: String,
    /// Technical details for display only.
    pub details: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectiveDetail {
    pub summary: ObjectiveSummary,
    pub intent: Intent,
    pub intent_revision: u64,
    pub integration_target: TargetRef,
    pub start_commit: GitId,
    pub branch: String,
    pub session: Option<SessionInfo>,
    pub candidates: Vec<CandidateSummary>,
    pub approval: Option<ApprovalSummary>,
    pub verifications: Vec<VerificationSummary>,
    pub change_counts: ChangeCounts,
    pub unsent_drafts: u64,
    pub available_actions: Vec<AvailableAction>,
    pub terminal_open: Option<TerminalOpenAction>,
    pub history_deleted: bool,
}

impl ObjectiveDetail {
    pub fn availability(&self, action: ActionName) -> ActionAvailability {
        match self.available_actions.iter().find(|a| a.action == action) {
            None => ActionAvailability::NotOffered,
            Some(a) if a.enabled => ActionAvailability::Enabled,
            Some(a) => ActionAvailability::Disabled {
                reason: a
                    .reason
                    .clone()
                    .unwrap_or_else(|| "Unavailable — the service gave no reason".to_owned()),
            },
        }
    }

    pub fn terminal_availability(&self) -> ActionAvailability {
        match &self.terminal_open {
            None => ActionAvailability::NotOffered,
            Some(a) if a.enabled => ActionAvailability::Enabled,
            Some(a) => ActionAvailability::Disabled {
                reason: a
                    .reason
                    .clone()
                    .unwrap_or_else(|| "Unavailable — the service gave no reason".to_owned()),
            },
        }
    }

    pub fn terminal_note(&self) -> Option<&str> {
        self.terminal_open
            .as_ref()
            .filter(|a| a.enabled)
            .and_then(|a| a.reason.as_deref())
    }

    pub fn current_candidate(&self) -> Option<&CandidateSummary> {
        self.candidates
            .iter()
            .find(|c| c.status == CandidateStatus::Current)
    }
}
