//! Request payloads: read queries and mutating commands.

use super::{Basis, DraftRevision, Intent, Reference, SearchScope, Side, Start, StopReason};
use crate::types::{
    Counter, DeliveryMode, GitId, Id, Model, PageLimit, ReadLimit, ThinkingLevel, Timestamp,
    WirePath,
};
use serde::{Deserialize, Serialize};

wire_struct!(ObjectiveSelector { objective_id: Id });
wire_struct!(DraftCursor {
    created_at: Timestamp,
    draft_id: Id,
});
wire_struct!(DraftsListQuery {
    objective_id: Id,
    limit: PageLimit,
    #[serde(skip_serializing_if = "Option::is_none")]
    after: Option<DraftCursor>,
});
wire_struct!(InputsListQuery {
    objective_id: Id,
    limit: PageLimit,
    #[serde(skip_serializing_if = "Option::is_none")]
    after: Option<Counter>,
});
wire_struct!(ActivityPageQuery {
    objective_id: Id,
    limit: PageLimit,
    #[serde(skip_serializing_if = "Option::is_none")]
    before: Option<Counter>,
});
wire_struct!(ChangesListQuery {
    objective_id: Id,
    basis: Basis,
});
wire_struct!(FileReadQuery {
    objective_id: Id,
    basis: Basis,
    path: WirePath,
    side: Side,
    offset: Counter,
    limit: ReadLimit,
});
wire_struct!(DiffGetQuery {
    objective_id: Id,
    basis: Basis,
    path: WirePath,
});
wire_struct!(SearchQuery {
    workspace_id: Id,
    scope: SearchScope,
    query: String,
    limit: PageLimit,
    #[serde(skip_serializing_if = "Option::is_none")]
    objective_id: Option<Id>,
    #[serde(skip_serializing_if = "Option::is_none")]
    basis: Option<Basis>,
});
wire_struct!(OperationGetQuery { operation_id: Id });
wire_struct!(WorkspaceOpenCommand { path: WirePath });
wire_struct!(ObjectiveCreateCommand {
    workspace_id: Id,
    title: String,
    intent: Intent,
    integration_target: String,
    start: Start,
});
wire_struct!(UpdateIntentCommand {
    objective_id: Id,
    expected_intent_revision: Counter,
    intent: Intent,
});
wire_struct!(ExecutionStartCommand {
    objective_id: Id,
    model: Model,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingLevel>,
});
wire_struct!(MessageSendCommand {
    objective_id: Id,
    text: String,
    mode: DeliveryMode,
});
wire_struct!(DraftSaveCommand {
    draft_id: Id,
    objective_id: Id,
    expected_revision: Counter,
    reference: Reference,
    body: String,
});
wire_struct!(DraftDeleteCommand {
    draft_id: Id,
    expected_revision: Counter,
});
wire_struct!(FeedbackSubmitCommand {
    objective_id: Id,
    destination_session_id: Id,
    association_revision: Counter,
    drafts: Vec<DraftRevision>,
    mode: DeliveryMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
});
wire_struct!(CandidateCommand {
    objective_id: Id,
    candidate_id: Id,
});
wire_struct!(ReviewVerifyCommand {
    objective_id: Id,
    candidate_id: Id,
    command: Vec<String>,
});
wire_struct!(ReviewApproveCommand {
    objective_id: Id,
    candidate_id: Id,
    target_commit: GitId,
    #[serde(skip_serializing_if = "Option::is_none")]
    override_reason: Option<String>,
});
wire_struct!(ReviewIntegrateCommand {
    objective_id: Id,
    approval_id: Id,
});
wire_struct!(ObjectiveCleanupCommand {
    objective_id: Id,
    confirm_data_loss: bool,
});
wire_struct!(ObjectiveHistoryDeleteCommand {
    objective_id: Id,
    confirm_history_loss: bool,
});
wire_struct!(SettingsUpdateCommand {
    #[serde(skip_serializing_if = "Option::is_none")]
    default_model: Option<Model>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verification_commands: Option<Vec<Vec<String>>>,
});
wire_struct!(ServiceStopCommand { reason: StopReason });
