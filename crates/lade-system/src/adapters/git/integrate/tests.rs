use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use super::super::fixtures::{id, objective, path};
use super::*;
use crate::adapters::git::{GitError, STDERR_LIMIT, TargetBusyReason};
use crate::adapters_support::{TempDir, commit_all, git, init_repo, status};

#[test]
fn merge_tree_clean_and_conflict() {
    let tmp = TempDir::new("merge");
    let repo_dir = init_repo(tmp.path(), "sha1");
    fs::write(repo_dir.join("shared.txt"), "1\n2\n3\n").unwrap();
    let base = commit_all(&repo_dir, "base");
    git(&repo_dir, &["checkout", "-q", "-b", "side"]);
    fs::write(repo_dir.join("side.txt"), "side\n").unwrap();
    fs::write(repo_dir.join("shared.txt"), "1\nside\n3\n").unwrap();
    let side = commit_all(&repo_dir, "side");
    git(&repo_dir, &["checkout", "-q", "main"]);
    fs::write(repo_dir.join("main.txt"), "main\n").unwrap();
    let main_clean = commit_all(&repo_dir, "main clean");
    let repo = Repository::open(&repo_dir).unwrap();

    git(&repo_dir, &["checkout", "-q", "-b", "other", &base]);
    fs::write(repo_dir.join("other.txt"), "other\n").unwrap();
    let other = commit_all(&repo_dir, "other");
    match repo
        .merge_tree(&id(&repo, &main_clean), &id(&repo, &other))
        .unwrap()
    {
        MergeOutcome::Clean { tree } => {
            let names: Vec<_> = repo
                .ls_tree(&tree, None, true)
                .unwrap()
                .into_iter()
                .map(|e| e.path.display())
                .collect();
            assert!(names.contains(&"main.txt".to_owned()));
            assert!(names.contains(&"other.txt".to_owned()));
        }
        other => panic!("expected clean merge, got {other:?}"),
    }

    git(&repo_dir, &["checkout", "-q", "main"]);
    fs::write(repo_dir.join("shared.txt"), "1\nmain\n3\n").unwrap();
    let main_conflict = commit_all(&repo_dir, "main conflict");
    match repo
        .merge_tree(&id(&repo, &main_conflict), &id(&repo, &side))
        .unwrap()
    {
        MergeOutcome::Conflict { paths, .. } => {
            assert_eq!(paths, vec![path(b"shared.txt")]);
        }
        other => panic!("expected conflict, got {other:?}"),
    }
}

struct IntegrationFixture {
    tmp: TempDir,
    repo: Repository,
    target: GitId,
    candidate_tree: GitId,
}

fn integration_fixture(tag: &str) -> IntegrationFixture {
    let tmp = TempDir::new(tag);
    let repo_dir = init_repo(tmp.path(), "sha1");
    let repo = Repository::open(&repo_dir).unwrap();
    let wt = objective(&repo, tmp.path(), "int");
    fs::write(wt.path().join("feature.txt"), "feature\n").unwrap();
    fs::write(wt.path().join("README"), "hello\nchanged\n").unwrap();
    let scratch = tmp.path().join("scratch");
    fs::create_dir(&scratch).unwrap();
    let candidate_tree = repo.capture_working_tree(&wt, &scratch).unwrap().tree;
    let target = repo.resolve_commit("refs/heads/main").unwrap();
    IntegrationFixture {
        tmp,
        repo,
        target,
        candidate_tree,
    }
}

impl IntegrationFixture {
    fn request(&self, branch: &str) -> SquashRequest {
        SquashRequest {
            target_branch: branch.into(),
            expected_target: self.target.clone(),
            candidate_tree: self.candidate_tree.clone(),
            message: "Integrate objective".into(),
            timestamp: Some(1_700_000_100),
        }
    }

    fn main_dir(&self) -> PathBuf {
        self.repo.work_tree().to_path_buf()
    }
}

#[test]
fn integrate_into_branch_not_checked_out() {
    let f = integration_fixture("int-a");
    let dir = f.main_dir();
    git(&dir, &["branch", "release", f.target.hex()]);
    let status_before = status(&dir);
    let outcome = f.repo.integrate_squash(&f.request("release")).unwrap();
    assert_eq!(outcome.updated_worktree, None);
    assert_eq!(
        f.repo.resolve_ref("refs/heads/release").unwrap(),
        Some(outcome.commit.clone())
    );
    assert_eq!(
        git(&dir, &["rev-parse", &format!("{}^", outcome.commit)]),
        f.target.hex()
    );
    assert_eq!(
        git(
            &dir,
            &["rev-parse", &format!("{}^{{tree}}", outcome.commit)]
        ),
        f.candidate_tree.hex()
    );
    assert_eq!(status(&dir), status_before);
    assert_eq!(fs::read(dir.join("README")).unwrap(), b"hello\n");
    let _ = &f.tmp;
}

#[test]
fn integrate_into_valid_utf8_target_branches() {
    let f = integration_fixture("int-utf8");
    let dir = f.main_dir();
    for branch in [
        "feature/mañana",
        "release/v1@candidate",
        "fix/(parser)",
        "fix./parser",
    ] {
        git(&dir, &["branch", branch, f.target.hex()]);
        let listed = f.repo.list_branches().unwrap();
        assert!(listed.iter().any(|entry| entry.name == branch));
        let refname = format!("refs/heads/{branch}");
        assert_eq!(
            f.repo.resolve_ref(&refname).unwrap(),
            Some(f.target.clone())
        );
        assert_eq!(f.repo.resolve_commit(&refname).unwrap(), f.target);

        let outcome = f.repo.integrate_squash(&f.request(branch)).unwrap();
        assert_eq!(f.repo.resolve_ref(&refname).unwrap(), Some(outcome.commit));
        assert_eq!(fs::read(dir.join("README")).unwrap(), b"hello\n");
    }
}

#[test]
fn integrate_into_clean_checked_out_target_updates_files() {
    let f = integration_fixture("int-b");
    let dir = f.main_dir();
    fs::write(dir.join("untracked-note"), "keep me\n").unwrap();
    let outcome = f.repo.integrate_squash(&f.request("main")).unwrap();
    assert_eq!(outcome.updated_worktree.as_deref(), Some(dir.as_path()));
    assert_eq!(git(&dir, &["rev-parse", "HEAD"]), outcome.commit.hex());
    assert_eq!(fs::read(dir.join("feature.txt")).unwrap(), b"feature\n");
    assert_eq!(fs::read(dir.join("README")).unwrap(), b"hello\nchanged\n");
    assert_eq!(
        git(&dir, &["status", "--porcelain", "--untracked-files=no"]),
        ""
    );
    assert_eq!(fs::read(dir.join("untracked-note")).unwrap(), b"keep me\n");
}

#[test]
fn integrate_refuses_dirty_or_busy_target_and_leaves_it_untouched() {
    let f = integration_fixture("int-c");
    let dir = f.main_dir();
    fs::write(dir.join("README"), "local edit\n").unwrap();
    let status_before = status(&dir);
    match f.repo.integrate_squash(&f.request("main")) {
        Err(GitError::TargetDirty { worktree }) => assert_eq!(worktree, dir),
        other => panic!("expected dirty refusal, got {other:?}"),
    }
    assert_eq!(
        f.repo.resolve_ref("refs/heads/main").unwrap(),
        Some(f.target.clone())
    );
    assert_eq!(fs::read(dir.join("README")).unwrap(), b"local edit\n");
    assert_eq!(status(&dir), status_before);

    git(&dir, &["checkout", "-q", "--", "README"]);
    fs::write(dir.join(".git/MERGE_HEAD"), format!("{}\n", f.target)).unwrap();
    match f.repo.integrate_squash(&f.request("main")) {
        Err(GitError::TargetBusy {
            reason: TargetBusyReason::InProgress(states),
            ..
        }) => assert!(!states.is_empty()),
        other => panic!("expected busy refusal, got {other:?}"),
    }
    fs::remove_file(dir.join(".git/MERGE_HEAD")).unwrap();
    fs::write(dir.join(".git/index.lock"), "").unwrap();
    assert!(matches!(
        f.repo.integrate_squash(&f.request("main")),
        Err(GitError::TargetBusy {
            reason: TargetBusyReason::IndexLocked,
            ..
        })
    ));
    fs::remove_file(dir.join(".git/index.lock")).unwrap();
    assert_eq!(
        f.repo.resolve_ref("refs/heads/main").unwrap(),
        Some(f.target.clone())
    );
}

#[test]
fn integrate_refuses_moved_target() {
    let f = integration_fixture("int-d");
    let dir = f.main_dir();
    fs::write(dir.join("other.txt"), "moved\n").unwrap();
    let moved = commit_all(&dir, "target moved");
    match f.repo.integrate_squash(&f.request("main")) {
        Err(GitError::TargetMoved { current }) => {
            assert_eq!(current.map(|c| c.hex().to_owned()), Some(moved.clone()));
        }
        other => panic!("expected moved refusal, got {other:?}"),
    }
    assert_eq!(git(&dir, &["rev-parse", "main"]), moved);
}

#[test]
fn integrate_refuses_untracked_collision_before_moving_the_ref() {
    let f = integration_fixture("int-e");
    let dir = f.main_dir();
    let reflog_before = git(&dir, &["reflog", "show", "--format=%H", "refs/heads/main"]);
    fs::write(dir.join("feature.txt"), "precious local file\n").unwrap();
    match f.repo.integrate_squash(&f.request("main")) {
        Err(GitError::TargetUntracked {
            worktree,
            paths,
            stderr,
        }) => {
            assert_eq!(worktree, dir);
            assert_eq!(paths, vec![path(b"feature.txt")]);
            assert!(stderr.as_bytes().len() <= STDERR_LIMIT);
        }
        other => panic!("expected untracked refusal, got {other:?}"),
    }
    assert_eq!(
        f.repo.resolve_ref("refs/heads/main").unwrap(),
        Some(f.target.clone())
    );
    assert_eq!(
        git(&dir, &["reflog", "show", "--format=%H", "refs/heads/main"]),
        reflog_before
    );
    assert_eq!(
        fs::read(dir.join("feature.txt")).unwrap(),
        b"precious local file\n"
    );
    assert_eq!(fs::read(dir.join("README")).unwrap(), b"hello\n");
    assert!(!dir.join(".git/index.lock").exists());
}

#[test]
fn integrate_reverts_ref_when_checkout_fails_after_the_preflight() {
    if rustix::process::geteuid().is_root() {
        eprintln!(
            "skipped: running as root, so a read-only target directory cannot make the checkout step fail"
        );
        return;
    }
    let f = integration_fixture("int-f");
    let dir = f.main_dir();
    let mode = fs::metadata(&dir).unwrap().permissions().mode();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    let result = f.repo.integrate_squash(&f.request("main"));
    fs::set_permissions(&dir, fs::Permissions::from_mode(mode)).unwrap();
    match result {
        Err(GitError::CheckoutFailed {
            reverted, stderr, ..
        }) => {
            assert!(reverted);
            assert!(stderr.as_bytes().len() <= STDERR_LIMIT);
        }
        other => panic!("expected checkout failure, got {other:?}"),
    }
    assert_eq!(
        f.repo.resolve_ref("refs/heads/main").unwrap(),
        Some(f.target.clone())
    );
    assert!(!dir.join("feature.txt").exists());
}
