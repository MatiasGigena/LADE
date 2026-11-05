//! Captures of the working tree and the Candidates frozen and refreshed from it, with the
//! manifest that names their exact contents.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use lade_core::domain::ids::{CaptureId, DraftId, GitId};
use lade_core::domain::inspection::Reference;
use lade_core::domain::review::ChangeCounts;
use lade_protocol::client::Rejection;
use lade_protocol::types::WirePath;

use crate::adapters::artifacts::ArtifactStore;
use crate::adapters::git::{self, ChangedFile, CommitSpec, MergeOutcome, Repository};

use crate::service::convert::{
    adapter_git, conflict, core_git, domain_rejection, durability, git_rejection,
};
use crate::service::projection::actions::NO_CHANGES_TO_FREEZE;

use super::read::{counts, reference_stale, target_commit};

pub fn manifest(target: &GitId, tree: &GitId, files: &[ChangedFile]) -> Vec<u8> {
    let mut out = format!(
        "lade-manifest 1\ntarget {}\ntree {}\n",
        target.hex(),
        tree.hex()
    );
    for file in files {
        let id = |id: &Option<git::GitId>| {
            id.as_ref()
                .map_or_else(|| "-".to_string(), |i| i.hex().to_string())
        };
        let path = WirePath::from_bytes(file.path.as_bytes().to_vec())
            .map_or_else(|_| "?".into(), |p| p.b64());
        let old = file
            .old_path
            .as_ref()
            .and_then(|p| WirePath::from_bytes(p.as_bytes().to_vec()).ok())
            .map_or_else(|| "-".to_string(), |p| p.b64());
        let _ = writeln!(
            out,
            "{:?}\t{:o}\t{:o}\t{}\t{}\t{}\t{}\t{}\t{}",
            file.status,
            file.old_mode,
            file.new_mode,
            id(&file.old_id),
            id(&file.new_id),
            file.added,
            file.removed,
            old,
            path
        );
    }
    out.into_bytes()
}

/// The newest registered capture, reused when the worktree and target have not moved.
pub struct LatestCapture {
    pub id: CaptureId,
    pub tree: GitId,
    pub target: GitId,
}

pub struct CaptureInput {
    pub repo: Repository,
    pub worktree: PathBuf,
    pub scratch: PathBuf,
    pub target_branch: String,
    pub capture_id: CaptureId,
    pub latest: Option<LatestCapture>,
    pub drafts: Vec<(DraftId, Reference)>,
}

#[derive(Debug)]
pub struct CaptureOut {
    pub capture_id: CaptureId,
    pub reused: bool,
    pub tree: GitId,
    pub target: GitId,
    pub files: Vec<ChangedFile>,
    pub stale: Vec<(DraftId, bool)>,
    pub skipped_nested: Vec<git::RepoPath>,
    pub filtered: Vec<git::RepoPath>,
}

pub fn capture(input: &CaptureInput) -> Result<CaptureOut, Rejection> {
    let repo = &input.repo;
    let worktree = repo.open_worktree(&input.worktree).map_err(git_rejection)?;
    let captured = repo
        .capture_working_tree(&worktree, &input.scratch)
        .map_err(git_rejection)?;
    let tree = core_git(&captured.tree);
    let target = target_commit(repo, &input.target_branch)?;
    let files = repo
        .diff_numstat(&adapter_git(&target), &captured.tree)
        .map_err(git_rejection)?;
    let filtered = filtered_paths(repo, &captured.tree, &files)?;
    let (capture_id, reused) = match &input.latest {
        Some(latest) if latest.tree == tree && latest.target == target => (latest.id, true),
        _ => {
            repo.retain(
                &format!("capture-{}", input.capture_id),
                &adapter_git(&tree),
            )
            .map_err(git_rejection)?;
            crate::fault::point("capture.retained");
            (input.capture_id, false)
        }
    };
    let stale = input
        .drafts
        .iter()
        .map(|(id, reference)| (*id, reference_stale(repo, &target, &tree, reference)))
        .collect();
    Ok(CaptureOut {
        capture_id,
        reused,
        tree,
        target,
        files,
        stale,
        skipped_nested: captured.skipped_nested,
        filtered,
    })
}

pub fn filtered_paths(
    repo: &Repository,
    tree: &git::GitId,
    files: &[ChangedFile],
) -> Result<Vec<git::RepoPath>, Rejection> {
    let paths: Vec<&git::RepoPath> = files.iter().map(|f| &f.path).collect();
    repo.filtered_paths(tree, &paths).map_err(git_rejection)
}

pub struct FreezeInput {
    pub repo: Repository,
    pub artifacts: Arc<ArtifactStore>,
    pub worktree: PathBuf,
    pub scratch: PathBuf,
    pub target_branch: String,
    pub start_commit: GitId,
    pub git_branch: String,
    pub candidate_ref: String,
    pub message: String,
}

#[derive(Debug)]
pub struct FreezeOut {
    pub commit: GitId,
    pub tree: GitId,
    pub target: GitId,
    pub counts: ChangeCounts,
    pub manifest_digest: String,
    pub changed: bool,
}

pub fn freeze(input: &FreezeInput) -> Result<FreezeOut, Rejection> {
    let repo = &input.repo;
    let worktree = repo.open_worktree(&input.worktree).map_err(git_rejection)?;
    let captured = repo
        .capture_working_tree(&worktree, &input.scratch)
        .map_err(git_rejection)?
        .tree;
    let target = target_commit(repo, &input.target_branch)?;
    let commit = repo
        .commit_tree(&CommitSpec {
            tree: captured.clone(),
            parents: vec![adapter_git(&input.start_commit)],
            message: input.message.clone(),
            timestamp: None,
        })
        .map_err(git_rejection)?;
    let tree = if input.start_commit == target {
        captured.clone()
    } else {
        match repo
            .merge_tree(&adapter_git(&target), &commit)
            .map_err(git_rejection)?
        {
            MergeOutcome::Clean { tree } => tree,
            MergeOutcome::Conflict { .. } => {
                return Err(conflict(
                    "the work conflicts with the moved integration target; continue the agent work first",
                ));
            }
        }
    };
    let files = repo
        .diff_numstat(&adapter_git(&target), &tree)
        .map_err(git_rejection)?;
    if files.is_empty() {
        return Err(domain_rejection(lade_core::DomainError::InvalidLifecycle(
            NO_CHANGES_TO_FREEZE,
        )));
    }
    let digest = input
        .artifacts
        .put(&manifest(&target, &core_git(&tree), &files))
        .map_err(|_| durability("manifest artifact could not be stored"))?;
    let branch_ref = format!("refs/heads/{}", input.git_branch);
    let old = repo.resolve_ref(&branch_ref).map_err(git_rejection)?;
    repo.update_ref_cas(&branch_ref, &commit, old.as_ref())
        .map_err(git_rejection)?;
    crate::fault::point("freeze.branch_moved");
    repo.retain(&input.candidate_ref, &commit)
        .map_err(git_rejection)?;
    if let Err(e) = repo.sync_worktree_index(&worktree, &input.git_branch, &commit) {
        eprintln!(
            "laded: worktree index not refreshed after freeze: {}",
            e.code()
        );
    }
    if tree != captured {
        repo.retain(&format!("{}-tree", input.candidate_ref), &tree)
            .map_err(git_rejection)?;
    }
    let again = repo
        .capture_working_tree(&worktree, &input.scratch)
        .map_err(git_rejection)?
        .tree;
    Ok(FreezeOut {
        commit: core_git(&commit),
        tree: core_git(&tree),
        target,
        counts: counts(&files),
        manifest_digest: digest.to_hex(),
        changed: again != captured,
    })
}

pub struct RefreshInput {
    pub repo: Repository,
    pub artifacts: Arc<ArtifactStore>,
    pub target_branch: String,
    pub candidate_commit: GitId,
    pub retain_ref: String,
}

#[derive(Debug)]
pub enum RefreshOut {
    Rebuilt {
        tree: GitId,
        target: GitId,
        counts: ChangeCounts,
        manifest_digest: String,
    },
    Conflict {
        target: GitId,
    },
}

pub fn refresh(input: &RefreshInput) -> Result<RefreshOut, Rejection> {
    let repo = &input.repo;
    let target = target_commit(repo, &input.target_branch)?;
    match repo
        .merge_tree(&adapter_git(&target), &adapter_git(&input.candidate_commit))
        .map_err(git_rejection)?
    {
        MergeOutcome::Conflict { .. } => Ok(RefreshOut::Conflict { target }),
        MergeOutcome::Clean { tree } => {
            let files = repo
                .diff_numstat(&adapter_git(&target), &tree)
                .map_err(git_rejection)?;
            if files.is_empty() {
                return Err(conflict(
                    "the moved target already contains this change set; complete the objective instead",
                ));
            }
            let digest = input
                .artifacts
                .put(&manifest(&target, &core_git(&tree), &files))
                .map_err(|_| durability("manifest artifact could not be stored"))?;
            repo.retain(&input.retain_ref, &tree)
                .map_err(git_rejection)?;
            Ok(RefreshOut::Rebuilt {
                tree: core_git(&tree),
                target,
                counts: counts(&files),
                manifest_digest: digest.to_hex(),
            })
        }
    }
}
