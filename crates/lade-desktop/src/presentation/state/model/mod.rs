//! The view model the store holds: plain data converted from the service protocol, grouped by
//! what it describes.

mod activity;
mod ids;
mod objective;
mod review;
mod service;

pub use activity::{
    ActivityItem, ActivityRole, DeliveryMode, DeliveryState, Draft, DraftState, InputSummary,
};
pub use ids::{
    ActivityItemId, ApprovalId, ArtifactDigest, CandidateId, CaptureId, DraftId, GitAlgorithm,
    GitId, InputId, ModelRef, ObjectiveId, OperationId, RepoPath, SessionId, Timestamp,
    VerificationId, WorkspaceId,
};
pub use objective::{
    ActionAvailability, ActionName, ActionSurface, ApprovalState, ApprovalSummary, Attention,
    AvailableAction, Badge, CandidateStatus, CandidateSummary, ChangeCounts, Execution,
    ExecutionState, Intent, Lifecycle, ObjectiveDetail, ObjectiveSummary, Provenance, Refusal,
    SessionInfo, TargetRef, TerminalOpenAction, VerificationState, VerificationSummary, Workspace,
};
pub use review::{
    Basis, ChangeList, ChangeStatus, ChangedFile, ContentNotice, ContentNoticeKind, DiffHunk,
    DiffLine, DiffLineKind, FileContent, FileDiff, FileEncoding, FilePage, Reference,
    ReferenceVersion, Side,
};
pub use service::{
    ConnectionState, Load, NoticeLevel, NoticeScope, ReportCode, SearchResult, SearchResultKind,
    SearchResults, SearchScope, ServiceNotice, Settings,
};
