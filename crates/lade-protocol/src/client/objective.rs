//! Objective summaries and details, with their execution, candidates, checks and approval.

use super::{
    ActionName, ApprovalState, Attention, Badge, CandidateStatus, ExecutionState, Lifecycle,
    VerificationState,
};
use crate::types::{ActivityRole, Counter, GitId, Id, Model, ThinkingLevel, Timestamp, WirePath};
use serde::{Deserialize, Serialize};

wire_struct!(Empty {});
wire_struct!(Intent {
    outcome: String,
    constraints: String,
    acceptance: String,
});
wire_struct!(Settings {
    verification_commands: Vec<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_model: Option<Model>,
});
wire_struct!(Workspace {
    workspace_id: Id,
    path: WirePath,
    name: String,
    branches: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_branch: Option<String>,
});
wire_struct!(TargetRef {
    branch: String,
    commit: GitId,
});
wire_struct!(Execution {
    state: ExecutionState,
    generation: Counter,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
});
wire_struct!(ObjectiveSummary {
    objective_id: Id,
    workspace_id: Id,
    title: String,
    outcome: String,
    lifecycle: Lifecycle,
    execution: Execution,
    attention: Attention,
    badges: Vec<Badge>,
    updated_at: Timestamp,
    revision: Counter,
});
wire_struct!(SessionInfo {
    session_id: Id,
    association_revision: Counter,
    execution_generation: Counter,
    model: Model,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingLevel>,
});
wire_struct!(ChangeCounts {
    files: Counter,
    added: Counter,
    removed: Counter,
});
wire_struct!(AvailableAction {
    action: ActionName,
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
});
wire_struct!(CandidateSummary {
    candidate_id: Id,
    ordinal: Counter,
    commit: GitId,
    base_target_commit: GitId,
    status: CandidateStatus,
    files: Counter,
    added: Counter,
    removed: Counter,
    created_at: Timestamp,
});
wire_struct!(VerificationSummary {
    verification_id: Id,
    objective_id: Id,
    candidate_id: Id,
    command: Vec<String>,
    state: VerificationState,
    started_at: Timestamp,
    output_tail: String,
    truncated: bool,
    provenance: Provenance,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<Counter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ended_at: Option<Timestamp>,
});
wire_enum!(Provenance { ServiceObserved });
wire_struct!(ApprovalSummary {
    approval_id: Id,
    candidate_id: Id,
    target_commit: GitId,
    state: ApprovalState,
    #[serde(skip_serializing_if = "Option::is_none")]
    override_reason: Option<String>,
});
wire_struct!(ObjectiveDetail {
    objective_id: Id,
    workspace_id: Id,
    title: String,
    outcome: String,
    lifecycle: Lifecycle,
    execution: Execution,
    attention: Attention,
    badges: Vec<Badge>,
    updated_at: Timestamp,
    revision: Counter,
    intent: Intent,
    intent_revision: Counter,
    integration_target: TargetRef,
    start_commit: GitId,
    branch: String,
    candidates: Vec<CandidateSummary>,
    verifications: Vec<VerificationSummary>,
    change_counts: ChangeCounts,
    unsent_drafts: Counter,
    available_actions: Vec<AvailableAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<SessionInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    approval: Option<ApprovalSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    history_deleted: Option<bool>,
});
impl ObjectiveDetail {
    pub fn summary(&self) -> ObjectiveSummary {
        ObjectiveSummary {
            objective_id: self.objective_id.clone(),
            workspace_id: self.workspace_id.clone(),
            title: self.title.clone(),
            outcome: self.outcome.clone(),
            lifecycle: self.lifecycle,
            execution: self.execution.clone(),
            attention: self.attention,
            badges: self.badges.clone(),
            updated_at: self.updated_at.clone(),
            revision: self.revision.clone(),
        }
    }
}
wire_struct!(ActivityItem {
    item_id: Id,
    seq: Counter,
    at: Timestamp,
    role: ActivityRole,
    title: String,
    text: String,
    is_error: bool,
    truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_name: Option<String>,
});
