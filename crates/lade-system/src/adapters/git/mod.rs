mod capture;
mod command;
mod diff;
mod error;
#[cfg(test)]
mod fixtures;
mod ids;
mod integrate;
mod refs;
mod tree;
mod worktree;

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use command::GitCommand;
use lade_core::domain::ids::{GitAlgorithm, GitId};

pub use capture::{MAX_SKIPPED_NESTED, WorkingCapture, user_excludes_file};
pub use command::{Diagnostics, LADE_IDENTITY_EMAIL, LADE_IDENTITY_NAME, STDERR_LIMIT};
pub use diff::{
    ChangeStatus, ChangedFile, DiffLine, FileDiff, Hunk, LineKind, MAX_DIFF_BLOB_BYTES,
    MAX_DIFF_LINES,
};
pub use error::{GitError, TargetBusyReason};
pub use ids::{GitVersion, RepoPath, empty_tree, parse_id};
pub use integrate::{CheckoutInfo, MergeOutcome, SquashOutcome, SquashRequest, TargetCheckout};
pub use refs::{
    Branch, CommitSpec, escape_branch_name, unescape_branch_name, validate_branch_name,
    validate_refname,
};
pub use tree::{
    BlobPage, GrepMatch, GrepQuery, GrepResults, MAX_BLOB_PAGE, MAX_GREP_RESULTS, ObjectKind,
    TreeEntry,
};
pub use worktree::Worktree;

pub type Result<T> = std::result::Result<T, GitError>;

pub const MIN_GIT_VERSION: GitVersion = GitVersion::new(2, 36, 0);
pub const MIN_MERGE_TREE_VERSION: GitVersion = GitVersion::new(2, 38, 0);
pub const MIN_CHECK_ATTR_SOURCE_VERSION: GitVersion = GitVersion::new(2, 40, 0);
pub const RETENTION_PREFIX: &str = "refs/lade/retain/";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InProgress {
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Bisect,
    Sequencer,
}

#[derive(Clone, Debug)]
pub struct Repository {
    work_tree: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
    format: GitAlgorithm,
    version: GitVersion,
    excludes_file: Option<PathBuf>,
}

impl Repository {
    /// Opens the non-bare repository whose top-level work tree is `path`.
    ///
    /// # Errors
    ///
    /// Returns [`GitError::NotRepository`] when `path` does not exist or is not inside a repository,
    /// [`GitError::BareRepository`] or [`GitError::NotTopLevel`] when it is not the top of a work tree,
    /// [`GitError::UnsupportedVersion`] or [`GitError::UnsupportedObjectFormat`] when the installed
    /// Git or the repository is not supported, and a command error when `git` cannot run.
    pub fn open(path: &Path) -> Result<Self> {
        let top = path.canonicalize().map_err(|_| GitError::NotRepository)?;
        let version = GitCommand::new("version")
            .without_config_scan()
            .cwd(&top)
            .arg("version")
            .run()
            .ok()
            .and_then(|out| GitVersion::parse(&out))
            .ok_or(GitError::Parse {
                op: "version",
                what: "version",
            })?;
        if version < MIN_GIT_VERSION {
            return Err(GitError::UnsupportedVersion {
                found: version,
                required: MIN_GIT_VERSION,
            });
        }
        let probe = GitCommand::new("rev-parse")
            .cwd(&top)
            .read_only()
            .args(["rev-parse", "--is-bare-repository", "--is-inside-work-tree"])
            .output()?;
        if !probe.status.success() {
            return Err(GitError::NotRepository);
        }
        match lines(&probe.stdout).as_slice() {
            [b"true", ..] => return Err(GitError::BareRepository),
            [b"false", b"true"] => {}
            _ => return Err(GitError::NotTopLevel),
        }
        let out = GitCommand::new("rev-parse")
            .cwd(&top)
            .read_only()
            .args([
                "rev-parse",
                "--show-toplevel",
                "--absolute-git-dir",
                "--git-common-dir",
                "--show-object-format",
            ])
            .run()?;
        let [toplevel, git_dir, common_dir, format] = lines(&out)[..] else {
            return Err(GitError::Parse {
                op: "rev-parse",
                what: "layout",
            });
        };
        let as_path = |bytes: &[u8]| PathBuf::from(OsString::from_vec(bytes.to_vec()));
        if as_path(toplevel).canonicalize().ok().as_deref() != Some(top.as_path()) {
            return Err(GitError::NotTopLevel);
        }
        let format = match format {
            b"sha1" => GitAlgorithm::Sha1,
            b"sha256" => GitAlgorithm::Sha256,
            _ => return Err(GitError::UnsupportedObjectFormat),
        };
        let canonical = |p: PathBuf| {
            top.join(p).canonicalize().map_err(|_| GitError::Parse {
                op: "rev-parse",
                what: "git dir",
            })
        };
        Ok(Self {
            git_dir: canonical(as_path(git_dir))?,
            common_dir: canonical(as_path(common_dir))?,
            work_tree: top,
            format,
            version,
            excludes_file: None,
        })
    }

    #[must_use]
    pub fn work_tree(&self) -> &Path {
        &self.work_tree
    }

    #[must_use]
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    #[must_use]
    pub fn common_dir(&self) -> &Path {
        &self.common_dir
    }

    #[must_use]
    pub fn object_format(&self) -> GitAlgorithm {
        self.format
    }

    #[must_use]
    pub fn git_version(&self) -> GitVersion {
        self.version
    }

    pub(crate) fn git(&self, op: &'static str) -> GitCommand {
        GitCommand::new(op)
            .git_dir(&self.git_dir)
            .work_tree(&self.work_tree)
    }

    fn parse_id(&self, op: &'static str, bytes: &[u8]) -> Result<GitId> {
        let text = std::str::from_utf8(bytes.trim_ascii()).map_err(|_| GitError::Parse {
            op,
            what: "object id",
        })?;
        parse_id(self.format, text).map_err(|_| GitError::Parse {
            op,
            what: "object id",
        })
    }

    fn check_id(&self, id: &GitId) -> Result<()> {
        if id.algorithm() == self.format {
            Ok(())
        } else {
            Err(GitError::InvalidId)
        }
    }
}

fn lines(bytes: &[u8]) -> Vec<&[u8]> {
    bytes
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .collect()
}

fn unique_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    format!("{}-{nanos}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

pub(super) struct RemoveOnDrop(Vec<PathBuf>);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}
