use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use lade_core::domain::ids::{GitAlgorithm, GitId};
use lade_protocol::client::ErrorCode;

use super::pool::Pool;
use super::verify::{resolve_program, tail_text};
use super::*;
use crate::adapters::git::{self, ChangeStatus, ChangedFile, RepoPath, Repository};
use crate::service::adapters_support::{TempDir, commit_all, git as run_git, init_repo};
use crate::service::convert::core_git;

fn repo(root: &TempDir) -> (PathBuf, Repository) {
    let path = init_repo(root.path(), "sha1");
    let repo = open_repository(&path).expect("open");
    (path, repo)
}

fn head(repo: &Repository) -> GitId {
    target_commit(repo, "main").expect("main")
}

fn tree_of(path: &Path, rev: &str) -> GitId {
    GitId::new(
        GitAlgorithm::Sha1,
        run_git(path, &["rev-parse", &format!("{rev}^{{tree}}")]).trim(),
    )
    .expect("tree")
}

fn changed(path: &[u8], added: u64, removed: u64) -> ChangedFile {
    ChangedFile {
        path: RepoPath::new(path.to_vec()).expect("path"),
        old_path: None,
        status: ChangeStatus::Modified,
        old_mode: 0o100_644,
        new_mode: 0o100_755,
        old_id: None,
        new_id: None,
        added,
        removed,
        binary: false,
    }
}

#[test]
fn a_pool_runs_every_task_submitted_before_shutdown() {
    let pool = Pool::new(3).expect("pool");
    let ran = Arc::new(AtomicUsize::new(0));
    for _ in 0..50 {
        let ran = Arc::clone(&ran);
        pool.submit(move || {
            std::thread::sleep(Duration::from_millis(1));
            ran.fetch_add(1, Ordering::SeqCst);
        });
    }
    pool.shutdown();
    assert_eq!(ran.load(Ordering::SeqCst), 50);
}

#[test]
fn verification_runs_have_their_own_pool_and_absent_pools_drop_work() {
    let (tx, rx) = mpsc::channel();
    let mut pools = Pools {
        general: None,
        verify: Some(Pool::new(1).expect("pool")),
        reaper: None,
    };
    let general = tx.clone();
    pools.submit(
        false,
        Box::new(move || general.send("general").expect("send")),
    );
    pools.submit(true, Box::new(move || tx.send("verify").expect("send")));
    pools.shutdown();
    assert_eq!(rx.try_iter().collect::<Vec<_>>(), ["verify"]);
    assert!(pools.verify.is_none(), "shutdown takes the pools");
}

#[test]
fn change_counts_sum_the_files_and_saturate_the_file_count() {
    let files = [changed(b"a", 3, 1), changed(b"b", 4, 0)];
    let total = counts(&files);
    assert_eq!((total.files, total.added, total.removed), (2, 7, 1));
    assert_eq!(counts(&[]).files, 0);
}

#[test]
fn the_manifest_names_the_target_tree_and_every_file_exactly() {
    let target = GitId::new(GitAlgorithm::Sha1, &"a".repeat(40)).expect("target");
    let tree = GitId::new(GitAlgorithm::Sha1, &"b".repeat(40)).expect("tree");
    let mut renamed = changed(b"new \xff", 1, 2);
    renamed.old_path = Some(RepoPath::new(b"old".to_vec()).expect("old"));
    renamed.status = ChangeStatus::Renamed { similarity: 90 };
    renamed.new_id = Some(git::GitId::parse(git::ObjectFormat::Sha1, &"c".repeat(40)).expect("id"));
    let files = [changed(b"plain.rs", 5, 0), renamed];
    let manifest =
        String::from_utf8(super::candidate::manifest(&target, &tree, &files)).expect("ascii");
    let lines: Vec<&str> = manifest.lines().collect();
    assert_eq!(lines[0], "lade-manifest 1");
    assert_eq!(lines[1], format!("target {}", "a".repeat(40)));
    assert_eq!(lines[2], format!("tree {}", "b".repeat(40)));
    assert_eq!(lines.len(), 5);
    let fields: Vec<&str> = lines[4].split('\t').collect();
    assert_eq!(fields[1..3], ["100644", "100755"]);
    assert_eq!(fields[3], "-", "a missing id is a dash");
    assert_eq!(fields[4], "c".repeat(40));
    assert_eq!(fields[5..7], ["1", "2"]);
    assert_ne!(fields[7], "-", "the old path of a rename is listed");
    assert!(
        fields[8].bytes().all(|b| b.is_ascii_graphic()),
        "paths are encoded, never raw bytes"
    );
}

#[test]
fn whole_reads_return_regular_files_and_refuse_directories() {
    let root = TempDir::new("jobs-read");
    let (path, repo) = repo(&root);
    fs::create_dir(path.join("dir")).expect("dir");
    fs::write(path.join("dir/file"), "content\n").expect("file");
    commit_all(&path, "dir");
    let tree = tree_of(&path, "HEAD");
    let read = |name: &str| {
        read_whole(
            &repo,
            &tree,
            &RepoPath::new(name.as_bytes().to_vec()).expect("path"),
        )
    };
    assert_eq!(read("dir/file").expect("read"), Some(b"content\n".to_vec()));
    assert_eq!(read("missing").expect("read"), None);
    assert_eq!(
        read("dir").expect_err("a tree").code,
        ErrorCode::UnsupportedContent
    );
    let bytes = read_file_bytes(
        &repo,
        &tree,
        &RepoPath::new(b"dir/file".to_vec()).expect("path"),
    )
    .expect("read")
    .expect("present");
    assert!(matches!(bytes, FileBytes::Whole(ref b) if b == b"content\n"));
    assert_eq!(
        target_commit(&repo, "no-such-branch")
            .expect_err("unknown")
            .code,
        ErrorCode::NotFound
    );
}

#[test]
fn verification_tails_keep_the_last_characters_of_lossy_output() {
    assert_eq!(tail_text(b"short"), "short");
    assert_eq!(tail_text(b"a\xffb"), "a\u{fffd}b");
    let long = "x".repeat(5000) + "end";
    let tail = tail_text(long.as_bytes());
    assert_eq!(tail.chars().count(), 4096);
    assert!(tail.ends_with("xend"));
}

#[test]
fn verification_programs_resolve_to_absolute_paths_only() {
    assert_eq!(
        resolve_program("/opt/tool"),
        Some(PathBuf::from("/opt/tool"))
    );
    assert_eq!(
        resolve_program("./tool"),
        None,
        "relative paths are refused"
    );
    assert_eq!(resolve_program("bin/tool"), None);
    let sh = resolve_program("sh").expect("sh is installed");
    assert!(
        sh == Path::new("/usr/bin/sh") || sh == Path::new("/bin/sh"),
        "{sh:?}"
    );
    assert_eq!(resolve_program("lade-no-such-program"), None);
}

#[test]
fn disk_usage_counts_nested_files_and_skips_missing_roots() {
    let root = TempDir::new("jobs-usage");
    let nested = root.path().join("a/b");
    fs::create_dir_all(&nested).expect("dirs");
    let empty = disk_usage(&[root.path().to_path_buf()]);
    fs::write(nested.join("big"), vec![1u8; 256 << 10]).expect("file");
    let used = disk_usage(&[root.path().to_path_buf(), root.path().join("missing")]);
    assert!(used >= empty + (256 << 10), "{used} vs {empty}");
    assert_eq!(disk_usage(&[root.path().join("missing")]), 0);
}

fn worktree(path: &Path, branch: &str) -> PathBuf {
    let worktree = path.with_file_name(format!("wt-{branch}"));
    run_git(
        path,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            branch,
            worktree.to_str().expect("utf-8"),
            "main",
        ],
    );
    worktree
}

#[test]
fn a_change_set_is_empty_until_the_worktree_differs_from_the_target() {
    let root = TempDir::new("jobs-empty");
    let (path, repo) = repo(&root);
    let scratch = root.path().join("scratch");
    fs::create_dir(&scratch).expect("scratch");
    let start = head(&repo);
    assert!(change_set_empty(&repo, None, &scratch, "main", &start).expect("no worktree"));
    let wt = worktree(&path, "lade/empty");
    assert!(change_set_empty(&repo, Some(&wt), &scratch, "main", &start).expect("clean"));
    fs::write(wt.join("new.txt"), "new\n").expect("write");
    assert!(!change_set_empty(&repo, Some(&wt), &scratch, "main", &start).expect("changed"));
}

#[test]
fn cleanup_keeps_unsaved_work_unless_forced_then_removes_worktree_and_branch() {
    let root = TempDir::new("jobs-cleanup");
    let (path, repo) = repo(&root);
    let scratch = root.path().join("scratch");
    fs::create_dir(&scratch).expect("scratch");
    let wt = worktree(&path, "lade/cleanup");
    fs::write(wt.join("unsaved.txt"), "work\n").expect("write");
    let kept =
        cleanup(&repo, Some(&wt), Some("lade/cleanup"), &scratch, false).expect_err("unsaved work");
    assert_eq!(kept.code, ErrorCode::Conflict);
    assert!(wt.exists());
    cleanup(&repo, Some(&wt), Some("lade/cleanup"), &scratch, true).expect("forced");
    assert!(!wt.exists());
    assert!(
        repo.resolve_ref("refs/heads/lade/cleanup")
            .expect("resolve")
            .is_none()
    );

    let clean = worktree(&path, "lade/clean");
    cleanup(&repo, Some(&clean), Some("lade/clean"), &scratch, false)
        .expect("a worktree matching its branch needs no confirmation");
    assert!(!clean.exists());
    let stray = root.path().join("not-a-worktree");
    fs::create_dir(&stray).expect("stray");
    cleanup(&repo, Some(&stray), None, &scratch, false).expect("a stray directory is removed");
    assert!(!stray.exists());
}

#[test]
fn deleting_refs_skips_the_ones_already_gone() {
    let root = TempDir::new("jobs-refs");
    let (path, repo) = repo(&root);
    run_git(&path, &["update-ref", "refs/lade/keep/one", "main"]);
    delete_refs(
        &repo,
        &["refs/lade/keep/one".into(), "refs/lade/keep/missing".into()],
    )
    .expect("delete");
    assert!(
        repo.resolve_ref("refs/lade/keep/one")
            .expect("resolve")
            .is_none()
    );
    assert_eq!(
        core_git(
            &repo
                .resolve_ref("refs/heads/main")
                .expect("main")
                .expect("main")
        ),
        head(&repo)
    );
}
