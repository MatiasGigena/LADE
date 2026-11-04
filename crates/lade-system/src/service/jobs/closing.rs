//! The end of an Objective in the repository: whether it changed anything, removing its
//! worktree and branch, and deleting its retained refs.

use std::fs;
use std::path::Path;

use lade_core::domain::ids::GitId;
use lade_protocol::client::Rejection;

use crate::adapters::git::{GitError, Repository};

use crate::service::convert::{adapter_git, conflict, git_rejection};

use super::read::target_commit;

pub fn change_set_empty(
    repo: &Repository,
    worktree: Option<&Path>,
    scratch: &Path,
    target_branch: &str,
    start_commit: &GitId,
) -> Result<bool, Rejection> {
    let target = target_commit(repo, target_branch)?;
    let tree = match worktree {
        Some(path) => {
            let wt = repo.open_worktree(path).map_err(git_rejection)?;
            repo.capture_working_tree(&wt, scratch)
                .map_err(git_rejection)?
                .tree
        }
        None => adapter_git(start_commit),
    };
    let files = repo
        .diff_numstat(&adapter_git(&target), &tree)
        .map_err(git_rejection)?;
    Ok(files.is_empty())
}

pub fn cleanup(
    repo: &Repository,
    worktree: Option<&Path>,
    git_branch: Option<&str>,
    scratch: &Path,
    force: bool,
) -> Result<(), Rejection> {
    if let Some(path) = worktree {
        match repo.open_worktree(path) {
            Ok(wt) => {
                let mut force = force;
                if !force && let Some(branch) = git_branch {
                    let head = repo
                        .resolve_ref(&format!("refs/heads/{branch}"))
                        .map_err(git_rejection)?;
                    let tree = repo
                        .capture_working_tree(&wt, scratch)
                        .map_err(git_rejection)?
                        .tree;
                    if let Some(head) = head {
                        force = repo
                            .diff_numstat(&head, &tree)
                            .map_err(git_rejection)?
                            .is_empty();
                    }
                }
                match repo.worktree_remove(&wt, force) {
                    Ok(()) => {}
                    Err(GitError::WorktreeDirty) => {
                        return Err(conflict(
                            "worktree has changes not in any candidate; confirm data loss to remove it",
                        ));
                    }
                    Err(e) => return Err(git_rejection(e)),
                }
            }
            Err(GitError::WorktreeNotFound) => {
                let _ = fs::remove_dir_all(path);
            }
            Err(e) => return Err(git_rejection(e)),
        }
    }
    if let Some(branch) = git_branch {
        let refname = format!("refs/heads/{branch}");
        if let Some(current) = repo.resolve_ref(&refname).map_err(git_rejection)? {
            repo.delete_ref_cas(&refname, &current)
                .map_err(git_rejection)?;
        }
    }
    Ok(())
}

pub fn delete_refs(repo: &Repository, refs: &[String]) -> Result<(), Rejection> {
    for refname in refs {
        if let Some(current) = repo.resolve_ref(refname).map_err(git_rejection)? {
            repo.delete_ref_cas(refname, &current)
                .map_err(git_rejection)?;
        }
    }
    Ok(())
}
