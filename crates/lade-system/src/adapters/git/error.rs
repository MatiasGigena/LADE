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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn every_variant() -> Vec<GitError> {
        let path = PathBuf::from("/repo");
        let version = |minor| GitVersion::new(2, minor, 0);
        vec![
            GitError::Spawn {
                op: "status",
                kind: io::ErrorKind::NotFound,
            },
            GitError::Io {
                op: "read",
                kind: io::ErrorKind::BrokenPipe,
            },
            GitError::Failed {
                op: "commit-tree",
                exit_code: Some(128),
                stderr: Diagnostics::default(),
            },
            GitError::OutputLimit {
                op: "diff",
                limit: 1,
            },
            GitError::Parse {
                op: "version",
                what: "version",
            },
            GitError::NotRepository,
            GitError::NotTopLevel,
            GitError::BareRepository,
            GitError::UnsupportedObjectFormat,
            GitError::UnsupportedVersion {
                found: version(30),
                required: version(36),
            },
            GitError::InvalidRefName,
            GitError::InvalidPath,
            GitError::InvalidId,
            GitError::InvalidArgument { what: "query" },
            GitError::NotFound { what: "branch" },
            GitError::NotABlob,
            GitError::StaleRef {
                refname: "refs/heads/main".into(),
                current: None,
            },
            GitError::WorktreeNotFound,
            GitError::WorktreeDirty,
            GitError::TargetMoved { current: None },
            GitError::TargetDirty {
                worktree: path.clone(),
            },
            GitError::TargetBusy {
                worktree: path.clone(),
                reason: TargetBusyReason::IndexLocked,
            },
            GitError::TargetUntracked {
                worktree: path.clone(),
                paths: Vec::new(),
                stderr: Diagnostics::default(),
            },
            GitError::TargetWorktreeUnavailable {
                worktree: path.clone(),
            },
            GitError::CheckoutFailed {
                worktree: path,
                reverted: true,
                stderr: Diagnostics::default(),
            },
        ]
    }

    #[test]
    fn every_error_has_its_own_stable_code() {
        let errors = every_variant();
        let codes: HashSet<_> = errors.iter().map(GitError::code).collect();
        assert_eq!(codes.len(), errors.len(), "codes are distinct");
        assert!(
            codes
                .iter()
                .all(|c| c.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
        );
    }

    #[test]
    fn messages_start_with_the_code_and_name_the_operation_never_the_output() {
        for error in every_variant() {
            assert!(error.to_string().starts_with(error.code()), "{error}");
        }
        let failed = GitError::Failed {
            op: "commit-tree",
            exit_code: Some(128),
            stderr: Diagnostics::default(),
        };
        assert_eq!(failed.to_string(), "git_failed (commit-tree, exit 128)");
        let signalled = GitError::Failed {
            op: "gc",
            exit_code: None,
            stderr: Diagnostics::default(),
        };
        assert_eq!(signalled.to_string(), "git_failed (gc, signal)");
        let old = GitError::UnsupportedVersion {
            found: GitVersion::new(2, 30, 1),
            required: GitVersion::new(2, 36, 0),
        };
        assert_eq!(
            old.to_string(),
            "unsupported_git_version (found 2.30.1, requires 2.36.0)"
        );
        assert_eq!(
            GitError::NotFound { what: "branch" }.to_string(),
            "not_found (branch)"
        );
        assert_eq!(GitError::WorktreeDirty.to_string(), "worktree_dirty");
    }

    #[test]
    fn only_command_failures_carry_diagnostics() {
        let with_stderr: Vec<_> = every_variant()
            .into_iter()
            .filter(|e| e.stderr().is_some())
            .map(|e| e.code())
            .collect();
        assert_eq!(
            with_stderr,
            ["git_failed", "target_untracked", "checkout_failed"]
        );
    }
}
