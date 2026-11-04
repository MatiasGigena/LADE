use std::fmt;
use std::io;
use std::path::PathBuf;

use super::command::Diagnostics;
use super::{GitId, GitVersion, InProgress, RepoPath};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetBusyReason {
    InProgress(Vec<InProgress>),
    IndexLocked,
    MultipleCheckouts,
}

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
    StaleRef {
        refname: String,
        current: Option<GitId>,
    },
    WorktreeNotFound,
    WorktreeDirty,
    TargetMoved {
        current: Option<GitId>,
    },
    TargetDirty {
        worktree: PathBuf,
    },
    TargetBusy {
        worktree: PathBuf,
        reason: TargetBusyReason,
    },
    TargetUntracked {
        worktree: PathBuf,
        paths: Vec<RepoPath>,
        stderr: Diagnostics,
    },
    TargetWorktreeUnavailable {
        worktree: PathBuf,
    },
    CheckoutFailed {
        worktree: PathBuf,
        reverted: bool,
        stderr: Diagnostics,
    },
}

impl GitError {
    #[must_use]
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
            Self::TargetUntracked { .. } => "target_untracked",
            Self::TargetWorktreeUnavailable { .. } => "target_worktree_unavailable",
            Self::CheckoutFailed { .. } => "checkout_failed",
        }
    }

    #[must_use]
    pub fn stderr(&self) -> Option<&Diagnostics> {
        match self {
            Self::Failed { stderr, .. }
            | Self::CheckoutFailed { stderr, .. }
            | Self::TargetUntracked { stderr, .. } => Some(stderr),
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
