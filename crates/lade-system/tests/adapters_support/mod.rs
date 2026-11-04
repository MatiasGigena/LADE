#![allow(dead_code)]

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Reports a test skipped because a sandbox prerequisite (bubblewrap, prlimit, working user
/// namespaces, or the node runtime of the worker tests) is missing. With `LADE_REQUIRE_SANDBOX=1`,
/// as CI sets it, the missing prerequisite fails the test instead of skipping it.
pub fn skip_without_sandbox(why: impl std::fmt::Display) {
    assert!(
        std::env::var_os("LADE_REQUIRE_SANDBOX").is_none_or(|v| v != "1"),
        "LADE_REQUIRE_SANDBOX=1, but the sandbox prerequisites are missing: {why}"
    );
    eprintln!("skipping: sandbox prerequisites missing ({why})");
}

pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "lade-test-{tag}-{}-{}-{nanos}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path.canonicalize().expect("canonical temp dir"))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn git_cmd(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .current_dir(dir);
    cmd
}

pub fn git_os<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Vec<u8> {
    let out = git_cmd(dir).args(args).output().expect("spawn git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args.iter().map(|a| a.as_ref()).collect::<Vec<_>>(),
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    String::from_utf8(git_os(dir, args))
        .expect("utf-8 git output")
        .trim_end()
        .to_owned()
}

pub fn status(dir: &Path) -> Vec<u8> {
    git_os(
        dir,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ],
    )
}

pub fn init_repo(root: &Path, format: &str) -> PathBuf {
    let repo = root.join("repo");
    fs::create_dir(&repo).expect("mkdir repo");
    git(
        &repo,
        &[
            "init",
            "-q",
            "-b",
            "main",
            &format!("--object-format={format}"),
        ],
    );
    git(&repo, &["config", "user.name", "Test"]);
    git(&repo, &["config", "user.email", "test@example.invalid"]);
    fs::write(repo.join("README"), "hello\n").expect("write README");
    commit_all(&repo, "initial");
    repo
}

pub fn commit_all(repo: &Path, message: &str) -> String {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "--no-verify", "-m", message]);
    git(repo, &["rev-parse", "HEAD"])
}
