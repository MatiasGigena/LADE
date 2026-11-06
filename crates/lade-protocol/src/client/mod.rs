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
        #[cfg(test)]
        impl $name {
            pub(crate) const ALL: &'static [Self] = &[$(Self::$variant),*];
        }
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
#[cfg(test)]
mod fixtures;
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
    ObjectiveCleanupCommand, ObjectiveCreateCommand, ObjectiveDeleteHistoryCommand,
    ObjectiveSelector, ObjectiveUpdateIntentCommand, OperationGetQuery, ReviewApproveCommand,
    ReviewIntegrateCommand, ReviewVerifyCommand, SearchQuery, ServiceStopCommand,
    SettingsUpdateCommand, WorkspaceOpenCommand,
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

fn to_value(value: &impl Serialize) -> Result<Value, WireError> {
    serde_json::to_value(value).map_err(|_| WireError::InvalidSchema)
}
fn from_value<T: DeserializeOwned>(value: Value) -> Result<T, WireError> {
    serde_json::from_value(value).map_err(|_| WireError::InvalidSchema)
}

/// Serializes a closed name catalogue as its wire string and refuses names outside it.
macro_rules! wire_name {
    ($name:ident) => {
        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }
        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let name = <String as serde::Deserialize>::deserialize(deserializer)?;
                Self::parse(&name).ok_or_else(|| serde::de::Error::custom("invalid_schema"))
            }
        }
    };
}

/// How a request is handled across retries and reconnects.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum RequestClass {
    /// Read-only; no `operation_id`. A client may resend it after a reconnect.
    Query,
    /// Mutating; carries an `operation_id` and is journaled, so a retry returns the first outcome.
    Command,
    /// Acts on a live service-owned resource (a terminal); no `operation_id`, never journaled,
    /// replayed or resent after a reconnect.
    Interaction,
}

macro_rules! requests {
    ($($variant:ident = $wire:literal, $class:ident, $payload:ty => $data:ty;)*) => {
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
            pub const fn class(self) -> RequestClass {
                match self { $(RequestName::$variant => RequestClass::$class),* }
            }
            /// Whether the request is a [`RequestClass::Command`], the only class that carries an
            /// `operation_id`.
            pub const fn is_mutating(self) -> bool {
                matches!(self.class(), RequestClass::Command)
            }
        }

        #[derive(Clone, Debug, PartialEq)]
        pub enum Request { $($variant($payload)),* }
        impl Request {
            pub const fn name(&self) -> RequestName {
                match self { $(Request::$variant(_) => RequestName::$variant),* }
            }
            /// The request's payload as JSON.
            ///
            /// # Errors
            ///
            /// `WireError::InvalidSchema` when the payload cannot be represented as JSON.
            pub fn payload_value(&self) -> Result<Value, WireError> {
                match self { $(Request::$variant(p) => to_value(p)),* }
            }
            /// Rebuilds the typed request named `name` from its JSON payload.
            ///
            /// # Errors
            ///
            /// `WireError::InvalidSchema` when the payload does not match the request's payload
            /// type.
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
            /// The reply data as JSON.
            ///
            /// # Errors
            ///
            /// `WireError::InvalidSchema` when the data cannot be represented as JSON.
            pub fn data_value(&self) -> Result<Value, WireError> {
                match self { $(ReplyData::$variant(d) => to_value(d)),* }
            }
            /// Rebuilds the typed reply data for `name` from its JSON value.
            ///
            /// # Errors
            ///
            /// `WireError::InvalidSchema` when the data does not match the request's reply type.
            pub fn from_parts(name: RequestName, data: Value) -> Result<Self, WireError> {
                match name { $(RequestName::$variant => from_value(data).map(ReplyData::$variant)),* }
            }
        }
    };
}

requests! {
    Snapshot = "snapshot", Query, Empty => SnapshotData;
    ObjectiveGet = "objective.get", Query, ObjectiveSelector => Box<ObjectiveDetail>;
    ActivityPage = "activity.page", Query, ActivityPageQuery => ActivityPage;
    ChangesList = "changes.list", Query, ChangesListQuery => ChangeList;
    FileRead = "file.read", Query, FileReadQuery => FileContent;
    DiffGet = "diff.get", Query, DiffGetQuery => FileDiff;
    Search = "search", Query, SearchQuery => SearchResults;
    DraftsList = "drafts.list", Query, DraftsListQuery => DraftList;
    InputsList = "inputs.list", Query, InputsListQuery => InputList;
    OperationGet = "operation.get", Query, OperationGetQuery => OperationStatus;
    WorkspaceOpen = "workspace.open", Command, WorkspaceOpenCommand => Workspace;
    ObjectiveCreate = "objective.create", Command, ObjectiveCreateCommand => Box<ObjectiveDetail>;
    ObjectiveUpdateIntent = "objective.update_intent", Command, ObjectiveUpdateIntentCommand => Box<ObjectiveDetail>;
    ExecutionStart = "execution.start", Command, ExecutionStartCommand => Box<ObjectiveDetail>;
    ExecutionCancel = "execution.cancel", Command, ObjectiveSelector => Box<ObjectiveDetail>;
    ExecutionResume = "execution.resume", Command, ObjectiveSelector => Box<ObjectiveDetail>;
    MessageSend = "message.send", Command, MessageSendCommand => InputSummary;
    DraftSave = "draft.save", Command, DraftSaveCommand => Draft;
    DraftDelete = "draft.delete", Command, DraftDeleteCommand => Empty;
    FeedbackSubmit = "feedback.submit", Command, FeedbackSubmitCommand => InputSummary;
    ReviewFreeze = "review.freeze", Command, ObjectiveSelector => CandidateSummary;
    ReviewReturn = "review.return", Command, CandidateCommand => Box<ObjectiveDetail>;
    ReviewVerify = "review.verify", Command, ReviewVerifyCommand => VerificationSummary;
    ReviewApprove = "review.approve", Command, ReviewApproveCommand => ApprovalSummary;
    ReviewRefresh = "review.refresh", Command, CandidateCommand => CandidateSummary;
    ReviewIntegrate = "review.integrate", Command, ReviewIntegrateCommand => Box<ObjectiveDetail>;
    ObjectiveComplete = "objective.complete", Command, ObjectiveSelector => Box<ObjectiveDetail>;
    ObjectiveDiscard = "objective.discard", Command, ObjectiveSelector => Box<ObjectiveDetail>;
    ObjectiveCleanup = "objective.cleanup", Command, ObjectiveCleanupCommand => Box<ObjectiveDetail>;
    SettingsUpdate = "settings.update", Command, SettingsUpdateCommand => Settings;
    ModelsList = "models.list", Query, ModelsListQuery => ModelCatalog;
    SessionTree = "session.tree", Query, ObjectiveSelector => SessionTree;
    ResourcesList = "resources.list", Query, Empty => ResourceInventory;
    SessionNavigate = "session.navigate", Command, SessionNavigateCommand => SessionState;
    SessionFork = "session.fork", Command, SessionForkCommand => SessionState;
    SessionImport = "session.import", Command, SessionImportCommand => SessionState;
    SessionExport = "session.export", Command, ObjectiveSelector => SessionExport;
    SessionCompact = "session.compact", Command, SessionCompactCommand => SessionState;
    SessionReload = "session.reload", Command, ObjectiveSelector => SessionState;
    SessionConfigure = "session.configure", Command, SessionConfigureCommand => SessionState;
    ResourceDecide = "resource.decide", Command, ResourceDecideCommand => ResourceInventory;
    ProfileImport = "profile.import", Command, ProfileImportCommand => ProfileImportResult;
    UiRespond = "ui.respond", Command, UiRespondCommand => Empty;
    TerminalOpen = "terminal.open", Interaction, TerminalOpenCommand => TerminalInfo;
    TerminalInput = "terminal.input", Interaction, TerminalInputCommand => Empty;
    TerminalResize = "terminal.resize", Interaction, TerminalResizeCommand => Empty;
    TerminalClose = "terminal.close", Interaction, TerminalSelector => Empty;
    ServiceStop = "service.stop", Command, ServiceStopCommand => Empty;
    ObjectiveDeleteHistory = "objective.delete_history", Command, ObjectiveDeleteHistoryCommand => Box<ObjectiveDetail>;
}
wire_name!(RequestName);

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
            /// The event's payload as JSON.
            ///
            /// # Errors
            ///
            /// `WireError::InvalidSchema` when the payload cannot be represented as JSON.
            pub fn payload_value(&self) -> Result<Value, WireError> {
                match self { $(Event::$variant(p) => to_value(p)),* }
            }
            /// Rebuilds the typed event named `name` from its JSON payload.
            ///
            /// # Errors
            ///
            /// `WireError::InvalidSchema` when the payload does not match the event's payload type.
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
wire_name!(EventName);
