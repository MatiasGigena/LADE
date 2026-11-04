use std::fs;
use std::path::{Path, PathBuf};

use super::{
    ChangeStatus, CommitSpec, GitError, GitId, InProgress, MIN_MERGE_TREE_VERSION, RepoPath,
    Repository, Result, TargetBusyReason, Worktree, validate_branch_name,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeOutcome {
    Clean { tree: GitId },
    Conflict { tree: GitId, paths: Vec<RepoPath> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckoutInfo {
    pub path: PathBuf,
    pub worktree: Option<Worktree>,
    pub dirty: bool,
    pub untracked: bool,
    pub in_progress: Vec<InProgress>,
    pub index_locked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetCheckout {
    pub branch: String,
    pub commit: Option<GitId>,
    pub checkouts: Vec<CheckoutInfo>,
}

#[derive(Clone, Debug)]
pub struct SquashRequest {
    pub target_branch: String,
    pub expected_target: GitId,
    pub candidate_tree: GitId,
    pub message: String,
    pub timestamp: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SquashOutcome {
    pub commit: GitId,
    pub updated_worktree: Option<PathBuf>,
}

impl Repository {
    /// Merges `candidate` into `target` without touching any work tree, reporting either the
    /// clean result tree or the conflicting paths.
    ///
    /// # Errors
    ///
    /// Returns [`GitError::UnsupportedVersion`] when the installed Git has no `merge-tree
    /// --write-tree`, a command error when it fails, or [`GitError::Parse`] when its output is
    /// malformed.
    pub fn merge_tree(&self, target: &GitId, candidate: &GitId) -> Result<MergeOutcome> {
        if self.git_version() < MIN_MERGE_TREE_VERSION {
            return Err(GitError::UnsupportedVersion {
                found: self.git_version(),
                required: MIN_MERGE_TREE_VERSION,
            });
        }
        let out = self
            .git("merge-tree")
            .args([
                "merge-tree",
                "--write-tree",
                "-z",
                "--name-only",
                "--no-messages",
                target.hex(),
                candidate.hex(),
            ])
            .output()?;
        let code = out.status.code();
        if !matches!(code, Some(0 | 1)) {
            return Err(GitError::Failed {
                op: "merge-tree",
                exit_code: code,
                stderr: out.stderr,
            });
        }
        let mut fields = out.stdout.split(|b| *b == 0);
        let tree = self.parse_id("merge-tree", fields.next().unwrap_or_default())?;
        if code == Some(0) {
            return Ok(MergeOutcome::Clean { tree });
        }
        let mut paths = Vec::new();
        for field in fields.take_while(|f| !f.is_empty()) {
            let path = RepoPath::from_git("merge-tree", field)?;
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        Ok(MergeOutcome::Conflict { tree, paths })
    }

    /// Where `branch` points and which work trees have it checked out, with their state.
    ///
    /// # Errors
    ///
    /// Returns [`GitError::InvalidRefName`] for an invalid branch name, or a command error when the ref
    /// or the work tree list cannot be read.
    pub fn target_checkout_state(&self, branch: &str) -> Result<TargetCheckout> {
        validate_branch_name(branch)?;
        let refname = format!("refs/heads/{branch}");
        let commit = self.resolve_ref(&refname)?;
        let entries = self.worktree_entries()?;
        let mut checkouts = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            if entry.bare || entry.branch.as_deref() != Some(refname.as_bytes()) {
                continue;
            }
            let worktree = match entry.path.canonicalize() {
                Ok(path) if !entry.prunable => {
                    let admin = if index == 0 {
                        Some(self.common_dir().to_path_buf())
                    } else {
                        self.linked_admin_dir(&path)
                    };
                    admin.map(|admin_dir| Worktree {
                        path: path.clone(),
                        admin_dir,
                    })
                }
                _ => None,
            };
            let mut info = CheckoutInfo {
                path: entry.path.clone(),
                worktree: worktree.clone(),
                dirty: false,
                untracked: false,
                in_progress: Vec::new(),
                index_locked: false,
            };
            if let Some(wt) = &worktree {
                info.in_progress = in_progress(wt.admin_dir());
                info.index_locked = wt.admin_dir().join("index.lock").exists();
                (info.dirty, info.untracked) = worktree_status(wt)?;
            }
            checkouts.push(info);
        }
        Ok(TargetCheckout {
            branch: branch.to_owned(),
            commit,
            checkouts,
        })
    }

    /// Squash-commits the candidate tree onto the target branch and updates the one work tree
    /// that has it checked out.
    ///
    /// # Errors
    ///
    /// Returns [`GitError::TargetMoved`] when the branch no longer points at the expected commit,
    /// [`GitError::TargetBusy`], [`GitError::TargetDirty`] or [`GitError::TargetUntracked`] when the
    /// checked-out work tree cannot take the update, [`GitError::CheckoutFailed`] (with whether the ref
    /// was moved back) when updating it fails after the ref moved, or a command error.
    pub fn integrate_squash(&self, request: &SquashRequest) -> Result<SquashOutcome> {
        let state = self.target_checkout_state(&request.target_branch)?;
        let refname = format!("refs/heads/{}", request.target_branch);
        if state.commit.as_ref() != Some(&request.expected_target) {
            return Err(GitError::TargetMoved {
                current: state.commit,
            });
        }
        let worktree = match state.checkouts.as_slice() {
            [] => None,
            [only] => Some(checked_out_worktree(only)?),
            [first, ..] => {
                return Err(GitError::TargetBusy {
                    worktree: first.path.clone(),
                    reason: TargetBusyReason::MultipleCheckouts,
                });
            }
        };
        if let Some(worktree) = &worktree {
            self.preflight_checkout(worktree, request)?;
        }
        let commit = self.commit_tree(&CommitSpec {
            tree: request.candidate_tree.clone(),
            parents: vec![request.expected_target.clone()],
            message: request.message.clone(),
            timestamp: request.timestamp,
        })?;
        self.update_ref_cas(&refname, &commit, Some(&request.expected_target))
            .map_err(|e| match e {
                GitError::StaleRef { current, .. } => GitError::TargetMoved { current },
                other => other,
            })?;
        crate::fault::point("integrate.ref_moved");
        let Some(worktree) = worktree else {
            return Ok(SquashOutcome {
                commit,
                updated_worktree: None,
            });
        };
        let checkout = worktree
            .git("read-tree")
            .args([
                "read-tree",
                "-m",
                "-u",
                request.expected_target.hex(),
                commit.hex(),
            ])
            .output();
        match checkout {
            Ok(out) if out.status.success() => Ok(SquashOutcome {
                commit,
                updated_worktree: Some(worktree.path().to_path_buf()),
            }),
            failed => {
                let reverted = self
                    .update_ref_cas(&refname, &request.expected_target, Some(&commit))
                    .is_ok();
                Err(GitError::CheckoutFailed {
                    worktree: worktree.path().to_path_buf(),
                    reverted,
                    stderr: match failed {
                        Ok(out) => out.stderr,
                        Err(e) => e.stderr().cloned().unwrap_or_default(),
                    },
                })
            }
        }
    }

    /// Updates the work tree that has `branch` checked out from `expected_target` to
    /// `commit`, for an integration whose ref moved before a restart.
    ///
    /// # Errors
    ///
    /// Returns [`GitError::TargetMoved`] when `branch` is not at `commit`,
    /// [`GitError::TargetBusy`] or [`GitError::TargetWorktreeUnavailable`] when the work tree cannot be
    /// updated, [`GitError::CheckoutFailed`] when `git read-tree` fails, or a command error.
    pub fn finish_integration_checkout(
        &self,
        branch: &str,
        expected_target: &GitId,
        commit: &GitId,
    ) -> Result<Option<PathBuf>> {
        let state = self.target_checkout_state(branch)?;
        if state.commit.as_ref() != Some(commit) {
            return Err(GitError::TargetMoved {
                current: state.commit,
            });
        }
        let info = match state.checkouts.as_slice() {
            [] => return Ok(None),
            [only] => only,
            [first, ..] => {
                return Err(GitError::TargetBusy {
                    worktree: first.path.clone(),
                    reason: TargetBusyReason::MultipleCheckouts,
                });
            }
        };
        let worktree =
            info.worktree
                .clone()
                .ok_or_else(|| GitError::TargetWorktreeUnavailable {
                    worktree: info.path.clone(),
                })?;
        if !info.in_progress.is_empty() || info.index_locked {
            return Err(GitError::TargetBusy {
                worktree: info.path.clone(),
                reason: if info.index_locked {
                    TargetBusyReason::IndexLocked
                } else {
                    TargetBusyReason::InProgress(info.in_progress.clone())
                },
            });
        }
        let out = worktree
            .git("read-tree")
            .args(["read-tree", "-m", "-u", expected_target.hex(), commit.hex()])
            .output()?;
        if out.status.success() {
            Ok(Some(worktree.path().to_path_buf()))
        } else {
            Err(GitError::CheckoutFailed {
                worktree: worktree.path().to_path_buf(),
                reverted: false,
                stderr: out.stderr,
            })
        }
    }
}

impl Repository {
    fn preflight_checkout(&self, worktree: &Worktree, request: &SquashRequest) -> Result<()> {
        let out = worktree
            .git("read-tree-preflight")
            .args([
                "read-tree",
                "-m",
                "-u",
                "-n",
                request.expected_target.hex(),
                request.candidate_tree.hex(),
            ])
            .output()?;
        if out.status.success() {
            return Ok(());
        }
        let mut paths = Vec::new();
        for file in self.diff_numstat(&request.expected_target, &request.candidate_tree)? {
            let adds = matches!(
                file.status,
                ChangeStatus::Added | ChangeStatus::Renamed { .. } | ChangeStatus::Copied { .. }
            );
            let on_disk = worktree.path().join(file.path.as_os_str());
            if adds && fs::symlink_metadata(&on_disk).is_ok() && paths.len() < MAX_REPORTED_PATHS {
                paths.push(file.path);
            }
        }
        Err(GitError::TargetUntracked {
            worktree: worktree.path().to_path_buf(),
            paths,
            stderr: out.stderr,
        })
    }
}

const MAX_REPORTED_PATHS: usize = 32;

fn checked_out_worktree(info: &CheckoutInfo) -> Result<Worktree> {
    let worktree = info
        .worktree
        .clone()
        .ok_or_else(|| GitError::TargetWorktreeUnavailable {
            worktree: info.path.clone(),
        })?;
    let busy = |reason| GitError::TargetBusy {
        worktree: info.path.clone(),
        reason,
    };
    if !info.in_progress.is_empty() {
        return Err(busy(TargetBusyReason::InProgress(info.in_progress.clone())));
    }
    if info.index_locked {
        return Err(busy(TargetBusyReason::IndexLocked));
    }
    if info.dirty {
        return Err(GitError::TargetDirty {
            worktree: info.path.clone(),
        });
    }
    Ok(worktree)
}

fn in_progress(admin_dir: &Path) -> Vec<InProgress> {
    [
        ("MERGE_HEAD", InProgress::Merge),
        ("rebase-merge", InProgress::Rebase),
        ("rebase-apply", InProgress::Rebase),
        ("CHERRY_PICK_HEAD", InProgress::CherryPick),
        ("REVERT_HEAD", InProgress::Revert),
        ("BISECT_LOG", InProgress::Bisect),
        ("sequencer", InProgress::Sequencer),
    ]
    .into_iter()
    .filter(|(name, _)| admin_dir.join(name).exists())
    .fold(Vec::new(), |mut acc, (_, state)| {
        if !acc.contains(&state) {
            acc.push(state);
        }
        acc
    })
}

/// Whether `worktree` has tracked changes and whether it has untracked files.
fn worktree_status(worktree: &Worktree) -> Result<(bool, bool)> {
    let out = worktree
        .git("status")
        .read_only()
        .args([
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=normal",
            "--ignore-submodules=none",
            "--no-renames",
        ])
        .run()?;
    let mut dirty = false;
    let mut untracked = false;
    for record in out.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        if record.starts_with(b"?? ") {
            untracked = true;
        } else {
            dirty = true;
        }
    }
    Ok((dirty, untracked))
}

#[cfg(test)]
mod tests;
