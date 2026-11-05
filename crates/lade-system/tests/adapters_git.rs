#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code fails loudly on broken fixtures"
)]

mod adapters_support;

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use adapters_support::{TempDir, commit_all, git, git_os, init_repo, status};
use lade_core::domain::ids::{GitAlgorithm, GitId};
use lade_system::adapters::git::{
    ChangeStatus, CommitSpec, GitError, GrepQuery, LineKind, MAX_DIFF_LINES, MergeOutcome,
    RepoPath, Repository, SquashRequest, Worktree, empty_tree, escape_branch_name, parse_id,
    unescape_branch_name, validate_branch_name, validate_refname,
};

fn path(bytes: &[u8]) -> RepoPath {
    RepoPath::new(bytes.to_vec()).expect("valid path")
}

fn id(repo: &Repository, hex: &str) -> GitId {
    parse_id(repo.object_format(), hex).expect("valid id")
}

fn head(repo: &Repository, dir: &Path) -> GitId {
    id(repo, &git(dir, &["rev-parse", "HEAD"]))
}

fn objective(repo: &Repository, root: &Path, name: &str) -> Worktree {
    let start = repo.resolve_commit("refs/heads/main").expect("main");
    repo.worktree_add(&format!("lade/{name}"), &start, &root.join(name))
        .expect("worktree add")
}

#[test]
fn open_validates_top_level_non_bare_and_format() {
    let tmp = TempDir::new("open");
    let repo_dir = init_repo(tmp.path(), "sha1");
    let repo = Repository::open(&repo_dir).expect("open");
    assert_eq!(repo.object_format(), GitAlgorithm::Sha1);
    assert_eq!(repo.work_tree(), repo_dir.as_path());
    assert_eq!(repo.head_branch().unwrap().as_deref(), Some("main"));
    let branches = repo.list_branches().unwrap();
    assert_eq!(branches.len(), 1);
    assert_eq!(branches[0].name, "main");

    fs::create_dir(repo_dir.join("sub")).unwrap();
    assert!(matches!(
        Repository::open(&repo_dir.join("sub")),
        Err(GitError::NotTopLevel)
    ));
    assert!(matches!(
        Repository::open(&repo_dir.join(".git")),
        Err(GitError::NotTopLevel | GitError::BareRepository)
    ));
    let plain = tmp.path().join("plain");
    fs::create_dir(&plain).unwrap();
    assert!(matches!(
        Repository::open(&plain),
        Err(GitError::NotRepository)
    ));
    let bare = tmp.path().join("bare.git");
    git(
        tmp.path(),
        &["init", "-q", "--bare", bare.to_str().unwrap()],
    );
    assert!(matches!(
        Repository::open(&bare),
        Err(GitError::BareRepository)
    ));

    let sha256 = TempDir::new("open256");
    let repo256 = Repository::open(&init_repo(sha256.path(), "sha256")).expect("open sha256");
    assert_eq!(repo256.object_format(), GitAlgorithm::Sha256);
    let main = repo256.resolve_commit("refs/heads/main").unwrap();
    assert_eq!(main.hex().len(), 64);
    assert_eq!(repo256.resolve_ref("refs/heads/missing").unwrap(), None);
    assert!(matches!(
        repo256.resolve_ref("refs/heads/../x"),
        Err(GitError::InvalidRefName)
    ));
}

fn capture_case(format: &str) {
    let tmp = TempDir::new("capture");
    let repo_dir = init_repo(tmp.path(), format);
    fs::write(repo_dir.join("modify.txt"), "one\n").unwrap();
    fs::write(repo_dir.join("delete.txt"), "gone\n").unwrap();
    fs::write(
        repo_dir.join("rename-src.txt"),
        "a fairly long line of content that survives a rename\nsecond\nthird\n",
    )
    .unwrap();
    fs::write(repo_dir.join("tool.sh"), "#!/bin/sh\n").unwrap();
    commit_all(&repo_dir, "base");
    let repo = Repository::open(&repo_dir).unwrap();
    let wt = objective(&repo, tmp.path(), "cap");
    let w = wt.path();

    git(w, &["config", "core.fileMode", "false"]);
    fs::write(w.join("staged.txt"), "staged\n").unwrap();
    git(w, &["add", "staged.txt"]);
    fs::write(w.join("modify.txt"), "one\ntwo\n").unwrap();
    fs::remove_file(w.join("delete.txt")).unwrap();
    fs::rename(w.join("rename-src.txt"), w.join("rename-dst.txt")).unwrap();
    fs::write(w.join("untracked.txt"), "new\n").unwrap();
    fs::write(w.join("blob.bin"), [0u8, 1, 2, 255, 0]).unwrap();
    let odd = OsStr::from_bytes(b"caf\xe9-\xff.txt");
    fs::write(w.join(odd), "latin1 name\n").unwrap();
    symlink("modify.txt", w.join("link")).unwrap();
    fs::set_permissions(w.join("tool.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(w.join(".gitignore"), "ignored.log\n").unwrap();
    fs::write(w.join("ignored.log"), "noise\n").unwrap();

    let index_path = wt.admin_dir().join("index");
    let index_before = fs::read(&index_path).unwrap();
    let status_before = status(w);
    let scratch = tmp.path().join("scratch");
    fs::create_dir(&scratch).unwrap();
    let tree = repo
        .capture_working_tree(&wt, &scratch)
        .expect("capture")
        .tree;
    assert_eq!(status(w), status_before, "status changed by capture");
    assert_eq!(
        fs::read(&index_path).unwrap(),
        index_before,
        "real index touched"
    );
    assert_eq!(
        fs::read_dir(&scratch).unwrap().count(),
        0,
        "temp index left"
    );
    assert_eq!(tree.algorithm(), repo.object_format());

    let base = repo.resolve_commit("refs/heads/main").unwrap();
    let changes = repo.diff_numstat(&base, &tree).unwrap();
    let find = |p: &[u8]| {
        changes
            .iter()
            .find(|c| c.path.as_bytes() == p)
            .unwrap_or_else(|| panic!("missing change {:?}", String::from_utf8_lossy(p)))
    };
    assert_eq!(find(b"modify.txt").status, ChangeStatus::Modified);
    assert_eq!(
        (find(b"modify.txt").added, find(b"modify.txt").removed),
        (1, 0)
    );
    assert_eq!(find(b"delete.txt").status, ChangeStatus::Deleted);
    let renamed = find(b"rename-dst.txt");
    assert!(matches!(renamed.status, ChangeStatus::Renamed { .. }));
    assert_eq!(
        renamed.old_path.as_ref().map(RepoPath::as_bytes),
        Some(&b"rename-src.txt"[..])
    );
    assert_eq!(find(b"untracked.txt").status, ChangeStatus::Added);
    assert_eq!(find(b"staged.txt").status, ChangeStatus::Added);
    assert!(find(b"blob.bin").binary);
    assert_eq!(find(b"caf\xe9-\xff.txt").status, ChangeStatus::Added);
    assert_eq!(find(b"link").new_mode, 0o120000);
    let tool = find(b"tool.sh");
    assert_eq!((tool.old_mode, tool.new_mode), (0o100644, 0o100755));
    assert!(changes.iter().all(|c| c.path.as_bytes() != b"ignored.log"));

    let link = repo.read_blob(&tree, &path(b"link"), 0, 1024).unwrap();
    assert_eq!(link.data, b"modify.txt");
    assert_eq!(link.mode, 0o120000);
    let odd_blob = repo
        .read_blob(&tree, &path(b"caf\xe9-\xff.txt"), 0, 1024)
        .unwrap();
    assert_eq!(odd_blob.data, b"latin1 name\n");
}

#[test]
fn capture_records_every_kind_of_change_without_touching_the_index_sha1() {
    capture_case("sha1");
}

#[test]
fn capture_records_every_kind_of_change_without_touching_the_index_sha256() {
    capture_case("sha256");
}

#[test]
fn commit_tree_and_compare_and_swap_refs() {
    let tmp = TempDir::new("cas");
    let repo_dir = init_repo(tmp.path(), "sha1");
    let repo = Repository::open(&repo_dir).unwrap();
    let main = head(&repo, &repo_dir);
    let tree = id(&repo, &git(&repo_dir, &["rev-parse", "HEAD^{tree}"]));
    let commit = repo
        .commit_tree(&CommitSpec {
            tree: tree.clone(),
            parents: vec![main.clone()],
            message: "squash".into(),
            timestamp: Some(1_700_000_000),
        })
        .unwrap();
    let body = git(&repo_dir, &["cat-file", "commit", commit.hex()]);
    assert!(body.contains("author LADE <lade@localhost> 1700000000 +0000"));
    assert!(body.contains("committer LADE <lade@localhost> 1700000000 +0000"));
    assert!(body.contains(&format!("parent {main}")));

    let refname = "refs/lade/test/candidate";
    repo.update_ref_cas(refname, &commit, None).unwrap();
    assert_eq!(repo.resolve_ref(refname).unwrap(), Some(commit.clone()));
    match repo.update_ref_cas(refname, &main, None) {
        Err(GitError::StaleRef { current, .. }) => assert_eq!(current, Some(commit.clone())),
        other => panic!("expected stale ref, got {other:?}"),
    }
    match repo.update_ref_cas(refname, &commit, Some(&main)) {
        Err(GitError::StaleRef { current, .. }) => assert_eq!(current, Some(commit.clone())),
        other => panic!("expected stale ref, got {other:?}"),
    }
    repo.update_ref_cas(refname, &main, Some(&commit)).unwrap();
    assert!(matches!(
        repo.delete_ref_cas(refname, &commit),
        Err(GitError::StaleRef { .. })
    ));
    repo.delete_ref_cas(refname, &main).unwrap();
    assert_eq!(repo.resolve_ref(refname).unwrap(), None);
    let other_format = empty_tree(GitAlgorithm::Sha256);
    assert!(matches!(
        repo.update_ref_cas(refname, &other_format, None),
        Err(GitError::InvalidId)
    ));
}

#[test]
fn retention_ref_survives_gc_prune() {
    let tmp = TempDir::new("retain");
    let repo = Repository::open(&init_repo(tmp.path(), "sha1")).unwrap();
    let wt = objective(&repo, tmp.path(), "keep");
    let scratch = tmp.path().join("scratch");
    fs::create_dir(&scratch).unwrap();
    fs::write(wt.path().join("kept.txt"), "retained unique content 1\n").unwrap();
    let kept = repo.capture_working_tree(&wt, &scratch).unwrap().tree;
    fs::write(wt.path().join("kept.txt"), "unretained unique content 2\n").unwrap();
    let dropped = repo.capture_working_tree(&wt, &scratch).unwrap().tree;
    let refname = repo.retain("capture-1", &kept).unwrap();
    assert_eq!(refname, "refs/lade/retain/capture-1");
    assert_eq!(repo.retain("capture-1", &kept).unwrap(), refname);
    assert!(matches!(
        repo.retain("capture-1", &dropped),
        Err(GitError::StaleRef { .. })
    ));
    assert!(matches!(
        Repository::retention_ref("../x"),
        Err(GitError::InvalidRefName)
    ));
    let main_dir = repo.work_tree();
    git(main_dir, &["gc", "-q", "--prune=now"]);
    let exists = |object: &GitId| {
        adapters_support::git_cmd(main_dir)
            .args(["cat-file", "-e", object.hex()])
            .status()
            .unwrap()
            .success()
    };
    assert!(exists(&kept), "retained tree pruned");
    assert!(
        repo.read_blob(&kept, &path(b"kept.txt"), 0, 100)
            .unwrap()
            .data
            .starts_with(b"retained")
    );
    assert!(
        !exists(&dropped),
        "unretained tree survived; test is not meaningful"
    );
    repo.release_retention("capture-1", &kept).unwrap();
}

#[test]
fn numstat_hunks_and_byte_offsets() {
    let tmp = TempDir::new("diff");
    let repo_dir = init_repo(tmp.path(), "sha1");
    fs::write(repo_dir.join("crlf.txt"), "alpha\r\nbeta\r\ngamma\r\n").unwrap();
    fs::write(repo_dir.join("utf8.txt"), "añb\nçdé\n日本語\nend").unwrap();
    fs::write(repo_dir.join("gone.txt"), "x\ny\n").unwrap();
    let base_hex = commit_all(&repo_dir, "base");
    fs::write(
        repo_dir.join("crlf.txt"),
        "alpha\r\nBETA\r\ngamma\r\ndelta\r\n",
    )
    .unwrap();
    fs::write(repo_dir.join("utf8.txt"), "añb\n変更\nçdé\n日本語\nend!").unwrap();
    fs::remove_file(repo_dir.join("gone.txt")).unwrap();
    fs::write(repo_dir.join("new.txt"), "n1\nn2").unwrap();
    let huge: String = (0..MAX_DIFF_LINES + 500)
        .map(|i| format!("line {i}\n"))
        .collect();
    fs::write(repo_dir.join("huge.txt"), &huge).unwrap();
    let new_hex = commit_all(&repo_dir, "change");
    let repo = Repository::open(&repo_dir).unwrap();
    let (base, new) = (id(&repo, &base_hex), id(&repo, &new_hex));

    let changes = repo.diff_numstat(&base, &new).unwrap();
    let counts = |p: &[u8]| {
        let c = changes.iter().find(|c| c.path.as_bytes() == p).unwrap();
        (c.status, c.added, c.removed)
    };
    assert_eq!(counts(b"crlf.txt"), (ChangeStatus::Modified, 2, 1));
    assert_eq!(counts(b"utf8.txt"), (ChangeStatus::Modified, 2, 1));
    assert_eq!(counts(b"gone.txt"), (ChangeStatus::Deleted, 0, 2));
    assert_eq!(counts(b"new.txt"), (ChangeStatus::Added, 2, 0));

    let check_offsets = |file: &str| {
        let diff = repo
            .diff_hunks(&base, &new, &path(file.as_bytes()), None)
            .unwrap();
        let new_blob = fs::read(repo_dir.join(file)).unwrap();
        let old_blob = diff
            .old_blob
            .as_ref()
            .map(|b| git_os(&repo_dir, &["cat-file", "blob", b.hex()]))
            .unwrap_or_default();
        for line in diff.hunks.iter().flat_map(|h| &h.lines) {
            if let Some(start) = line.new_byte_start {
                assert!(
                    new_blob[start as usize..].starts_with(&line.text),
                    "{file} new"
                );
                assert!(start == 0 || new_blob[start as usize - 1] == b'\n');
            }
            if let Some(start) = line.old_byte_start {
                assert!(
                    old_blob[start as usize..].starts_with(&line.text),
                    "{file} old"
                );
            }
        }
        diff
    };
    let crlf = check_offsets("crlf.txt");
    assert!(!crlf.binary && !crlf.truncated);
    let lines = &crlf.hunks[0].lines;
    let beta = lines.iter().find(|l| l.kind == LineKind::Add).unwrap();
    assert_eq!(beta.text, b"BETA\r");
    assert_eq!((beta.new_no, beta.new_byte_start), (Some(2), Some(7)));
    let delta = lines.iter().rfind(|l| l.kind == LineKind::Add).unwrap();
    assert_eq!((delta.new_no, delta.new_byte_start), (Some(4), Some(20)));

    let utf8 = check_offsets("utf8.txt");
    let all: Vec<_> = utf8.hunks.iter().flat_map(|h| &h.lines).collect();
    let added = all.iter().find(|l| l.text == "変更".as_bytes()).unwrap();
    assert_eq!((added.new_no, added.new_byte_start), (Some(2), Some(5)));
    let end = all.iter().find(|l| l.text == b"end!").unwrap();
    assert_eq!(
        end.new_byte_start,
        Some("añb\n変更\nçdé\n日本語\n".len() as u64)
    );
    assert!(end.no_newline);
    let old_end = all.iter().find(|l| l.text == b"end").unwrap();
    assert_eq!(
        old_end.old_byte_start,
        Some("añb\nçdé\n日本語\n".len() as u64)
    );
    assert!(old_end.no_newline);

    let added_file = check_offsets("new.txt");
    assert_eq!(added_file.old_blob, None);
    assert_eq!(added_file.hunks[0].lines[1].new_byte_start, Some(3));
    assert!(added_file.hunks[0].lines[1].no_newline);
    let deleted = repo
        .diff_hunks(&base, &new, &path(b"gone.txt"), None)
        .unwrap();
    assert!(
        deleted.hunks[0]
            .lines
            .iter()
            .all(|l| l.kind == LineKind::Del)
    );
    assert_eq!(deleted.hunks[0].lines[1].old_byte_start, Some(2));

    let huge_diff = repo
        .diff_hunks(&base, &new, &path(b"huge.txt"), None)
        .unwrap();
    assert!(huge_diff.truncated);
    assert_eq!(
        huge_diff.hunks.iter().map(|h| h.lines.len()).sum::<usize>(),
        MAX_DIFF_LINES
    );
    assert!(matches!(
        repo.diff_hunks(&base, &new, &path(b"absent"), None),
        Err(GitError::NotFound { .. })
    ));
}

#[test]
fn ref_validation_rejects_revision_syntax_and_invalid_git_names() {
    for name in [
        "refs/heads/main~1",
        "refs/heads/main^{}",
        "refs/heads/main:path",
        "refs/heads/main@{0}",
        "refs/heads/a..b",
        "refs/heads/a b",
        "refs/heads/a\nb",
        "refs/heads/a\0b",
        "refs/heads/a\u{7f}b",
        "refs/heads/a?b",
        "refs/heads/a*b",
        "refs/heads/a[b",
        "refs/heads/a\\b",
        "refs/heads/.hidden",
        "refs/heads/a.lock",
        "refs/heads/a.lock/b",
        "refs/heads/a/",
        "refs/heads/a.",
        "refs//heads/a",
        "--help",
    ] {
        assert!(
            matches!(validate_refname(name), Err(GitError::InvalidRefName)),
            "{name:?}"
        );
    }
    for name in ["-option", "HEAD", "main~1", "main@{0}"] {
        assert!(
            matches!(validate_branch_name(name), Err(GitError::InvalidRefName)),
            "{name:?}"
        );
    }
}

#[test]
fn malicious_repository_configuration_never_executes() {
    let tmp = TempDir::new("malicious");
    let repo_dir = init_repo(tmp.path(), "sha1");
    fs::write(
        repo_dir.join(".gitattributes"),
        "*.txt filter=evil merge=evil diff=evil\n",
    )
    .unwrap();
    fs::write(repo_dir.join("shared.txt"), "1\n2\n3\n").unwrap();
    commit_all(&repo_dir, "attributes");
    git(&repo_dir, &["checkout", "-q", "-b", "side"]);
    fs::write(repo_dir.join("shared.txt"), "1\nside\n3\n").unwrap();
    let side = commit_all(&repo_dir, "side");
    git(&repo_dir, &["checkout", "-q", "main"]);

    let markers = tmp.path().join("markers");
    fs::create_dir(&markers).unwrap();
    let marker_cmd = |name: &str| format!("touch {}/{name}; cat", markers.display());
    let hook_dir = tmp.path().join("evil-hooks");
    fs::create_dir(&hook_dir).unwrap();
    let hooks = [
        "pre-commit",
        "post-commit",
        "post-checkout",
        "reference-transaction",
        "post-index-change",
        "post-merge",
        "pre-auto-gc",
    ];
    for dir in [&hook_dir, &repo_dir.join(".git/hooks")] {
        for hook in hooks {
            let script = dir.join(hook);
            fs::write(
                &script,
                format!("#!/bin/sh\ntouch {}/hook-{hook}\n", markers.display()),
            )
            .unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let config = [
        ("core.hooksPath", hook_dir.display().to_string()),
        ("core.fsmonitor", marker_cmd("fsmonitor")),
        ("filter.evil.clean", marker_cmd("clean")),
        ("filter.evil.smudge", marker_cmd("smudge")),
        ("filter.evil.process", marker_cmd("process")),
        ("filter.evil.required", "true".into()),
        ("merge.evil.driver", marker_cmd("merge-driver")),
        ("diff.evil.textconv", marker_cmd("textconv")),
        ("diff.evil.command", marker_cmd("diff-command")),
        ("diff.external", marker_cmd("diff-external")),
        ("commit.gpgSign", "true".into()),
        ("gpg.program", marker_cmd("gpg")),
        ("core.pager", marker_cmd("pager")),
        ("core.editor", marker_cmd("editor")),
        ("gc.auto", "1".into()),
    ];
    for (key, value) in &config {
        git(&repo_dir, &["config", key, value]);
    }

    let repo = Repository::open(&repo_dir).unwrap();
    let main = repo.resolve_commit("refs/heads/main").unwrap();
    let wt = repo
        .worktree_add("lade/evil", &main, &tmp.path().join("evil-wt"))
        .unwrap();
    assert_eq!(
        fs::read(wt.path().join("shared.txt")).unwrap(),
        b"1\n2\n3\n"
    );
    fs::write(wt.path().join("shared.txt"), "1\n2\n3\n4\n").unwrap();
    fs::write(wt.path().join("added.txt"), "added\n").unwrap();
    let scratch = tmp.path().join("scratch");
    fs::create_dir(&scratch).unwrap();
    let tree = repo.capture_working_tree(&wt, &scratch).unwrap().tree;
    let changes = repo.diff_numstat(&main, &tree).unwrap();
    assert_eq!(changes.len(), 2);
    repo.diff_hunks(&main, &tree, &path(b"shared.txt"), None)
        .unwrap();
    repo.grep(
        &tree,
        &GrepQuery {
            text: "added".into(),
            ignore_case: false,
            max_results: 5,
        },
    )
    .unwrap();
    let candidate = repo
        .commit_tree(&CommitSpec {
            tree: tree.clone(),
            parents: vec![main.clone()],
            message: "candidate".into(),
            timestamp: None,
        })
        .unwrap();
    repo.update_ref_cas("refs/lade/candidates/evil", &candidate, None)
        .unwrap();
    assert!(matches!(
        repo.merge_tree(&main, &id(&repo, &side)).unwrap(),
        MergeOutcome::Clean { .. }
    ));
    git(
        &repo_dir,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "branch",
            "conflicting",
            &side,
        ],
    );
    let conflicting = repo.resolve_commit("refs/heads/conflicting").unwrap();
    assert!(matches!(
        repo.merge_tree(&candidate, &conflicting).unwrap(),
        MergeOutcome::Conflict { .. }
    ));
    let state = repo.target_checkout_state("main").unwrap();
    assert_eq!(state.checkouts.len(), 1);
    assert!(!state.checkouts[0].dirty);
    let outcome = repo
        .integrate_squash(&SquashRequest {
            target_branch: "main".into(),
            expected_target: main,
            candidate_tree: tree,
            message: "integrate".into(),
            timestamp: None,
        })
        .unwrap();
    assert!(outcome.updated_worktree.is_some());
    assert_eq!(
        fs::read(repo_dir.join("shared.txt")).unwrap(),
        b"1\n2\n3\n4\n"
    );
    assert_eq!(fs::read(repo_dir.join("added.txt")).unwrap(), b"added\n");
    repo.worktree_remove(&wt, true).unwrap();

    let fired: Vec<_> = fs::read_dir(&markers)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(
        fired.is_empty(),
        "untrusted configuration executed: {fired:?}"
    );
}

fn config_hooks_supported(tmp: &Path) -> bool {
    let probe = tmp.join("hook-probe");
    fs::create_dir(&probe).unwrap();
    git(&probe, &["init", "-q"]);
    let marker = tmp.join("hook-probe-ran");
    git(
        &probe,
        &[
            "config",
            "hook.probe.command",
            &format!("touch {}", marker.display()),
        ],
    );
    git(&probe, &["config", "hook.probe.event", "pre-commit"]);
    let _ = adapters_support::git_cmd(&probe)
        .args(["hook", "run", "pre-commit"])
        .output();
    marker.exists()
}

#[test]
fn configuration_hooks_never_run() {
    let tmp = TempDir::new("config-hooks");
    if !config_hooks_supported(tmp.path()) {
        eprintln!(
            "skipped real run: host Git has no configuration-defined hooks (hook.<name>.command); \
             the emitted overrides are covered by the command unit tests"
        );
        return;
    }
    let repo_dir = init_repo(tmp.path(), "sha1");
    let markers = tmp.path().join("markers");
    fs::create_dir(&markers).unwrap();
    for event in [
        "reference-transaction",
        "post-index-change",
        "post-checkout",
        "pre-commit",
        "post-commit",
        "post-merge",
    ] {
        let name = format!("evil-{event}");
        git(
            &repo_dir,
            &[
                "config",
                &format!("hook.{name}.command"),
                &format!("touch {}/{event}", markers.display()),
            ],
        );
        git(&repo_dir, &["config", &format!("hook.{name}.event"), event]);
    }
    let repo = Repository::open(&repo_dir).unwrap();
    let main = repo.resolve_commit("refs/heads/main").unwrap();
    let wt = repo
        .worktree_add("lade/hooks", &main, &tmp.path().join("hooks-wt"))
        .unwrap();
    fs::write(wt.path().join("added.txt"), "added\n").unwrap();
    let scratch = tmp.path().join("scratch");
    fs::create_dir(&scratch).unwrap();
    let tree = repo.capture_working_tree(&wt, &scratch).unwrap().tree;
    repo.retain("capture-hooks", &tree).unwrap();
    repo.integrate_squash(&SquashRequest {
        target_branch: "main".into(),
        expected_target: main,
        candidate_tree: tree,
        message: "integrate".into(),
        timestamp: None,
    })
    .unwrap();
    repo.worktree_remove(&wt, true).unwrap();
    let fired: Vec<_> = fs::read_dir(&markers)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(fired.is_empty(), "configuration hooks executed: {fired:?}");
}

#[test]
fn non_utf8_branch_names_are_listed_losslessly() {
    let tmp = TempDir::new("branches");
    let repo_dir = init_repo(tmp.path(), "sha1");
    let raw = b"feat-caf\xe9-\xff";
    git_os(
        &repo_dir,
        &[
            OsStr::new("branch"),
            OsStr::from_bytes(raw),
            OsStr::new("main"),
        ],
    );
    git(&repo_dir, &["branch", "plain"]);
    let repo = Repository::open(&repo_dir).unwrap();
    let branches = repo.list_branches().unwrap();
    let names: Vec<&str> = branches.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["feat-caf\\xe9-\\xff", "main", "plain"]);
    let odd = &branches[0];
    assert_eq!(odd.raw, raw);
    assert!(!odd.is_utf8());
    assert_eq!(unescape_branch_name(&odd.name).as_deref(), Some(&raw[..]));
    assert_eq!(escape_branch_name(b"plain"), "plain");
    assert!(branches[1].is_utf8());
    assert_eq!(odd.commit, repo.resolve_commit("refs/heads/main").unwrap());
    assert_eq!(unescape_branch_name("bad\\x4"), None);
}

#[test]
fn capture_skips_nested_repositories_without_a_commit() {
    let tmp = TempDir::new("nested");
    let repo_dir = init_repo(tmp.path(), "sha1");
    let repo = Repository::open(&repo_dir).unwrap();
    let wt = objective(&repo, tmp.path(), "nest");
    let w = wt.path();
    for dir in ["empty-a", "deep/empty b"] {
        fs::create_dir_all(w.join(dir)).unwrap();
        git(&w.join(dir), &["init", "-q"]);
        fs::write(w.join(dir).join("inside.txt"), "not captured\n").unwrap();
    }
    fs::create_dir(w.join("full")).unwrap();
    git(&w.join("full"), &["init", "-q", "-b", "main"]);
    git(&w.join("full"), &["config", "user.name", "Test"]);
    git(
        &w.join("full"),
        &["config", "user.email", "t@example.invalid"],
    );
    fs::write(w.join("full/x"), "x\n").unwrap();
    commit_all(&w.join("full"), "nested");
    fs::write(w.join("regular.txt"), "kept\n").unwrap();
    let scratch = tmp.path().join("scratch");
    fs::create_dir(&scratch).unwrap();
    let capture = repo.capture_working_tree(&wt, &scratch).unwrap();
    let mut skipped = capture.skipped_nested.clone();
    skipped.sort();
    assert_eq!(skipped, vec![path(b"deep/empty b"), path(b"empty-a")]);
    let base = repo.resolve_commit("refs/heads/main").unwrap();
    let changes = repo.diff_numstat(&base, &capture.tree).unwrap();
    let paths: Vec<&[u8]> = changes.iter().map(|c| c.path.as_bytes()).collect();
    assert_eq!(paths, [&b"full"[..], b"regular.txt"]);
    assert_eq!(
        changes[0].new_mode, 0o160000,
        "committed nested repo is a gitlink"
    );
    assert_eq!(fs::read_dir(&scratch).unwrap().count(), 0);
}

#[test]
fn capture_applies_only_the_configured_user_excludes_file() {
    let tmp = TempDir::new("excludes");
    let repo_dir = init_repo(tmp.path(), "sha1");
    let repo_excludes = tmp.path().join("repo-excludes");
    fs::write(&repo_excludes, "repo-hidden.txt\n").unwrap();
    git(
        &repo_dir,
        &[
            "config",
            "core.excludesFile",
            repo_excludes.to_str().unwrap(),
        ],
    );
    let user_excludes = tmp.path().join("user-ignore");
    fs::write(&user_excludes, "*.secret\n").unwrap();
    let plain = Repository::open(&repo_dir).unwrap();
    let wt = objective(&plain, tmp.path(), "exc");
    for name in ["keys.secret", "repo-hidden.txt", "visible.txt"] {
        fs::write(wt.path().join(name), "x\n").unwrap();
    }
    let scratch = tmp.path().join("scratch");
    fs::create_dir(&scratch).unwrap();
    let base = plain.resolve_commit("refs/heads/main").unwrap();
    let names = |repo: &Repository| -> Vec<Vec<u8>> {
        let tree = repo.capture_working_tree(&wt, &scratch).unwrap().tree;
        repo.diff_numstat(&base, &tree)
            .unwrap()
            .into_iter()
            .map(|c| c.path.into_bytes())
            .collect()
    };
    assert_eq!(
        names(&plain),
        [&b"keys.secret"[..], b"repo-hidden.txt", b"visible.txt"]
    );
    let with_user = plain.clone().with_excludes_file(Some(user_excludes));
    assert_eq!(names(&with_user), [&b"repo-hidden.txt"[..], b"visible.txt"]);
}

#[test]
fn filtered_paths_name_files_with_a_filter_driver() {
    let tmp = TempDir::new("filtered");
    let repo_dir = init_repo(tmp.path(), "sha1");
    fs::write(
        repo_dir.join(".gitattributes"),
        "*.bin filter=lfs diff=lfs merge=lfs -text\n*.txt -filter\n",
    )
    .unwrap();
    fs::write(
        repo_dir.join("model.bin"),
        "version https://git-lfs.github.com/spec/v1\n",
    )
    .unwrap();
    fs::write(repo_dir.join("notes.txt"), "plain\n").unwrap();
    let commit = commit_all(&repo_dir, "lfs");
    let repo = Repository::open(&repo_dir).unwrap();
    let tree = id(&repo, &commit);
    let model = path(b"model.bin");
    let notes = path(b"notes.txt");
    let readme = path(b"README");
    assert_eq!(
        repo.filtered_paths(&tree, &[&model, &notes, &readme])
            .unwrap(),
        vec![model.clone()]
    );
    assert!(repo.filtered_paths(&tree, &[]).unwrap().is_empty());
}
