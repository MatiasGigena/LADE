//! Service-wide state: search, the connection, notices, settings and loading state.

use lade_protocol::client::{ErrorCode, NoticeCode};

use super::ids::{ModelRef, ObjectiveId, RepoPath};
use super::review::Basis;

/// The stable code of a refusal or notice: the service's own, or one the desktop gives a
/// problem or outcome it found itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportCode {
    /// The service rejected a request.
    Rejected(ErrorCode),
    /// The service sent a notice.
    Service(NoticeCode),
    /// A request the window could not build.
    NotSent,
    /// A command issued while the window was not connected.
    NotConnected,
    /// The outcome of a command whose reply was lost is being checked.
    CheckingOutcome,
    /// A command whose reply was lost had completed.
    Recovered,
    /// The service cannot say how a command whose reply was lost ended.
    Uncertain,
    /// The service reports a command whose reply was lost as blocked.
    Blocked,
    /// A command whose reply was lost never reached the service.
    NotReceived,
    /// The terminal pane could not reach its terminal.
    TerminalProblem,
    /// A draft could not be saved.
    SaveFailed,
    /// The keybindings file was ignored.
    KeybindingsIgnored,
    /// A profile import finished.
    ProfileImported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SearchScope {
    Objectives,
    Files,
    Content,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchResultKind {
    Objective,
    File,
    Content,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchResult {
    pub kind: SearchResultKind,
    pub objective_id: Option<ObjectiveId>,
    pub path: Option<RepoPath>,
    pub line: Option<u64>,
    pub preview: String,
    pub byte_start: Option<u64>,
    pub byte_end: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchResults {
    pub scope: SearchScope,
    pub query: String,
    pub scope_label: String,
    pub basis: Option<Basis>,
    pub objective_id: Option<ObjectiveId>,
    pub results: Vec<SearchResult>,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Connected,
    Reconnecting { attempt: u32 },
    Disconnected { reason: String },
    Unavailable { reason: String },
    LeaseHeld,
}

impl ConnectionState {
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

/// What a notice is about, so a later outcome can resolve the notices it answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NoticeScope {
    #[default]
    General,
    /// A failed attempt to open a workspace folder.
    WorkspaceOpen,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceNotice {
    pub level: NoticeLevel,
    pub code: ReportCode,
    pub message: String,
    pub scope: NoticeScope,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Settings {
    pub default_model: Option<ModelRef>,
    pub verification_commands: Vec<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Load<T> {
    NotLoaded,
    Loading,
    Loaded(T),
    Failed(String),
}

impl<T> Load<T> {
    pub fn loaded(&self) -> Option<&T> {
        match self {
            Self::Loaded(value) => Some(value),
            _ => None,
        }
    }
}
