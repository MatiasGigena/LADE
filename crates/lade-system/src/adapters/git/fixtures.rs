//! Repository fixtures for the Git adapter's unit tests.

use std::path::Path;

use super::{GitId, RepoPath, Repository, Worktree};
use crate::adapters_support::git;

pub(super) fn path(bytes: &[u8]) -> RepoPath {
    RepoPath::new(bytes.to_vec()).expect("valid path")
}

pub(super) fn id(repo: &Repository, hex: &str) -> GitId {
    GitId::parse(repo.object_format(), hex).expect("valid id")
}

pub(super) fn head(repo: &Repository, dir: &Path) -> GitId {
    id(repo, &git(dir, &["rev-parse", "HEAD"]))
}

/// An Objective worktree `name` on a new `lade/<name>` branch from `main`.
pub(super) fn objective(repo: &Repository, root: &Path, name: &str) -> Worktree {
    let start = repo.resolve_commit("refs/heads/main").expect("main");
    repo.worktree_add(&format!("lade/{name}"), &start, &root.join(name))
        .expect("worktree add")
}
