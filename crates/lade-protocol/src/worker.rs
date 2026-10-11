//! Service <-> worker DTOs mirroring `contracts/v1/worker-schema.json` (JSONL framing).
//!
//! A worker report is never approval, verification authority or durable acceptance; the
//! service binds every worker link to one Objective at launch and ignores identity claims.
use crate::types::{
    ActivityRole, Counter, DeliveryMode, ForkPosition, Id, Model, SessionChangeReason,
    ThinkingLevel, TreeEntry, UiAnswer, UiCloseReason, UiPrompt,
};
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thinking: Option<ThinkingLevel>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resources: Option<WorkerResources>,
        /// Resume this pi session file instead of the most recent one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_file: Option<String>,
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
    /// IMP-011 session operation; answered by exactly one `session.result`.
    #[serde(rename = "session.command")]
    Command {
        command_id: Id,
        command: SessionCommand,
    },
    /// The developer's answer to an open extension dialog of `generation`.
    #[serde(rename = "ui.respond")]
    UiRespond {
        ui_id: Id,
        generation: Counter,
        answer: UiAnswer,
    },
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
    #[serde(rename = "session.result")]
    CommandResult {
        command_id: Id,
        ok: bool,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entries: Option<Vec<TreeEntry>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        leaf_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        truncated: Option<bool>,
    },
    /// The worker (re)bound its pi session runtime; `generation` grows with every rebind.
    #[serde(rename = "session.changed")]
    SessionChanged {
        generation: Counter,
        reason: SessionChangeReason,
        pi_session_id: String,
        session_file: String,
        model: Model,
        thinking: ThinkingLevel,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        leaf_id: Option<String>,
    },
    #[serde(rename = "resources.loaded")]
    ResourcesLoaded {
        generation: Counter,
        resources: Vec<LoadedResource>,
    },
    #[serde(rename = "ui.request")]
    UiRequest {
        ui_id: Id,
        generation: Counter,
        prompt: UiPrompt,
    },
    #[serde(rename = "ui.closed")]
    UiClosed { ui_id: Id, reason: UiCloseReason },
}

/// An approved LADE-profile extension: loaded only while its content hashes to `sha256`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerExtension {
    pub name: String,
    pub path: String,
    pub sha256: String,
}

/// The LADE profile snapshot inside the session directory and its approved extensions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerResources {
    pub profile_dir: String,
    pub extensions: Vec<WorkerExtension>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionCommand {
    Tree,
    Navigate {
        entry_id: String,
    },
    Fork {
        entry_id: String,
        position: ForkPosition,
    },
    Import {
        path: String,
    },
    Export {
        path: String,
    },
    Compact {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
    },
    Reload {
        resources: WorkerResources,
    },
    Configure {
        model: Model,
        thinking: ThinkingLevel,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadedKind {
    Skill,
    Prompt,
    Context,
    Extension,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceSource {
    LadeProfile,
    Repository,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadStatus {
    Loaded,
    Refused,
    Failed,
}

/// What the worker actually loaded for one generation (a report, never an approval).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadedResource {
    pub kind: LoadedKind,
    pub name: String,
    pub source: ResourceSource,
    pub status: LoadStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
impl WorkerBody {
    /// True for bodies only the worker may send; the service must reject the others from a worker.
    pub fn is_from_worker(&self) -> bool {
        !matches!(
            self,
            Self::Start { .. }
                | Self::Deliver { .. }
                | Self::Abort
                | Self::Shutdown
                | Self::Command { .. }
                | Self::UiRespond { .. }
        )
    }
}
