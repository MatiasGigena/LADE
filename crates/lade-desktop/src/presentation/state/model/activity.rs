//! The Agent Session's activity log, and the drafts and inputs addressed to it.

use super::ids::{
    ActivityItemId, DraftId, InputId, ObjectiveId, OperationId, SessionId, Timestamp,
};
use super::review::Reference;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ActivityRole {
    User,
    Assistant,
    Thinking,
    Tool,
    System,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityItem {
    pub item_id: ActivityItemId,
    pub seq: u64,
    pub at: Timestamp,
    pub role: ActivityRole,
    pub title: String,
    pub text: String,
    pub tool_name: Option<String>,
    pub is_error: bool,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeliveryMode {
    Prompt,
    Steer,
    FollowUp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftState {
    Unsent,
    Submitted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub draft_id: DraftId,
    pub objective_id: ObjectiveId,
    pub destination_session_id: SessionId,
    pub reference: Reference,
    pub body: String,
    pub revision: u64,
    pub state: DraftState,
    pub stale: bool,
    pub input_id: Option<InputId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryState {
    Queued,
    Dispatched,
    Received,
    Uncertain,
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputSummary {
    pub input_id: InputId,
    pub objective_id: ObjectiveId,
    pub operation_id: OperationId,
    pub mode: DeliveryMode,
    pub text_preview: String,
    pub accepted_at: Timestamp,
    pub delivery: DeliveryState,
    pub blocked_reason: Option<String>,
}
