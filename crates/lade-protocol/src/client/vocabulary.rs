//! Closed wire vocabularies: error codes, safe actions, states, badges and action names.

use super::RequestName;
use serde::{Deserialize, Serialize};

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
wire_enum!(TerminalMode {
    ReadWrite,
    ReadOnly
});
wire_enum!(TerminalCloseReason {
    Exited,
    Closed,
    ReviewFreeze,
    Lifecycle,
    Failed,
});
wire_enum!(StopReason { Exit, Logout });
wire_enum!(NoticeLevel {
    Info,
    Warning,
    Error,
});

macro_rules! notice_codes {
    ($($variant:ident = $code:literal,)*) => {
        /// What a `service.notice` reports; `as_str` is the wire code.
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
        pub enum NoticeCode { $(#[serde(rename = $code)] $variant),* }
        impl NoticeCode {
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $code),* }
            }
        }
    };
}
notice_codes! {
    LeaseHeld = "lease_held",
    LinkOverflow = "link_overflow",
    Recovery = "recovery",
    ExecutionFailed = "execution_failed",
    LimitReached = "limit_reached",
    WorkerError = "worker_error",
    WorkerProtocolViolation = "worker_protocol_violation",
    SessionCommandTimeout = "session_command_timeout",
    CandidateInvalidated = "candidate_invalidated",
    StorageOk = "storage_ok",
    StorageLow = "storage_low",
    StorageCritical = "storage_critical",
}

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
    #[serde(rename = "objective.delete_history")]
    ObjectiveDeleteHistory,
    /// Opening a contained terminal: the reason says why not (no worktree yet, closed, no sandbox).
    #[serde(rename = "terminal.open")]
    TerminalOpen,
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
            ActionName::ObjectiveDeleteHistory => Self::ObjectiveDeleteHistory,
            ActionName::TerminalOpen => Self::TerminalOpen,
        }
    }
}
