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

pub const MAX_ITEM_TEXT_BYTES: usize = 192 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum WorkerBody {
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_file: Option<String>,
    },
    #[serde(rename = "input.deliver")]
    Deliver {
        input_id: Id,
        mode: DeliveryMode,
        text: String,
    },
    #[serde(rename = "session.abort")]
    Abort,
    #[serde(rename = "worker.shutdown")]
    Shutdown,
    #[serde(rename = "session.command")]
    Command {
        command_id: Id,
        command: SessionCommand,
    },
    #[serde(rename = "ui.respond")]
    UiRespond {
        ui_id: Id,
        generation: Counter,
        answer: UiAnswer,
    },
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerExtension {
    pub name: String,
    pub path: String,
    pub sha256: String,
}

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

/// The model catalogue a worker in catalogue mode (`src/worker/catalog.ts`) writes to its output
/// file. It is not a frame and is not schema-checked: fields default when absent so the service can
/// skip incomplete entries, and it applies its own caps.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub sdk_version: String,
    pub providers: Vec<CatalogProvider>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogProvider {
    pub id: String,
    pub name: String,
    pub api_key: bool,
    pub oauth: Option<String>,
    pub subscription: bool,
    pub base_url: Option<String>,
    pub models: Vec<CatalogModel>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogModel {
    pub id: String,
    pub name: String,
    pub reasoning: bool,
    /// pi thinking level names; levels this build does not know are skipped by the reader.
    pub thinking: Vec<String>,
}
