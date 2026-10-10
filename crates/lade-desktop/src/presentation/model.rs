//! Plain view models mirroring the delivery-plan §4.1 data shapes.
//!
//! These are presentation copies of service state, never authority. WP-G maps the protocol DTOs onto
//! them; nothing here performs I/O or depends on GTK.

macro_rules! id_type {
    ($($name:ident),* $(,)?) => {$(
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// First eight characters, for compact labels only (never as an identity).
            pub fn short(&self) -> &str {
                self.0.get(..8).unwrap_or(&self.0)
            }
        }
    )*};
}

id_type!(
    WorkspaceId,
    ObjectiveId,
    SessionId,
    CandidateId,
    CaptureId,
    DraftId,
    InputId,
    OperationId,
    VerificationId,
    ApprovalId,
    ActivityItemId,
    ArtifactDigest,
);

/// Unix milliseconds. WP-G converts the RFC 3339 wire timestamps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(pub i64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GitAlgorithm {
    Sha1,
    Sha256,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GitId {
    pub algorithm: GitAlgorithm,
    pub hex: String,
}

impl GitId {
    pub fn sha1(hex: impl Into<String>) -> Self {
        Self {
            algorithm: GitAlgorithm::Sha1,
            hex: hex.into(),
        }
    }

    pub fn short(&self) -> &str {
        self.hex.get(..7).unwrap_or(&self.hex)
    }
}

/// Lossless repository path: raw bytes are the identity, `display` is a lossy label only.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RepoPath {
    pub bytes: Vec<u8>,
    pub display: String,
}

impl RepoPath {
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        let display = String::from_utf8_lossy(&bytes).into_owned();
        Self { bytes, display }
    }

    pub fn utf8(path: &str) -> Self {
        Self::from_bytes(path.as_bytes().to_vec())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ModelRef {
    pub provider: String,
    pub id: String,
}

impl ModelRef {
    pub fn new(provider: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            id: id.into(),
        }
    }

    pub fn label(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workspace {
    pub workspace_id: WorkspaceId,
    pub path: RepoPath,
    pub name: String,
    pub default_branch: Option<String>,
    pub branches: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lifecycle {
    Defined,
    Active,
    Frozen,
    Approved,
    Integrated,
    CompletedNoChanges,
    Discarded,
}

impl ObjectiveSummary {
    /// Archived on the board: terminal lifecycle or the service's `archived` attention.
    pub fn is_archived(&self) -> bool {
        self.attention == Attention::Archived || self.lifecycle.is_archived()
    }

    pub fn badge_text(&self) -> String {
        self.badges
            .iter()
            .map(|b| b.label())
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

impl Lifecycle {
    pub fn is_archived(self) -> bool {
        matches!(
            self,
            Self::Integrated | Self::CompletedNoChanges | Self::Discarded
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExecutionState {
    None,
    Starting,
    Running,
    Idle,
    Stopping,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Execution {
    pub state: ExecutionState,
    pub generation: u64,
    /// Failure or interruption reason, when the service gives one.
    pub reason: Option<String>,
}

impl Execution {
    pub fn new(state: ExecutionState, generation: u64) -> Self {
        Self {
            state,
            generation,
            reason: None,
        }
    }
}

/// Service-derived board category; ordering is the board order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[allow(
    clippy::enum_variant_names,
    reason = "mirrors the protocol's `needs_attention` attention value"
)]
pub enum Attention {
    NeedsAttention,
    ReadyForReview,
    Working,
    Idle,
    Archived,
}

/// Secondary, service-derived Objective badges.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Badge {
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
}

impl Badge {
    pub fn label(self) -> &'static str {
        match self {
            Self::Interrupted => "interrupted",
            Self::DeliveryUncertain => "delivery uncertain",
            Self::StaleFeedback => "stale feedback",
            Self::VerificationFailed => "check failed",
            Self::VerificationMissing => "checks missing",
            Self::IntegrationConflict => "integration conflict",
            Self::StaleTarget => "target moved",
            Self::LimitReached => "limit reached",
            Self::CleanedUp => "cleaned up",
            Self::FixtureModel => "fixture model",
            Self::UnsentDrafts => "unsent drafts",
        }
    }
}

impl Attention {
    pub const BOARD_ORDER: [Attention; 4] = [
        Attention::NeedsAttention,
        Attention::ReadyForReview,
        Attention::Working,
        Attention::Idle,
    ];
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectiveSummary {
    pub objective_id: ObjectiveId,
    pub workspace_id: WorkspaceId,
    pub title: String,
    pub outcome: String,
    pub lifecycle: Lifecycle,
    pub execution: Execution,
    pub attention: Attention,
    pub badges: Vec<Badge>,
    pub updated_at: Timestamp,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Intent {
    pub outcome: String,
    pub constraints: String,
    pub acceptance: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetRef {
    pub branch: String,
    pub commit: GitId,
}

impl TargetRef {
    pub fn label(&self) -> String {
        format!("{}@{}", self.branch, self.commit.short())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    pub session_id: SessionId,
    pub association_revision: u64,
    pub execution_generation: u64,
    pub model: ModelRef,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CandidateStatus {
    Current,
    Historical,
    Invalidated,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateSummary {
    pub candidate_id: CandidateId,
    pub ordinal: u64,
    pub commit: GitId,
    pub base_target_commit: GitId,
    pub status: CandidateStatus,
    pub files: u64,
    pub added: u64,
    pub removed: u64,
    pub created_at: Timestamp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VerificationState {
    Running,
    Passed,
    Failed,
    Interrupted,
    Error,
}

/// Only service-observed provenance exists on the wire; agent claims live in the activity transcript.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Provenance {
    ServiceObserved,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerificationSummary {
    pub verification_id: VerificationId,
    pub objective_id: ObjectiveId,
    pub candidate_id: CandidateId,
    pub command: Vec<String>,
    pub state: VerificationState,
    pub exit_code: Option<u64>,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub output_tail: String,
    pub truncated: bool,
    pub provenance: Provenance,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalState {
    Valid,
    Consumed,
    Invalidated,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalSummary {
    pub approval_id: ApprovalId,
    pub candidate_id: CandidateId,
    pub target_commit: GitId,
    pub override_reason: Option<String>,
    pub state: ApprovalState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ChangeCounts {
    pub files: u64,
    pub added: u64,
    pub removed: u64,
}

/// Actions the service may project in `available_actions`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ActionName {
    ObjectiveUpdateIntent,
    ExecutionStart,
    ExecutionCancel,
    ExecutionResume,
    MessageSend,
    FeedbackSubmit,
    ReviewFreeze,
    ReviewReturn,
    ReviewVerify,
    ReviewApprove,
    ReviewRefresh,
    ReviewIntegrate,
    ObjectiveComplete,
    ObjectiveDiscard,
    ObjectiveCleanup,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvailableAction {
    pub action: ActionName,
    pub enabled: bool,
    pub reason: Option<String>,
}

/// What the client may show for an action. Absence from `available_actions` is "not offered".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionAvailability {
    Enabled,
    Disabled { reason: String },
    NotOffered,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectiveDetail {
    pub summary: ObjectiveSummary,
    pub intent: Intent,
    pub intent_revision: u64,
    pub integration_target: TargetRef,
    pub start_commit: GitId,
    pub branch: String,
    pub session: Option<SessionInfo>,
    pub candidates: Vec<CandidateSummary>,
    pub approval: Option<ApprovalSummary>,
    pub verifications: Vec<VerificationSummary>,
    pub change_counts: ChangeCounts,
    pub unsent_drafts: u64,
    pub available_actions: Vec<AvailableAction>,
}

impl ObjectiveDetail {
    /// Authority projection lookup. The client never derives availability itself.
    pub fn availability(&self, action: ActionName) -> ActionAvailability {
        match self.available_actions.iter().find(|a| a.action == action) {
            None => ActionAvailability::NotOffered,
            Some(a) if a.enabled => ActionAvailability::Enabled,
            Some(a) => ActionAvailability::Disabled {
                reason: a
                    .reason
                    .clone()
                    .unwrap_or_else(|| "Unavailable — the service gave no reason".to_owned()),
            },
        }
    }

    pub fn current_candidate(&self) -> Option<&CandidateSummary> {
        self.candidates
            .iter()
            .find(|c| c.status == CandidateStatus::Current)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Basis {
    Working,
    Candidate {
        candidate_id: CandidateId,
    },
    Since {
        from_candidate_id: CandidateId,
        to_candidate_id: CandidateId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    TypeChanged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: RepoPath,
    pub old_path: Option<RepoPath>,
    pub status: ChangeStatus,
    pub added: u64,
    pub removed: u64,
    pub binary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeList {
    pub objective_id: ObjectiveId,
    pub basis: Basis,
    pub basis_label: String,
    pub target: TargetRef,
    pub capture_id: Option<CaptureId>,
    pub candidate_id: Option<CandidateId>,
    pub files: Vec<ChangedFile>,
    /// The service listed only part of the Change Set.
    pub truncated: bool,
    pub total_added: u64,
    pub total_removed: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiffLineKind {
    Context,
    Add,
    Del,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_no: Option<u64>,
    pub new_no: Option<u64>,
    /// Line content without its terminating `\n` (a CR of a CRLF terminator is retained).
    pub text: String,
    pub new_byte_start: Option<u64>,
    pub old_byte_start: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffHunk {
    pub old_start: u64,
    pub old_lines: u64,
    pub new_start: u64,
    pub new_lines: u64,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    pub objective_id: ObjectiveId,
    pub basis: Basis,
    pub path: RepoPath,
    pub old_digest: Option<ArtifactDigest>,
    pub new_digest: Option<ArtifactDigest>,
    pub binary: bool,
    pub hunks: Vec<DiffHunk>,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Old,
    New,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileEncoding {
    Utf8,
    Binary,
}

/// One `file.read` reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePage {
    pub objective_id: ObjectiveId,
    pub basis: Basis,
    pub path: RepoPath,
    pub side: Side,
    pub artifact_digest: ArtifactDigest,
    pub total_bytes: u64,
    pub offset: u64,
    /// Bytes this page covers (the service's `length`); the next page starts at
    /// `offset + length`, also for binary content that carries no text.
    pub length: u64,
    pub encoding: FileEncoding,
    pub text: Option<String>,
    pub eof: bool,
    pub version_label: String,
}

/// Accumulated, contiguous pages of one file version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileContent {
    pub objective_id: ObjectiveId,
    pub basis: Basis,
    pub path: RepoPath,
    pub side: Side,
    pub artifact_digest: ArtifactDigest,
    pub total_bytes: u64,
    pub encoding: FileEncoding,
    pub text: String,
    /// Bytes represented by `text` starting at offset 0.
    pub loaded_bytes: u64,
    pub eof: bool,
    pub version_label: String,
}

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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ReferenceVersion {
    Working { capture_id: CaptureId },
    Candidate { candidate_id: CandidateId },
}

/// Versioned reference to exactly inspected bytes: `[start, end)` within the side's content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    pub artifact_digest: ArtifactDigest,
    pub path: RepoPath,
    pub version: ReferenceVersion,
    pub side: Side,
    pub byte_start: u64,
    pub byte_end: u64,
    pub selected_text: String,
    pub line_hint: Option<u64>,
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchResults {
    pub scope: SearchScope,
    pub query: String,
    /// Service label of the searched version/scope, e.g. "Content in Candidate 2 of Make retries safe".
    pub scope_label: String,
    /// The basis the search ran against, so following a result never silently switches revision.
    pub basis: Option<Basis>,
    pub objective_id: Option<ObjectiveId>,
    pub results: Vec<SearchResult>,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Connected,
    Reconnecting {
        attempt: u32,
    },
    Disconnected {
        reason: String,
    },
    /// The service could not be reached at all.
    Unavailable {
        reason: String,
    },
    /// The service is running but another LADE window holds its single controlling lease.
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceNotice {
    pub level: NoticeLevel,
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Settings {
    pub default_model: Option<ModelRef>,
    pub available_models: Vec<ModelRef>,
    pub verification_commands: Vec<Vec<String>>,
}

/// Loading state of a queried projection.
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
