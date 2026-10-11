//! Merge preview and protected squash integration into the Integration Target.
//!
//! Integration never uses `reset --hard`, `stash` or `checkout`: the target ref moves by
//! compare-and-swap and a checked-out target worktree is brought along with a two-tree
//! `read-tree -m -u`, which refuses to overwrite local modifications or untracked files. If that
//! fails, the ref is moved back by compare-and-swap.

use std::path::{Path, PathBuf};

use super::{
    CommitSpec, GitError, GitId, InProgress, MIN_MERGE_TREE_VERSION, RepoPath, Repository, Result,
    TargetBusyReason, Worktree, validate_branch_name,
};

/// Result of a merge preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeOutcome {
    Clean {
        tree: GitId,
    },
    /// `tree` contains conflict markers; `paths` lists conflicted paths once each.
    Conflict {
        tree: GitId,
        paths: Vec<RepoPath>,
    },
}

/// One worktree that has the target branch checked out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckoutInfo {
    pub path: PathBuf,
    /// `None` when the worktree directory is missing (prunable).
    pub worktree: Option<Worktree>,
    /// Tracked changes (staged or unstaged) relative to `HEAD`.
    pub dirty: bool,
    /// Untracked, non-ignored files exist (not a refusal: `read-tree -u` never overwrites them).
    pub untracked: bool,
    pub in_progress: Vec<InProgress>,
    pub index_locked: bool,
}

/// Where and how the Integration Target is checked out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetCheckout {
    pub branch: String,
    pub commit: Option<GitId>,
    pub checkouts: Vec<CheckoutInfo>,
}

/// Input to [`Repository::integrate_squash`].
#[derive(Clone, Debug)]
pub struct SquashRequest {
    pub target_branch: String,
    /// The target commit the approval was bound to.
    pub expected_target: GitId,
    /// The approved candidate tree (already merged onto `expected_target`).
    pub candidate_tree: GitId,
    pub message: String,
    pub timestamp: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SquashOutcome {
    pub commit: GitId,
    /// The worktree whose files and index were updated, when the target was checked out.
    pub updated_worktree: Option<PathBuf>,
}

impl Repository {
    /// Three-way merge preview of two commits without touching any ref, index or worktree.
    pub fn merge_tree(&self, target: &GitId, candidate: &GitId) -> Result<MergeOutcome> {
        if self.git_version() < MIN_MERGE_TREE_VERSION {
            return Err(GitError::UnsupportedVersion {
                found: self.git_version(),
                required: MIN_MERGE_TREE_VERSION,
            });
        }
        let overrides = Self::driver_overrides(self.git_dir(), self.work_tree())?;
        let out = self
            .git("merge-tree")
            .configs(&overrides)
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

    /// Which worktrees have `branch` checked out, and whether each is clean and idle.
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
                (info.dirty, info.untracked) = self.worktree_status(wt)?;
            }
            checkouts.push(info);
        }
        Ok(TargetCheckout {
            branch: branch.to_owned(),
            commit,
            checkouts,
        })
    }

    fn worktree_status(&self, worktree: &Worktree) -> Result<(bool, bool)> {
        let overrides = Self::driver_overrides(worktree.admin_dir(), worktree.path())?;
        let out = worktree
            .git("status")
            .read_only()
            .configs(&overrides)
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

    /// Squash-integrate an approved candidate tree onto the target branch.
    ///
    /// 1. The target ref must equal `expected_target` ([`GitError::TargetMoved`] otherwise).
    /// 2. If the target is checked out, that worktree must exist, be clean (tracked content), hold
    ///    no `index.lock` and have no merge/rebase/cherry-pick/revert/bisect in progress; otherwise
    ///    a typed refusal is returned and nothing is changed.
    /// 3. A commit with the fixed LADE identity is created (`candidate_tree`, parent
    ///    `expected_target`) and the ref is moved by compare-and-swap.
    /// 4. A checked-out worktree is updated with `read-tree -m -u <old> <new>`; on failure the ref
    ///    is moved back by compare-and-swap and [`GitError::CheckoutFailed`] is returned.
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
        let checkout =
            Self::driver_overrides(worktree.admin_dir(), worktree.path()).and_then(|overrides| {
                worktree
                    .git("read-tree")
                    .configs(&overrides)
                    .args([
                        "read-tree",
                        "-m",
                        "-u",
                        request.expected_target.hex(),
                        commit.hex(),
                    ])
                    .output()
            });
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

    /// Completes the checkout step of a squash integration whose ref move survived a crash of
    /// the integrating process (IMP-013 recovery): when `branch` holds `commit` and is checked
    /// out in exactly one idle worktree, runs `read-tree -m -u <expected_target> <commit>` there.
    /// The two-tree merge keeps index entries that already match `commit` and refuses to
    /// overwrite local modifications, so a completed or locally edited checkout is never
    /// damaged; calling it again is harmless. Returns the updated worktree, or `None` when the
    /// target is not checked out. The ref is never moved back here.
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
        let overrides = Self::driver_overrides(worktree.admin_dir(), worktree.path())?;
        let out = worktree
            .git("read-tree")
            .configs(&overrides)
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
