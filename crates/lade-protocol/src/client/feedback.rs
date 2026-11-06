//! Drafts and accepted inputs as the client sees them.

use super::{DeliveryState, DraftState, Reference};
use crate::types::{Counter, DeliveryMode, Id, Timestamp};
use serde::{Deserialize, Serialize};

wire_struct!(Draft {
    draft_id: Id,
    objective_id: Id,
    destination_session_id: Id,
    reference: Reference,
    body: String,
    revision: Counter,
    state: DraftState,
    stale: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_id: Option<Id>,
});
wire_struct!(InputSummary {
    input_id: Id,
    objective_id: Id,
    operation_id: Id,
    mode: DeliveryMode,
    text_preview: String,
    accepted_at: Timestamp,
    delivery: DeliveryState,
    #[serde(skip_serializing_if = "Option::is_none")]
    blocked_reason: Option<String>,
});
wire_struct!(DraftRevision {
    draft_id: Id,
    revision: Counter,
});
