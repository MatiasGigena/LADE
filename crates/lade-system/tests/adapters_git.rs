mod adapters_support;

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use adapters_support::{TempDir, commit_all, git, git_os, init_repo, status};
use lade_system::adapters::git::{
    ChangeStatus, CommitSpec, GitError, GitId, GrepQuery, LineKind, MAX_DIFF_LINES, MergeOutcome,
    ObjectFormat, RepoPath, Repository, STDERR_LIMIT, SquashRequest, TargetBusyReason, Worktree,
    escape_branch_name, unescape_branch_name, validate_branch_name, validate_refname,
};

fn path(bytes: &[u8]) -> RepoPath {
    RepoPath::new(bytes.to_vec()).expect("valid path")
}

fn id(repo: &Repository, hex: &str) -> GitId {
    GitId::parse(repo.object_format(), hex).expect("valid id")
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
    assert_eq!(repo.object_format(), ObjectFormat::Sha1);
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
    assert_eq!(repo256.object_format(), ObjectFormat::Sha256);
    let main = repo256.resolve_commit("refs/heads/main").unwrap();
    assert_eq!(main.hex().len(), 64);
    assert_eq!(repo256.resolve_ref("refs/heads/missing").unwrap(), None);
    assert!(matches!(
        repo256.resolve_ref("refs/heads/../x"),
        Err(GitError::InvalidRefName)
    ));
}

#[test]
fn worktree_add_open_and_remove() {
    let tmp = TempDir::new("worktree");
    let repo = Repository::open(&init_repo(tmp.path(), "sha1")).unwrap();
    let wt = objective(&repo, tmp.path(), "obj-1");
    assert!(wt.path().join("README").is_file());
    assert!(
        wt.admin_dir()
            .starts_with(repo.common_dir().join("worktrees"))
    );
    assert_eq!(repo.open_worktree(wt.path()).unwrap(), wt);
    assert!(repo.resolve_ref("refs/heads/lade/obj-1").unwrap().is_some());
    assert!(matches!(
        repo.worktree_add(
            "lade/obj-1",
            &head(&repo, repo.work_tree()),
            &tmp.path().join("again")
        ),
        Err(GitError::Failed { .. })
    ));

    fs::write(wt.path().join("scratch.txt"), "dirty").unwrap();
    fs::write(wt.path().join(".git"), "gitdir: /nonexistent\n").unwrap();
    assert!(matches!(
        repo.worktree_remove(&wt, false),
        Err(GitError::WorktreeDirty)
    ));
    repo.worktree_remove(&wt, true).expect("forced remove");
    assert!(!wt.path().exists());
    assert!(matches!(
        repo.open_worktree(wt.path()),
        Err(GitError::WorktreeNotFound)
    ));

    let clean = objective(&repo, tmp.path(), "obj-2");
    repo.worktree_remove(&clean, false).expect("clean remove");
    assert!(!clean.path().exists());
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
    assert_eq!(tree.format(), repo.object_format());

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
    let other_format = ObjectFormat::Sha256.empty_tree();
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
fn blob_pages_and_tree_listing() {
    let tmp = TempDir::new("blob");
    let repo_dir = init_repo(tmp.path(), "sha256");
    let content: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
    fs::create_dir(repo_dir.join("dir")).unwrap();
    fs::write(repo_dir.join("dir/data.bin"), &content).unwrap();
    fs::write(repo_dir.join("dir/*"), "literal star\n").unwrap();
    let commit_hex = commit_all(&repo_dir, "data");
    let repo = Repository::open(&repo_dir).unwrap();
    let commit = id(&repo, &commit_hex);
    let mut assembled = Vec::new();
    let mut offset = 0;
    loop {
        let page = repo
            .read_blob(&commit, &path(b"dir/data.bin"), offset, 1000)
            .unwrap();
        assert_eq!(page.total_bytes, 3000);
        assembled.extend_from_slice(&page.data);
        offset += page.data.len() as u64;
        if page.eof {
            break;
        }
    }
    assert_eq!(assembled, content);
    let past = repo
        .read_blob(&commit, &path(b"dir/data.bin"), 5000, 10)
        .unwrap();
    assert!(past.data.is_empty() && past.eof);
    assert!(matches!(
        repo.read_blob(&commit, &path(b"dir"), 0, 10),
        Err(GitError::NotABlob)
    ));
    let star = repo.read_blob(&commit, &path(b"dir/*"), 0, 100).unwrap();
    assert_eq!(star.data, b"literal star\n");

    let listing = repo.ls_tree(&commit, Some(&path(b"dir")), false).unwrap();
    let names: Vec<_> = listing.iter().map(|e| e.path.display()).collect();
    assert_eq!(names, ["dir/*", "dir/data.bin"]);
    assert_eq!(listing[1].size, Some(3000));
    let all = repo.ls_tree(&commit, None, true).unwrap();
    assert_eq!(all.len(), 3);
}

#[test]
fn grep_is_bounded_and_skips_binary() {
    let tmp = TempDir::new("grep");
    let repo_dir = init_repo(tmp.path(), "sha1");
    for i in 0..30 {
        fs::write(
            repo_dir.join(format!("f{i:02}.txt")),
            format!("first\nxx Needle {i}\n"),
        )
        .unwrap();
    }
    fs::write(repo_dir.join("bin.dat"), b"needle\0binary").unwrap();
    let commit_hex = commit_all(&repo_dir, "grep");
    let repo = Repository::open(&repo_dir).unwrap();
    let commit = id(&repo, &commit_hex);
    let query = |text: &str, ignore_case, max_results| GrepQuery {
        text: text.into(),
        ignore_case,
        max_results,
    };
    let bounded = repo.grep(&commit, &query("Needle", false, 10)).unwrap();
    assert_eq!(bounded.matches.len(), 10);
    assert!(bounded.truncated);
    let first = &bounded.matches[0];
    assert_eq!(first.path.as_bytes(), b"f00.txt");
    assert_eq!((first.line, first.column), (2, 4));
    assert_eq!(first.preview, b"xx Needle 0");

    let all = repo.grep(&commit, &query("needle", true, 200)).unwrap();
    assert_eq!(all.matches.len(), 30, "binary file must be skipped");
    assert!(!all.truncated);
    let none = repo.grep(&commit, &query("needle", false, 10)).unwrap();
    assert!(none.matches.is_empty() && !none.truncated);
    let dash = repo.grep(&commit, &query("-e", false, 10)).unwrap();
    assert!(dash.matches.is_empty());
    assert!(matches!(
        repo.grep(&commit, &query("a\nb", false, 10)),
        Err(GitError::InvalidArgument { .. })
    ));
}

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
    if rustix_is_root() {
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

fn rustix_is_root() -> bool {
    String::from_utf8_lossy(
        &std::process::Command::new("id")
            .arg("-u")
            .output()
            .expect("id -u")
            .stdout,
    )
    .trim()
        == "0"
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

#[test]
fn grep_reports_exact_byte_ranges() {
    let tmp = TempDir::new("grep-offsets");
    let repo_dir = init_repo(tmp.path(), "sha1");
    let text = "αβγ\r\nfirst line\r\nprefix ÉCOLE Needle needle\nlast";
    fs::write(repo_dir.join("u.txt"), text).unwrap();
    let big = "x".repeat(17 << 20) + "\nNeedle\n";
    fs::write(repo_dir.join("big.txt"), &big).unwrap();
    let commit = commit_all(&repo_dir, "offsets");
    let repo = Repository::open(&repo_dir).unwrap();
    let tree = id(&repo, &commit);
    let found = repo
        .grep(
            &tree,
            &GrepQuery {
                text: "needle".into(),
                ignore_case: true,
                max_results: 10,
            },
        )
        .unwrap();
    let hit = found
        .matches
        .iter()
        .find(|m| m.path.as_bytes() == b"u.txt")
        .unwrap();
    let start = text.find("Needle").unwrap();
    assert_eq!(hit.line, 3);
    assert_eq!(hit.byte_start, Some(start as u64));
    assert_eq!(hit.byte_end, Some((start + 6) as u64));
    assert_eq!(&text.as_bytes()[start..start + 6], b"Needle");
    let large = found
        .matches
        .iter()
        .find(|m| m.path.as_bytes() == b"big.txt")
        .unwrap();
    assert_eq!(
        (large.line, large.byte_start, large.byte_end),
        (2, None, None)
    );

    let accented = repo
        .grep(
            &tree,
            &GrepQuery {
                text: "ÉCOLE".into(),
                ignore_case: true,
                max_results: 10,
            },
        )
        .unwrap();
    let start = text.find("ÉCOLE").unwrap();
    assert_eq!(accented.matches.len(), 1);
    assert_eq!(accented.matches[0].byte_start, Some(start as u64));
    assert_eq!(
        accented.matches[0].byte_end,
        Some((start + "ÉCOLE".len()) as u64)
    );
}
