//! The client wire contract: envelopes, the request and event catalogues, replies and every DTO
//! they carry.

use crate::WireError;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

macro_rules! wire_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),* }
    };
}

macro_rules! wire_struct {
    ($(#[$meta:meta])* $name:ident { $($(#[$fmeta:meta])* $field:ident : $ty:ty),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $name { $($(#[$fmeta])* pub $field: $ty),* }
    };
}

mod commands;
mod envelope;
mod event;
mod feedback;
mod inspection;
mod objective;
mod reply;
mod request;
mod results;
mod session;
mod terminal;
mod vocabulary;

pub use commands::{
    ActivityPageQuery, CandidateCommand, ChangesListQuery, DiffGetQuery, DraftCursor,
    DraftDeleteCommand, DraftSaveCommand, DraftsListQuery, ExecutionStartCommand,
    FeedbackSubmitCommand, FileReadQuery, InputsListQuery, MessageSendCommand,
    ObjectiveCleanupCommand, ObjectiveCreateCommand, ObjectiveHistoryDeleteCommand,
    ObjectiveSelector, OperationGetQuery, ReviewApproveCommand, ReviewIntegrateCommand,
    ReviewVerifyCommand, SearchQuery, ServiceStopCommand, SettingsUpdateCommand,
    UpdateIntentCommand, WorkspaceOpenCommand,
};
pub use envelope::{ClientBody, ClientEnvelope, Direction, Scope};
pub use event::{ActivityAppended, ActivityDelta, EventFrame, ServiceNotice, WorktreeChanged};
pub use feedback::{Draft, DraftRevision, InputSummary};
pub use inspection::{
    Basis, ChangedFile, ContentNotice, ContentNoticeKind, DiffHunk, DiffLine, Reference,
    ReferenceVersion, SearchResult, Start,
};
pub use objective::{
    ActivityItem, ApprovalSummary, AvailableAction, CandidateSummary, ChangeCounts, Empty,
    Execution, Intent, ObjectiveDetail, ObjectiveSummary, Provenance, SessionInfo, Settings,
    TargetRef, VerificationSummary, Workspace,
};
pub use reply::{MAX_DETAIL_CHARS, MAX_STALE_DRAFTS, Outcome, Rejection, Reply, ReplyResult};
pub use request::RequestFrame;
pub use results::{
    ActivityPage, ChangeList, DraftList, FileContent, FileDiff, InputList, OperationStatus,
    SearchResults, SnapshotData,
};
pub use session::{
    ModelCatalog, ModelInfo, ModelsListQuery, ProfileImportCommand, ProfileImportResult,
    ProviderAuth, ProviderInfo, ProviderStatus, ResourceDecideCommand, ResourceEntry,
    ResourceInventory, ResourceKind, ResourceState, SessionCompactCommand, SessionConfigureCommand,
    SessionExport, SessionForkCommand, SessionImportCommand, SessionNavigateCommand, SessionState,
    SessionTree, UiClosed, UiRequest, UiRespondCommand,
};
pub use terminal::{
    TerminalClosed, TerminalInfo, TerminalInputCommand, TerminalOpenCommand, TerminalOutput,
    TerminalResizeCommand, TerminalSelector,
};
pub use vocabulary::{
    ActionName, ApprovalState, Attention, Badge, CandidateStatus, ChangeStatus, DeliveryState,
    DiffLineKind, DraftState, ErrorCode, ExecutionState, FileEncoding, Lifecycle, NoticeCode,
    NoticeLevel, OperationState, SafeAction, SearchResultKind, SearchScope, Side, StopReason,
    TerminalCloseReason, TerminalMode, VerificationState,
};

fn to_value(value: &impl Serialize) -> Value {
    serde_json::to_value(value).expect("wire DTOs serialize to JSON")
}
fn from_value<T: DeserializeOwned>(value: Value) -> Result<T, WireError> {
    serde_json::from_value(value).map_err(|_| WireError::InvalidSchema)
}

macro_rules! requests {
    ($($variant:ident = $wire:literal, $mutating:literal, $payload:ty => $data:ty;)*) => {
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
            pub const fn is_mutating(self) -> bool {
                match self { $(RequestName::$variant => $mutating),* }
            }
        }

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
    DraftsList = "drafts.list", false, DraftsListQuery => DraftList;
    InputsList = "inputs.list", false, InputsListQuery => InputList;
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
    TerminalOpen = "terminal.open", false, TerminalOpenCommand => TerminalInfo;
    TerminalInput = "terminal.input", false, TerminalInputCommand => Empty;
    TerminalResize = "terminal.resize", false, TerminalResizeCommand => Empty;
    TerminalClose = "terminal.close", false, TerminalSelector => Empty;
    ServiceStop = "service.stop", true, ServiceStopCommand => Empty;
    ObjectiveDeleteHistory = "objective.delete_history", true, ObjectiveHistoryDeleteCommand => Box<ObjectiveDetail>;
}

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
    TerminalOutput = "terminal.output", TerminalOutput;
    TerminalClosed = "terminal.closed", TerminalClosed;
}
