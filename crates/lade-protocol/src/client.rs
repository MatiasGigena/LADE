//! Client <-> service DTOs mirroring `contracts/v1/schema.json` one-to-one.
//!
//! Construction is not validation: `encode` re-validates against the schema before bytes leave,
//! and `decode` validates before any DTO is built.
use crate::{
    WireError,
    types::{
        ActivityRole, Capability, Counter, DeliveryMode, Digest, ForkPosition, GitId, Id, Model,
        PageLimit, ReadLimit, SessionChangeReason, ThinkingLevel, Timestamp, TreeEntry, UiAnswer,
        UiCloseReason, UiPrompt, WirePath, truncate_chars,
    },
};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

fn to_value(value: &impl Serialize) -> Value {
    // Every DTO here has string keys and no fallible custom serializer.
    serde_json::to_value(value).expect("wire DTOs serialize to JSON")
}
fn from_value<T: DeserializeOwned>(value: Value) -> Result<T, WireError> {
    serde_json::from_value(value).map_err(|_| WireError("invalid_schema"))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientEnvelope {
    pub protocol: u32,
    pub bundle: String,
    pub body: ClientBody,
}
impl ClientEnvelope {
    /// Wraps a body with this build's exact protocol and bundle identity.
    pub fn new(body: ClientBody) -> Self {
        Self {
            protocol: crate::VERSION,
            bundle: crate::BUNDLE.to_owned(),
            body,
        }
    }
}

/// Which peer may send a body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Both,
    ClientToService,
    ServiceToClient,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum ClientBody {
    /// Version declaration only; never authentication.
    #[serde(rename = "hello")]
    Hello,
    #[serde(rename = "auth")]
    Auth { capability: Capability },
    #[serde(rename = "welcome")]
    Welcome { service_epoch: Id, lease_id: Id },
    #[serde(rename = "request")]
    Request(RequestFrame),
    #[serde(rename = "reply")]
    Reply(Reply),
    #[serde(rename = "event")]
    Event(EventFrame),
    #[serde(rename = "chunk.start")]
    ChunkStart {
        transfer_id: Id,
        owner: Scope,
        execution_generation: Counter,
        total_bytes: Counter,
        sha256: Digest,
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
impl ClientBody {
    pub fn direction(&self) -> Direction {
        match self {
            Self::Hello
            | Self::ChunkStart { .. }
            | Self::ChunkData { .. }
            | Self::ChunkEnd { .. }
            | Self::ChunkCancel { .. } => Direction::Both,
            Self::Auth { .. } | Self::Request(_) => Direction::ClientToService,
            Self::Welcome { .. } | Self::Reply(_) | Self::Event(_) => Direction::ServiceToClient,
        }
    }
}

/// Chunk-transfer owner binding (retained from IMP-001).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub workspace_id: Id,
    pub objective_id: Id,
    pub session_id: Id,
    pub association_revision: Counter,
}

// ---------------------------------------------------------------------------------------------
// Requests, replies and their name-indexed payloads.

macro_rules! requests {
    ($($variant:ident = $wire:literal, $mutating:literal, $payload:ty => $data:ty;)*) => {
        /// Every request name in the v1 catalog.
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
        pub enum RequestName { $($variant),* }
        impl RequestName {
            pub const ALL: &'static [RequestName] = &[$(RequestName::$variant),*];
            pub const fn as_str(self) -> &'static str {
                match self { $(RequestName::$variant => $wire),* }
            }
            pub fn parse(name: &str) -> Option<Self> {
                match name { $($wire => Some(RequestName::$variant),)* _ => None }
            }
            /// Mutating names carry a stable `operation_id`; queries never do.
            pub const fn is_mutating(self) -> bool {
                match self { $(RequestName::$variant => $mutating),* }
            }
        }

        /// A request's typed payload, discriminated by its wire `name`.
        #[derive(Clone, Debug, PartialEq)]
        pub enum Request { $($variant($payload)),* }
        impl Request {
            pub const fn name(&self) -> RequestName {
                match self { $(Request::$variant(_) => RequestName::$variant),* }
            }
            pub fn payload_value(&self) -> Value {
                match self { $(Request::$variant(p) => to_value(p)),* }
            }
            pub fn from_parts(name: RequestName, payload: Value) -> Result<Self, WireError> {
                match name { $(RequestName::$variant => from_value(payload).map(Request::$variant)),* }
            }
        }

        /// Successful reply data, typed per request name.
        #[derive(Clone, Debug, PartialEq)]
        pub enum ReplyData { $($variant($data)),* }
        impl ReplyData {
            pub const fn name(&self) -> RequestName {
                match self { $(ReplyData::$variant(_) => RequestName::$variant),* }
            }
            pub fn data_value(&self) -> Value {
                match self { $(ReplyData::$variant(d) => to_value(d)),* }
            }
            pub fn from_parts(name: RequestName, data: Value) -> Result<Self, WireError> {
                match name { $(RequestName::$variant => from_value(data).map(ReplyData::$variant)),* }
            }
        }
    };
}

requests! {
    Snapshot = "snapshot", false, Empty => SnapshotData;
    ObjectiveGet = "objective.get", false, ObjectiveSelector => Box<ObjectiveDetail>;
    ActivityPage = "activity.page", false, ActivityPageQuery => ActivityPage;
    ChangesList = "changes.list", false, ChangesListQuery => ChangeList;
    FileRead = "file.read", false, FileReadQuery => FileContent;
    DiffGet = "diff.get", false, DiffGetQuery => FileDiff;
    Search = "search", false, SearchQuery => SearchResults;
    DraftsList = "drafts.list", false, ObjectiveSelector => DraftList;
    InputsList = "inputs.list", false, ObjectiveSelector => InputList;
    OperationGet = "operation.get", false, OperationGetQuery => OperationStatus;
    WorkspaceOpen = "workspace.open", true, WorkspaceOpenCommand => Workspace;
    ObjectiveCreate = "objective.create", true, ObjectiveCreateCommand => Box<ObjectiveDetail>;
    ObjectiveUpdateIntent = "objective.update_intent", true, UpdateIntentCommand => Box<ObjectiveDetail>;
    ExecutionStart = "execution.start", true, ExecutionStartCommand => Box<ObjectiveDetail>;
    ExecutionCancel = "execution.cancel", true, ObjectiveSelector => Box<ObjectiveDetail>;
    ExecutionResume = "execution.resume", true, ObjectiveSelector => Box<ObjectiveDetail>;
    MessageSend = "message.send", true, MessageSendCommand => InputSummary;
    DraftSave = "draft.save", true, DraftSaveCommand => Draft;
    DraftDelete = "draft.delete", true, DraftDeleteCommand => Empty;
    FeedbackSubmit = "feedback.submit", true, FeedbackSubmitCommand => InputSummary;
    ReviewFreeze = "review.freeze", true, ObjectiveSelector => CandidateSummary;
    ReviewReturn = "review.return", true, CandidateCommand => Box<ObjectiveDetail>;
    ReviewVerify = "review.verify", true, ReviewVerifyCommand => VerificationSummary;
    ReviewApprove = "review.approve", true, ReviewApproveCommand => ApprovalSummary;
    ReviewRefresh = "review.refresh", true, CandidateCommand => CandidateSummary;
    ReviewIntegrate = "review.integrate", true, ReviewIntegrateCommand => Box<ObjectiveDetail>;
    ObjectiveComplete = "objective.complete", true, ObjectiveSelector => Box<ObjectiveDetail>;
    ObjectiveDiscard = "objective.discard", true, ObjectiveSelector => Box<ObjectiveDetail>;
    ObjectiveCleanup = "objective.cleanup", true, ObjectiveCleanupCommand => Box<ObjectiveDetail>;
    SettingsUpdate = "settings.update", true, SettingsUpdateCommand => Settings;
    // IMP-011 pi compatibility.
    ModelsList = "models.list", false, ModelsListQuery => ModelCatalog;
    SessionTree = "session.tree", false, ObjectiveSelector => SessionTree;
    ResourcesList = "resources.list", false, Empty => ResourceInventory;
    SessionNavigate = "session.navigate", true, SessionNavigateCommand => SessionState;
    SessionFork = "session.fork", true, SessionForkCommand => SessionState;
    SessionImport = "session.import", true, SessionImportCommand => SessionState;
    SessionExport = "session.export", true, ObjectiveSelector => SessionExport;
    SessionCompact = "session.compact", true, SessionCompactCommand => SessionState;
    SessionReload = "session.reload", true, ObjectiveSelector => SessionState;
    SessionConfigure = "session.configure", true, SessionConfigureCommand => SessionState;
    ResourceDecide = "resource.decide", true, ResourceDecideCommand => ResourceInventory;
    ProfileImport = "profile.import", true, ProfileImportCommand => ProfileImportResult;
    UiRespond = "ui.respond", true, UiRespondCommand => Empty;
}

impl Serialize for RequestName {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
impl<'de> Deserialize<'de> for RequestName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::parse(&name).ok_or_else(|| serde::de::Error::custom("invalid_schema"))
    }
}

/// `{kind:"request", request_id, operation_id?, name, payload}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawRequest", into = "RawRequest")]
pub struct RequestFrame {
    pub request_id: Id,
    /// Present exactly when `request.name().is_mutating()`.
    pub operation_id: Option<Id>,
    pub request: Request,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRequest {
    request_id: Id,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation_id: Option<Id>,
    name: RequestName,
    payload: Value,
}
impl RequestFrame {
    /// Rejects an `operation_id` on a query or its absence on a mutating request.
    pub fn new(
        request_id: Id,
        operation_id: Option<Id>,
        request: Request,
    ) -> Result<Self, WireError> {
        if operation_id.is_some() != request.name().is_mutating() {
            return Err(WireError("invalid_schema"));
        }
        Ok(Self {
            request_id,
            operation_id,
            request,
        })
    }
    /// SHA-256 of the canonical semantic content (`operation_id`, `name`, `payload`; object keys
    /// sorted, arrays preserved, exact strings), excluding `request_id`. Equal digests mean a
    /// duplicate operation; differing digests under one `operation_id` are `duplicate_conflict`.
    pub fn canonical_digest(&self) -> [u8; 32] {
        let mut content = to_value(self);
        content
            .as_object_mut()
            .expect("request serializes as an object")
            .remove("request_id");
        // serde_json's default map is a BTreeMap: keys sort recursively, arrays keep order.
        Sha256::digest(serde_json::to_vec(&content).expect("JSON value serializes")).into()
    }
}
impl TryFrom<RawRequest> for RequestFrame {
    type Error = WireError;
    fn try_from(raw: RawRequest) -> Result<Self, WireError> {
        Self::new(
            raw.request_id,
            raw.operation_id,
            Request::from_parts(raw.name, raw.payload)?,
        )
    }
}
impl From<RequestFrame> for RawRequest {
    fn from(frame: RequestFrame) -> Self {
        Self {
            request_id: frame.request_id,
            operation_id: frame.operation_id,
            name: frame.request.name(),
            payload: frame.request.payload_value(),
        }
    }
}

pub const MAX_DETAIL_CHARS: usize = 512;

/// `{status:"rejected", code, action, detail}`. `detail` never carries source or secrets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rejection {
    pub code: ErrorCode,
    pub action: SafeAction,
    pub detail: String,
}
impl Rejection {
    /// Truncates `detail` to the 512-code-point schema bound.
    pub fn new(code: ErrorCode, action: SafeAction, detail: &str) -> Self {
        Self {
            code,
            action,
            detail: truncate_chars(detail, MAX_DETAIL_CHARS).to_owned(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum RawResult {
    Ok {
        data: Value,
    },
    Rejected {
        code: ErrorCode,
        action: SafeAction,
        detail: String,
    },
}
fn raw_result(data: Result<&ReplyData, &Rejection>) -> RawResult {
    match data {
        Ok(data) => RawResult::Ok {
            data: data.data_value(),
        },
        Err(r) => RawResult::Rejected {
            code: r.code,
            action: r.action,
            detail: r.detail.clone(),
        },
    }
}
fn typed_result(
    name: RequestName,
    raw: RawResult,
) -> Result<Result<ReplyData, Rejection>, WireError> {
    Ok(match raw {
        RawResult::Ok { data } => Ok(ReplyData::from_parts(name, data)?),
        RawResult::Rejected {
            code,
            action,
            detail,
        } => Err(Rejection {
            code,
            action,
            detail,
        }),
    })
}

#[derive(Clone, Debug, PartialEq)]
pub enum ReplyResult {
    Ok(Box<ReplyData>),
    /// `name` is absent only when the request's name could not be read (`invalid_request`).
    Rejected {
        name: Option<RequestName>,
        rejection: Rejection,
    },
}
impl ReplyResult {
    pub fn name(&self) -> Option<RequestName> {
        match self {
            Self::Ok(data) => Some(data.name()),
            Self::Rejected { name, .. } => *name,
        }
    }
}

/// `{kind:"reply", request_id, name?, result}`. The name echoes the request so `data` is
/// schema-checked against its name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawReply", into = "RawReply")]
pub struct Reply {
    pub request_id: Id,
    pub result: ReplyResult,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReply {
    request_id: Id,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<RequestName>,
    result: RawResult,
}
impl Reply {
    pub fn ok(request_id: Id, data: ReplyData) -> Self {
        Self {
            request_id,
            result: ReplyResult::Ok(Box::new(data)),
        }
    }
    pub fn rejected(request_id: Id, name: Option<RequestName>, rejection: Rejection) -> Self {
        Self {
            request_id,
            result: ReplyResult::Rejected { name, rejection },
        }
    }
    /// The durable outcome of a mutating reply (`None` for queries and unnamed rejections).
    pub fn outcome(&self) -> Option<Outcome> {
        match &self.result {
            ReplyResult::Ok(data) if data.name().is_mutating() => Some(Outcome::Ok(data.clone())),
            ReplyResult::Rejected {
                name: Some(name),
                rejection,
            } if name.is_mutating() => Some(Outcome::Rejected {
                name: *name,
                rejection: rejection.clone(),
            }),
            _ => None,
        }
    }
}
impl TryFrom<RawReply> for Reply {
    type Error = WireError;
    fn try_from(raw: RawReply) -> Result<Self, WireError> {
        let result = match (raw.name, raw.result) {
            (Some(name), result) => match typed_result(name, result)? {
                Ok(data) => ReplyResult::Ok(Box::new(data)),
                Err(rejection) => ReplyResult::Rejected {
                    name: Some(name),
                    rejection,
                },
            },
            (
                None,
                RawResult::Rejected {
                    code,
                    action,
                    detail,
                },
            ) => ReplyResult::Rejected {
                name: None,
                rejection: Rejection {
                    code,
                    action,
                    detail,
                },
            },
            (None, RawResult::Ok { .. }) => return Err(WireError("invalid_schema")),
        };
        Ok(Self {
            request_id: raw.request_id,
            result,
        })
    }
}
impl From<Reply> for RawReply {
    fn from(reply: Reply) -> Self {
        let (name, result) = match &reply.result {
            ReplyResult::Ok(data) => (Some(data.name()), raw_result(Ok(data))),
            ReplyResult::Rejected { name, rejection } => (*name, raw_result(Err(rejection))),
        };
        Self {
            request_id: reply.request_id,
            name,
            result,
        }
    }
}

/// The recorded outcome of a mutating operation, as returned by `operation.get`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawOutcome", into = "RawOutcome")]
pub enum Outcome {
    Ok(Box<ReplyData>),
    Rejected {
        name: RequestName,
        rejection: Rejection,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOutcome {
    name: RequestName,
    result: RawResult,
}
impl TryFrom<RawOutcome> for Outcome {
    type Error = WireError;
    fn try_from(raw: RawOutcome) -> Result<Self, WireError> {
        if !raw.name.is_mutating() {
            return Err(WireError("invalid_schema"));
        }
        Ok(match typed_result(raw.name, raw.result)? {
            Ok(data) => Self::Ok(Box::new(data)),
            Err(rejection) => Self::Rejected {
                name: raw.name,
                rejection,
            },
        })
    }
}
impl From<Outcome> for RawOutcome {
    fn from(outcome: Outcome) -> Self {
        match &outcome {
            Outcome::Ok(data) => Self {
                name: data.name(),
                result: raw_result(Ok(data)),
            },
            Outcome::Rejected { name, rejection } => Self {
                name: *name,
                result: raw_result(Err(rejection)),
            },
        }
    }
}
// ---------------------------------------------------------------------------------------------
// Events.

macro_rules! events {
    ($($variant:ident = $wire:literal, $payload:ty;)*) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
        pub enum EventName { $($variant),* }
        impl EventName {
            pub const ALL: &'static [EventName] = &[$(EventName::$variant),*];
            pub const fn as_str(self) -> &'static str {
                match self { $(EventName::$variant => $wire),* }
            }
            pub fn parse(name: &str) -> Option<Self> {
                match name { $($wire => Some(EventName::$variant),)* _ => None }
            }
        }
        #[derive(Clone, Debug, PartialEq)]
        pub enum Event { $($variant($payload)),* }
        impl Event {
            pub const fn name(&self) -> EventName {
                match self { $(Event::$variant(_) => EventName::$variant),* }
            }
            pub fn payload_value(&self) -> Value {
                match self { $(Event::$variant(p) => to_value(p)),* }
            }
            pub fn from_parts(name: EventName, payload: Value) -> Result<Self, WireError> {
                match name { $(EventName::$variant => from_value(payload).map(Event::$variant)),* }
            }
        }
    };
}

events! {
    ObjectiveChanged = "objective.changed", ObjectiveSummary;
    ObjectiveDetail = "objective.detail", Box<ObjectiveDetail>;
    ActivityAppended = "activity.appended", ActivityAppended;
    ActivityDelta = "activity.delta", ActivityDelta;
    DraftChanged = "draft.changed", Draft;
    InputChanged = "input.changed", InputSummary;
    WorktreeChanged = "worktree.changed", WorktreeChanged;
    VerificationChanged = "verification.changed", VerificationSummary;
    ServiceNotice = "service.notice", ServiceNotice;
    SessionChanged = "session.changed", SessionState;
    UiRequest = "ui.request", UiRequest;
    UiClosed = "ui.closed", UiClosed;
    ResourcesChanged = "resources.changed", ResourceInventory;
}

impl Serialize for EventName {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
impl<'de> Deserialize<'de> for EventName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::parse(&name).ok_or_else(|| serde::de::Error::custom("invalid_schema"))
    }
}

/// `{kind:"event", service_epoch, sequence, name, payload}`. A gap in `sequence` means the
/// client must re-query `snapshot`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawEvent", into = "RawEvent")]
pub struct EventFrame {
    pub service_epoch: Id,
    pub sequence: Counter,
    pub event: Event,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEvent {
    service_epoch: Id,
    sequence: Counter,
    name: EventName,
    payload: Value,
}
impl TryFrom<RawEvent> for EventFrame {
    type Error = WireError;
    fn try_from(raw: RawEvent) -> Result<Self, WireError> {
        Ok(Self {
            service_epoch: raw.service_epoch,
            sequence: raw.sequence,
            event: Event::from_parts(raw.name, raw.payload)?,
        })
    }
}
impl From<EventFrame> for RawEvent {
    fn from(frame: EventFrame) -> Self {
        Self {
            service_epoch: frame.service_epoch,
            sequence: frame.sequence,
            name: frame.event.name(),
            payload: frame.event.payload_value(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Enumerations.

macro_rules! wire_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),* }
    };
}

wire_enum!(ErrorCode {
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
    NotFound,
    InvalidRequest,
    Conflict,
    ExecutionUnavailable,
});
wire_enum!(SafeAction {
    Refresh,
    InspectPrevious,
    QueryOperation,
    RetryRead,
    ExplicitContinue,
    None,
});
wire_enum!(OperationState {
    Accepted,
    Pending,
    Completed,
    Blocked,
    Uncertain,
    Unknown,
});
wire_enum!(Side { Old, New });
wire_enum!(Lifecycle {
    Defined,
    Active,
    Frozen,
    Approved,
    Integrated,
    CompletedNoChanges,
    Discarded,
});
wire_enum!(ExecutionState {
    None,
    Starting,
    Running,
    Idle,
    Stopping,
    Cancelled,
    Failed,
});
wire_enum!(Attention {
    NeedsAttention,
    ReadyForReview,
    Working,
    Idle,
    Archived,
});
wire_enum!(Badge {
    Interrupted,
    DeliveryUncertain,
    StaleFeedback,
    VerificationFailed,
    VerificationMissing,
    IntegrationConflict,
    StaleTarget,
    LimitReached,
    CleanedUp,
    FixtureModel,
    UnsentDrafts,
});
wire_enum!(CandidateStatus {
    Current,
    Historical,
    Invalidated,
});
wire_enum!(VerificationState {
    Running,
    Passed,
    Failed,
    Interrupted,
    Error,
});
wire_enum!(ApprovalState {
    Valid,
    Consumed,
    Invalidated,
});
wire_enum!(ChangeStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    TypeChanged,
});
wire_enum!(DraftState { Unsent, Submitted });
wire_enum!(DeliveryState {
    Queued,
    Dispatched,
    Received,
    Uncertain,
    Blocked,
});
wire_enum!(DiffLineKind { Context, Add, Del });
wire_enum!(SearchScope {
    Objectives,
    Files,
    Content,
});
wire_enum!(SearchResultKind {
    Objective,
    File,
    Content,
});
wire_enum!(FileEncoding { Utf8, Binary });
wire_enum!(NoticeLevel {
    Info,
    Warning,
    Error,
});

/// Objective commands named in `available_actions` (the service's authority projection).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum ActionName {
    #[serde(rename = "objective.update_intent")]
    ObjectiveUpdateIntent,
    #[serde(rename = "execution.start")]
    ExecutionStart,
    #[serde(rename = "execution.cancel")]
    ExecutionCancel,
    #[serde(rename = "execution.resume")]
    ExecutionResume,
    #[serde(rename = "message.send")]
    MessageSend,
    #[serde(rename = "feedback.submit")]
    FeedbackSubmit,
    #[serde(rename = "review.freeze")]
    ReviewFreeze,
    #[serde(rename = "review.return")]
    ReviewReturn,
    #[serde(rename = "review.verify")]
    ReviewVerify,
    #[serde(rename = "review.approve")]
    ReviewApprove,
    #[serde(rename = "review.refresh")]
    ReviewRefresh,
    #[serde(rename = "review.integrate")]
    ReviewIntegrate,
    #[serde(rename = "objective.complete")]
    ObjectiveComplete,
    #[serde(rename = "objective.discard")]
    ObjectiveDiscard,
    #[serde(rename = "objective.cleanup")]
    ObjectiveCleanup,
}
impl From<ActionName> for RequestName {
    fn from(action: ActionName) -> Self {
        match action {
            ActionName::ObjectiveUpdateIntent => Self::ObjectiveUpdateIntent,
            ActionName::ExecutionStart => Self::ExecutionStart,
            ActionName::ExecutionCancel => Self::ExecutionCancel,
            ActionName::ExecutionResume => Self::ExecutionResume,
            ActionName::MessageSend => Self::MessageSend,
            ActionName::FeedbackSubmit => Self::FeedbackSubmit,
            ActionName::ReviewFreeze => Self::ReviewFreeze,
            ActionName::ReviewReturn => Self::ReviewReturn,
            ActionName::ReviewVerify => Self::ReviewVerify,
            ActionName::ReviewApprove => Self::ReviewApprove,
            ActionName::ReviewRefresh => Self::ReviewRefresh,
            ActionName::ReviewIntegrate => Self::ReviewIntegrate,
            ActionName::ObjectiveComplete => Self::ObjectiveComplete,
            ActionName::ObjectiveDiscard => Self::ObjectiveDiscard,
            ActionName::ObjectiveCleanup => Self::ObjectiveCleanup,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Shapes. Field names, optionality and order mirror the schema `$defs` of the same name.

macro_rules! wire_struct {
    ($(#[$meta:meta])* $name:ident { $($(#[$fmeta:meta])* $field:ident : $ty:ty),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $name { $($(#[$fmeta])* pub $field: $ty),* }
    };
}

wire_struct!(
    /// `{}`.
    Empty {}
);
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
wire_enum!(
    /// Verification evidence is only ever recorded as observed by the service.
    Provenance { ServiceObserved }
);
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
wire_struct!(ChangedFile {
    path: WirePath,
    status: ChangeStatus,
    added: Counter,
    removed: Counter,
    binary: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    old_path: Option<WirePath>,
});

/// Comparison basis for Change Set queries.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Basis {
    Working,
    Candidate {
        candidate_id: Id,
    },
    Since {
        from_candidate_id: Id,
        to_candidate_id: Id,
    },
}

/// Objective starting point.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Start {
    /// The integration target's current commit.
    Target,
    Commit {
        commit: GitId,
    },
}

/// The inspected version a feedback reference points at.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReferenceVersion {
    Working { capture_id: Id },
    Candidate { candidate_id: Id },
}

wire_struct!(
    /// Byte-exact selection `[byte_start, byte_end)` in one side of one inspected file.
    Reference {
        artifact_digest: Digest,
        path: WirePath,
        version: ReferenceVersion,
        side: Side,
        byte_start: Counter,
        byte_end: Counter,
        selected_text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        line_hint: Option<Counter>,
    }
);
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
wire_struct!(DiffLine {
    kind: DiffLineKind,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    old_no: Option<Counter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_no: Option<Counter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    old_byte_start: Option<Counter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_byte_start: Option<Counter>,
});
wire_struct!(DiffHunk {
    old_start: Counter,
    old_lines: Counter,
    new_start: Counter,
    new_lines: Counter,
    lines: Vec<DiffLine>,
});
wire_struct!(SearchResult {
    kind: SearchResultKind,
    preview: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    objective_id: Option<Id>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<WirePath>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<Counter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    byte_start: Option<Counter>,
});

// Request payloads.
wire_struct!(ObjectiveSelector { objective_id: Id });
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
    /// `0` creates the draft.
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
wire_struct!(SettingsUpdateCommand {
    #[serde(skip_serializing_if = "Option::is_none")]
    default_model: Option<Model>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verification_commands: Option<Vec<Vec<String>>>,
});

// Reply data.
wire_struct!(SnapshotData {
    service_epoch: Id,
    sequence: Counter,
    workspaces: Vec<Workspace>,
    objectives: Vec<ObjectiveSummary>,
    settings: Settings,
});
wire_struct!(ActivityPage {
    items: Vec<ActivityItem>,
    has_more: bool,
});
wire_struct!(ChangeList {
    basis_label: String,
    target: TargetRef,
    files: Vec<ChangedFile>,
    truncated: bool,
    total_added: Counter,
    total_removed: Counter,
    #[serde(skip_serializing_if = "Option::is_none")]
    capture_id: Option<Id>,
    #[serde(skip_serializing_if = "Option::is_none")]
    candidate_id: Option<Id>,
});
wire_struct!(FileContent {
    artifact_digest: Digest,
    total_bytes: Counter,
    offset: Counter,
    length: Counter,
    encoding: FileEncoding,
    eof: bool,
    version_label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
});
wire_struct!(FileDiff {
    binary: bool,
    hunks: Vec<DiffHunk>,
    truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    old_digest: Option<Digest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_digest: Option<Digest>,
});
wire_struct!(SearchResults {
    scope_label: String,
    results: Vec<SearchResult>,
    truncated: bool,
});
wire_struct!(DraftList { drafts: Vec<Draft> });
wire_struct!(InputList {
    inputs: Vec<InputSummary>
});
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationStatus {
    pub state: OperationState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
}

// Event payloads.
wire_struct!(ActivityAppended {
    objective_id: Id,
    items: Vec<ActivityItem>,
});
wire_struct!(ActivityDelta {
    objective_id: Id,
    item_id: Id,
    append_text: String,
});
wire_struct!(WorktreeChanged {
    objective_id: Id,
    capture_id: Id,
});
wire_struct!(ServiceNotice {
    level: NoticeLevel,
    code: String,
    message: String,
});

// ---------------------------------------------------------------------------------------------
// IMP-011: model catalogue, session tree operations, LADE-profile resources, extension UI.

wire_enum!(ProviderAuth {
    None,
    ApiKey,
    SubscriptionToken,
    OauthLogin,
    CloudCredentials,
    Custom,
});
wire_enum!(ProviderStatus {
    Available,
    MissingCredentials,
    Restricted,
});
wire_struct!(ProviderInfo {
    provider: String,
    name: String,
    auth: Vec<ProviderAuth>,
    status: ProviderStatus,
    models: Counter,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    variables: Option<Vec<String>>,
});
wire_struct!(ModelInfo {
    provider: String,
    id: String,
    name: String,
    reasoning: bool,
    thinking_levels: Vec<ThinkingLevel>,
});
wire_struct!(ModelsListQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
});
wire_struct!(ModelCatalog {
    sdk_version: String,
    providers: Vec<ProviderInfo>,
    models: Vec<ModelInfo>,
    truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
});
wire_struct!(SessionTree {
    objective_id: Id,
    entries: Vec<TreeEntry>,
    truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    leaf_id: Option<String>,
});
wire_struct!(SessionNavigateCommand {
    objective_id: Id,
    entry_id: String,
});
wire_struct!(SessionForkCommand {
    objective_id: Id,
    entry_id: String,
    position: ForkPosition,
});
wire_struct!(SessionImportCommand {
    objective_id: Id,
    path: WirePath,
});
wire_struct!(SessionCompactCommand {
    objective_id: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
});
wire_struct!(SessionConfigureCommand {
    objective_id: Id,
    model: Model,
    thinking: ThinkingLevel,
});
wire_struct!(SessionState {
    objective_id: Id,
    generation: Counter,
    reason: SessionChangeReason,
    model: Model,
    thinking: ThinkingLevel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    association_revision: Option<Counter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pi_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    leaf_id: Option<String>,
});
wire_struct!(SessionExport {
    objective_id: Id,
    path: WirePath,
});
wire_enum!(ResourceKind {
    Skill,
    Prompt,
    Context,
    Extension,
    Models,
});
wire_enum!(ResourceState {
    Active,
    PendingApproval,
    Rejected,
    Refused,
});
wire_struct!(ResourceEntry {
    kind: ResourceKind,
    name: String,
    state: ResourceState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha256: Option<Digest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
});
wire_struct!(ResourceInventory {
    profile_dir: WirePath,
    entries: Vec<ResourceEntry>,
    restrictions: Vec<String>,
});
wire_struct!(ResourceDecideCommand {
    name: String,
    sha256: Digest,
    approve: bool,
});
wire_struct!(ProfileImportCommand { source: WirePath });
wire_struct!(ProfileImportResult {
    copied: Vec<String>,
    skipped: Vec<String>,
    inventory: ResourceInventory,
});
wire_struct!(UiRequest {
    objective_id: Id,
    ui_id: Id,
    generation: Counter,
    prompt: UiPrompt,
});
wire_struct!(UiClosed {
    objective_id: Id,
    ui_id: Id,
    reason: UiCloseReason,
});
wire_struct!(UiRespondCommand {
    objective_id: Id,
    ui_id: Id,
    generation: Counter,
    answer: UiAnswer,
});
