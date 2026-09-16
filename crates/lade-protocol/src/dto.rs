use crate::WireError;
use serde::{Deserialize, Serialize};

macro_rules! wire_string {
    ($name:ident, $format:literal) => {
        #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);
        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl TryFrom<String> for $name {
            type Error = WireError;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                if crate::schema::format(&value, $format) {
                    Ok(Self(value))
                } else {
                    Err(WireError("invalid_schema"))
                }
            }
        }
        impl From<$name> for String {
            fn from(value: $name) -> String {
                value.0
            }
        }
    };
}
wire_string!(Id, "uuid-v4");
wire_string!(Counter, "u64");

// Construction is not validation: use decode/encode at all transport boundaries.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub protocol: u32,
    pub bundle: String,
    pub body: Body,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub workspace_id: Id,
    pub objective_id: Id,
    pub session_id: Id,
    pub association_revision: Counter,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Body {
    #[serde(rename = "hello")]
    Hello,
    #[serde(rename = "command")]
    Command {
        request_id: Id,
        operation_id: Id,
        expected_service_epoch: Id,
        scope: Scope,
        command: Command,
    },
    #[serde(rename = "response")]
    Response {
        request_id: Id,
        operation_id: Id,
        result: Response,
    },
    #[serde(rename = "event")]
    Event {
        service_epoch: Id,
        sequence: Counter,
        scope: Scope,
        operation_id: Id,
        state: OperationState,
    },
    #[serde(rename = "chunk.start")]
    ChunkStart {
        transfer_id: Id,
        owner: Scope,
        execution_generation: Counter,
        total_bytes: Counter,
        sha256: String,
    },
    #[serde(rename = "chunk.data")]
    ChunkData {
        transfer_id: Id,
        index: Counter,
        hex: String,
    },
    #[serde(rename = "chunk.end")]
    ChunkEnd { transfer_id: Id },
    #[serde(rename = "chunk.cancel")]
    ChunkCancel { transfer_id: Id },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "name", content = "payload", deny_unknown_fields)]
pub enum Command {
    #[serde(rename = "operation.get")]
    OperationGet { operation_id: Id },
    #[serde(rename = "feedback.submit")]
    FeedbackSubmit {
        batch_id: Id,
        expected_draft_revision: Counter,
        delivery_mode: DeliveryMode,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    Steering,
    FollowUp,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Rejected { code: ErrorCode, action: SafeAction },
    Accepted { input_id: Id },
    Operation { state: OperationState },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Accepted,
    Pending,
    Completed,
    Blocked,
    Uncertain,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafeAction {
    Refresh,
    InspectPrevious,
    QueryOperation,
    RetryRead,
    ExplicitContinue,
    None,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Unauthorized,
    StaleService,
    StaleAssociation,
    StaleExecution,
    StaleReference,
    StaleCandidate,
    StaleTarget,
    InvalidLifecycle,
    UnsupportedContent,
    VersionMismatch,
    DurabilityFailure,
    PermissionDenied,
    LimitReached,
    Interrupted,
    UnknownOperation,
    ReplayExpired,
    DuplicateConflict,
}
