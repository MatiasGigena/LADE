//! Service <-> worker DTOs mirroring `contracts/v1/worker-schema.json` (JSONL framing).
//!
//! A worker report is never approval, verification authority or durable acceptance; the
//! service binds every worker link to one Objective at launch and ignores identity claims.
use crate::types::{ActivityRole, DeliveryMode, Id, Model};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerEnvelope {
    pub protocol: u32,
    pub bundle: String,
    pub body: WorkerBody,
}
impl WorkerEnvelope {
    pub fn new(body: WorkerBody) -> Self {
        Self {
            protocol: crate::VERSION,
            bundle: crate::BUNDLE.to_owned(),
            body,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnState {
    Running,
    Idle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerErrorCode {
    StartupFailed,
    SessionFailed,
    ProviderFailed,
    DeliveryFailed,
    ProtocolViolation,
    LimitReached,
    Internal,
}

/// Single-item text above this many bytes is truncated by the worker (`truncated: true`).
pub const MAX_ITEM_TEXT_BYTES: usize = 192 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum WorkerBody {
    // Service -> worker.
    #[serde(rename = "worker.start")]
    Start {
        objective_id: Id,
        session_dir: String,
        cwd: String,
        model: Model,
        resume: bool,
        system_note: String,
    },
    #[serde(rename = "input.deliver")]
    Deliver {
        input_id: Id,
        mode: DeliveryMode,
        text: String,
    },
    /// Clears queued steering/follow-up input first, then aborts the running turn.
    #[serde(rename = "session.abort")]
    Abort,
    #[serde(rename = "worker.shutdown")]
    Shutdown,
    // Worker -> service.
    #[serde(rename = "worker.ready")]
    Ready {
        pi_session_id: String,
        session_file: String,
    },
    #[serde(rename = "input.received")]
    Received { input_id: Id },
    #[serde(rename = "activity.item")]
    ActivityItem {
        item_id: Id,
        role: ActivityRole,
        title: String,
        text: String,
        is_error: bool,
        truncated: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
    },
    #[serde(rename = "activity.delta")]
    ActivityDelta { item_id: Id, append_text: String },
    #[serde(rename = "turn.state")]
    TurnState { state: TurnState },
    #[serde(rename = "worker.error")]
    Error {
        code: WorkerErrorCode,
        message: String,
    },
}
impl WorkerBody {
    /// True for bodies only the worker may send; the service must reject the others from a worker.
    pub fn is_from_worker(&self) -> bool {
        !matches!(
            self,
            Self::Start { .. } | Self::Deliver { .. } | Self::Abort | Self::Shutdown
        )
    }
}
