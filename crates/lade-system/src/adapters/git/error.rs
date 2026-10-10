use std::fmt;
use std::io;
use std::path::PathBuf;

use super::command::Diagnostics;
use super::{GitId, GitVersion, InProgress};

/// Why a checked-out Integration Target cannot be updated right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetBusyReason {
    /// A merge, rebase, cherry-pick, revert, bisect or sequencer operation is in progress.
    InProgress(Vec<InProgress>),
    /// Another Git process holds `index.lock` in that worktree.
    IndexLocked,
    /// The branch is checked out in more than one worktree.
    MultipleCheckouts,
}

/// Typed Git adapter failure. `Display` is a short code (plus a static operation label); stderr is
/// kept separately, truncated to [`super::STDERR_LIMIT`] bytes, for diagnostics only.
#[derive(Debug)]
pub enum GitError {
    Spawn {
        op: &'static str,
        kind: io::ErrorKind,
    },
    Io {
        op: &'static str,
        kind: io::ErrorKind,
    },
    Failed {
        op: &'static str,
        exit_code: Option<i32>,
        stderr: Diagnostics,
    },
    OutputLimit {
        op: &'static str,
        limit: usize,
    },
    Parse {
        op: &'static str,
        what: &'static str,
    },
    NotRepository,
    NotTopLevel,
    BareRepository,
    UnsupportedObjectFormat,
    UnsupportedVersion {
        found: GitVersion,
        required: GitVersion,
    },
    InvalidRefName,
    InvalidPath,
    InvalidId,
    InvalidArgument {
        what: &'static str,
    },
    NotFound {
        what: &'static str,
    },
    NotABlob,
    /// Compare-and-swap reference update rejected: the reference no longer has the expected value.
    StaleRef {
        refname: String,
        current: Option<GitId>,
    },
    WorktreeNotFound,
    /// `worktree_remove` without force found modified or untracked content.
    WorktreeDirty,
    /// The Integration Target no longer points at the expected commit.
    TargetMoved {
        current: Option<GitId>,
    },
    /// The Integration Target is checked out with uncommitted tracked changes.
    TargetDirty {
        worktree: PathBuf,
    },
    TargetBusy {
        worktree: PathBuf,
        reason: TargetBusyReason,
    },
    /// The worktree that has the target checked out is missing on disk (prunable).
    TargetWorktreeUnavailable {
        worktree: PathBuf,
    },
    /// The ref was updated but the checked-out worktree could not be brought along.
    /// `reverted` reports whether the compare-and-swap revert of the ref succeeded.
    CheckoutFailed {
        worktree: PathBuf,
        reverted: bool,
        stderr: Diagnostics,
    },
}

impl GitError {
    /// Stable short code for logs and protocol mapping.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Spawn { .. } => "git_spawn_failed",
            Self::Io { .. } => "git_io",
            Self::Failed { .. } => "git_failed",
            Self::OutputLimit { .. } => "git_output_limit",
            Self::Parse { .. } => "git_parse",
            Self::NotRepository => "not_repository",
            Self::NotTopLevel => "not_top_level",
            Self::BareRepository => "bare_repository",
            Self::UnsupportedObjectFormat => "unsupported_object_format",
            Self::UnsupportedVersion { .. } => "unsupported_git_version",
            Self::InvalidRefName => "invalid_ref_name",
            Self::InvalidPath => "invalid_path",
            Self::InvalidId => "invalid_id",
            Self::InvalidArgument { .. } => "invalid_argument",
            Self::NotFound { .. } => "not_found",
            Self::NotABlob => "not_a_blob",
            Self::StaleRef { .. } => "stale_ref",
            Self::WorktreeNotFound => "worktree_not_found",
            Self::WorktreeDirty => "worktree_dirty",
            Self::TargetMoved { .. } => "target_moved",
            Self::TargetDirty { .. } => "target_dirty",
            Self::TargetBusy { .. } => "target_busy",
            Self::TargetWorktreeUnavailable { .. } => "target_worktree_unavailable",
            Self::CheckoutFailed { .. } => "checkout_failed",
        }
    }

    /// Truncated stderr of the failing Git process, when there was one.
    pub fn stderr(&self) -> Option<&Diagnostics> {
        match self {
            Self::Failed { stderr, .. } | Self::CheckoutFailed { stderr, .. } => Some(stderr),
            _ => None,
        }
    }
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())?;
        match self {
            Self::Spawn { op, .. }
            | Self::Io { op, .. }
            | Self::OutputLimit { op, .. }
            | Self::Parse { op, .. } => write!(f, " ({op})"),
            Self::Failed { op, exit_code, .. } => match exit_code {
                Some(code) => write!(f, " ({op}, exit {code})"),
                None => write!(f, " ({op}, signal)"),
            },
            Self::UnsupportedVersion { found, required } => {
                write!(f, " (found {found}, requires {required})")
            }
            Self::InvalidArgument { what } | Self::NotFound { what } => write!(f, " ({what})"),
            _ => Ok(()),
        }
    }
}

impl std::error::Error for GitError {}
