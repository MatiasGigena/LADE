//! Git adapter: every repository read and protected write the service performs.
//!
//! All processes are started through the single hardened builder in [`command`]: inherited
//! `GIT_*` variables and user/system configuration are dropped, hooks, fsmonitor, pagers, editors,
//! credential helpers, signing programs and network protocols are disabled, and repository-defined
//! filter and merge drivers are neutralized per invocation (see [`Repository::driver_overrides`]).
//! Repository configuration is treated as untrusted input; nothing in it executes with the service's
//! authority. Paths are lossless bytes (`-z` output everywhere).

mod command;
mod diff;
mod error;
mod integrate;
mod tree;
mod worktree;

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use command::GitCommand;

pub use command::{Diagnostics, LADE_IDENTITY_EMAIL, LADE_IDENTITY_NAME, STDERR_LIMIT};
pub use diff::{
    ChangeStatus, ChangedFile, DiffLine, FileDiff, Hunk, LineKind, MAX_DIFF_BLOB_BYTES,
    MAX_DIFF_LINES,
};
pub use error::{GitError, TargetBusyReason};
pub use integrate::{CheckoutInfo, MergeOutcome, SquashOutcome, SquashRequest, TargetCheckout};
pub use tree::{
    BlobPage, GrepMatch, GrepQuery, GrepResults, MAX_BLOB_PAGE, MAX_GREP_RESULTS, ObjectKind,
    TreeEntry,
};
pub use worktree::Worktree;

pub type Result<T> = std::result::Result<T, GitError>;

/// Minimum Git for the adapter (`worktree list -z`, `GIT_CONFIG_COUNT`).
pub const MIN_GIT_VERSION: GitVersion = GitVersion::new(2, 36, 0);
/// Minimum Git for [`Repository::merge_tree`] (`merge-tree --write-tree`).
pub const MIN_MERGE_TREE_VERSION: GitVersion = GitVersion::new(2, 38, 0);
/// Namespace of service-owned retention references.
pub const RETENTION_PREFIX: &str = "refs/lade/retain/";

/// Repository object hash algorithm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObjectFormat {
    Sha1,
    Sha256,
}

impl ObjectFormat {
    pub fn hex_len(self) -> usize {
        match self {
            Self::Sha1 => 40,
            Self::Sha256 => 64,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
        }
    }

    /// The well-known empty tree for this format.
    pub fn empty_tree(self) -> GitId {
        let hex = match self {
            Self::Sha1 => "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
            Self::Sha256 => "6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321",
        };
        GitId {
            format: self,
            hex: hex.to_owned(),
        }
    }

    fn zero(self) -> String {
        "0".repeat(self.hex_len())
    }
}

/// A full object id tagged with its algorithm.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GitId {
    format: ObjectFormat,
    hex: String,
}

impl GitId {
    /// Parse a full lowercase hexadecimal id of the given format.
    pub fn parse(format: ObjectFormat, hex: &str) -> Result<Self> {
        let valid = hex.len() == format.hex_len()
            && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if valid {
            Ok(Self {
                format,
                hex: hex.to_owned(),
            })
        } else {
            Err(GitError::InvalidId)
        }
    }

    pub fn format(&self) -> ObjectFormat {
        self.format
    }

    pub fn hex(&self) -> &str {
        &self.hex
    }

    fn is_zero(&self) -> bool {
        self.hex.bytes().all(|b| b == b'0')
    }
}

impl fmt::Display for GitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex)
    }
}

impl fmt::Debug for GitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.format.name(), self.hex)
    }
}

/// A lossless repository-relative path: raw bytes, `/`-separated, no empty, `.` or `..`
/// components, no leading/trailing `/`, no NUL.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RepoPath(Vec<u8>);

impl RepoPath {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self> {
        let bytes = bytes.into();
        let valid = !bytes.is_empty()
            && !bytes.contains(&0)
            && bytes
                .split(|b| *b == b'/')
                .all(|c| !c.is_empty() && c != b"." && c != b"..");
        if valid {
            Ok(Self(bytes))
        } else {
            Err(GitError::InvalidPath)
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn as_os_str(&self) -> &OsStr {
        OsStr::from_bytes(&self.0)
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    /// Lossy UTF-8 label; never authority.
    pub fn display(&self) -> String {
        String::from_utf8_lossy(&self.0).into_owned()
    }

    fn from_git(op: &'static str, bytes: &[u8]) -> Result<Self> {
        Self::new(bytes.to_vec()).map_err(|_| GitError::Parse { op, what: "path" })
    }
}

impl fmt::Debug for RepoPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", String::from_utf8_lossy(&self.0))
    }
}

/// `git version` triple.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GitVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl GitVersion {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Parse `git version 2.43.0` (vendor suffixes such as `.windows.1` or ` (Apple Git-1)` ignored).
    pub fn parse(output: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(output).ok()?;
        let version = text.trim().strip_prefix("git version ")?;
        let mut parts = version
            .split(|c: char| !c.is_ascii_digit())
            .map(str::parse::<u32>);
        let major = parts.next()?.ok()?;
        let minor = parts.next()?.ok()?;
        let patch = parts.next().and_then(|p| p.ok()).unwrap_or(0);
        Some(Self::new(major, minor, patch))
    }
}

impl fmt::Display for GitVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// An operation in progress in a worktree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InProgress {
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Bisect,
    Sequencer,
}

/// A local branch and the commit it points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    /// Short name (without `refs/heads/`). Branches whose names are not UTF-8 are not listed.
    pub name: String,
    pub commit: GitId,
}

/// Input to [`Repository::commit_tree`]. Author and committer are always the fixed LADE identity.
#[derive(Clone, Debug)]
pub struct CommitSpec {
    pub tree: GitId,
    pub parents: Vec<GitId>,
    pub message: String,
    /// Unix seconds (UTC) for author and committer dates; `None` uses the current time.
    pub timestamp: Option<i64>,
}

/// An opened, validated, non-bare repository at its top level.
#[derive(Clone, Debug)]
pub struct Repository {
    work_tree: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
    format: ObjectFormat,
    version: GitVersion,
}

impl Repository {
    /// Open the repository whose top-level working directory is exactly `path`.
    pub fn open(path: &Path) -> Result<Self> {
        let top = path.canonicalize().map_err(|_| GitError::NotRepository)?;
        let version = GitCommand::new("version")
            .cwd(&top)
            .arg("version")
            .run()
            .ok()
            .and_then(|out| GitVersion::parse(&out))
            .ok_or(GitError::Parse {
                op: "version",
                what: "version",
            })?;
        if version < MIN_GIT_VERSION {
            return Err(GitError::UnsupportedVersion {
                found: version,
                required: MIN_GIT_VERSION,
            });
        }
        let probe = GitCommand::new("rev-parse")
            .cwd(&top)
            .read_only()
            .args(["rev-parse", "--is-bare-repository", "--is-inside-work-tree"])
            .output()?;
        if !probe.status.success() {
            return Err(GitError::NotRepository);
        }
        match lines(&probe.stdout).as_slice() {
            [b"true", ..] => return Err(GitError::BareRepository),
            [b"false", b"true"] => {}
            _ => return Err(GitError::NotTopLevel),
        }
        let out = GitCommand::new("rev-parse")
            .cwd(&top)
            .read_only()
            .args([
                "rev-parse",
                "--show-toplevel",
                "--absolute-git-dir",
                "--git-common-dir",
                "--show-object-format",
            ])
            .run()?;
        let [toplevel, git_dir, common_dir, format] = lines(&out)[..] else {
            return Err(GitError::Parse {
                op: "rev-parse",
                what: "layout",
            });
        };
        let as_path = |bytes: &[u8]| PathBuf::from(OsString::from_vec(bytes.to_vec()));
        if as_path(toplevel).canonicalize().ok().as_deref() != Some(top.as_path()) {
            return Err(GitError::NotTopLevel);
        }
        let format = match format {
            b"sha1" => ObjectFormat::Sha1,
            b"sha256" => ObjectFormat::Sha256,
            _ => return Err(GitError::UnsupportedObjectFormat),
        };
        let canonical = |p: PathBuf| {
            top.join(p).canonicalize().map_err(|_| GitError::Parse {
                op: "rev-parse",
                what: "git dir",
            })
        };
        Ok(Self {
            git_dir: canonical(as_path(git_dir))?,
            common_dir: canonical(as_path(common_dir))?,
            work_tree: top,
            format,
            version,
        })
    }

    pub fn work_tree(&self) -> &Path {
        &self.work_tree
    }

    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// The shared Git directory (objects, refs, `worktrees/`).
    pub fn common_dir(&self) -> &Path {
        &self.common_dir
    }

    pub fn object_format(&self) -> ObjectFormat {
        self.format
    }

    pub fn git_version(&self) -> GitVersion {
        self.version
    }

    pub(crate) fn git(&self, op: &'static str) -> GitCommand {
        GitCommand::new(op)
            .git_dir(&self.git_dir)
            .work_tree(&self.work_tree)
    }

    fn parse_id(&self, op: &'static str, bytes: &[u8]) -> Result<GitId> {
        let text = std::str::from_utf8(bytes.trim_ascii()).map_err(|_| GitError::Parse {
            op,
            what: "object id",
        })?;
        GitId::parse(self.format, text).map_err(|_| GitError::Parse {
            op,
            what: "object id",
        })
    }

    fn check_id(&self, id: &GitId) -> Result<()> {
        if id.format == self.format {
            Ok(())
        } else {
            Err(GitError::InvalidId)
        }
    }

    /// Local branches, sorted by name.
    pub fn list_branches(&self) -> Result<Vec<Branch>> {
        let out = self
            .git("for-each-ref")
            .read_only()
            .args([
                "for-each-ref",
                "--format=%(objectname)%00%(refname)",
                "refs/heads/",
            ])
            .run()?;
        let mut branches = Vec::new();
        for line in lines(&out) {
            let mut fields = line.splitn(2, |b| *b == 0);
            let (Some(id), Some(name)) = (fields.next(), fields.next()) else {
                return Err(GitError::Parse {
                    op: "for-each-ref",
                    what: "branch",
                });
            };
            let Some(short) = name.strip_prefix(b"refs/heads/") else {
                continue;
            };
            if let Ok(name) = std::str::from_utf8(short) {
                branches.push(Branch {
                    name: name.to_owned(),
                    commit: self.parse_id("for-each-ref", id)?,
                });
            }
        }
        branches.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(branches)
    }

    /// The branch checked out in the opened worktree, if `HEAD` is attached.
    pub fn head_branch(&self) -> Result<Option<String>> {
        let out = self
            .git("symbolic-ref")
            .read_only()
            .args(["symbolic-ref", "--quiet", "HEAD"])
            .output()?;
        if !out.status.success() {
            return Ok(None);
        }
        Ok(std::str::from_utf8(out.stdout.trim_ascii())
            .ok()
            .and_then(|r| r.strip_prefix("refs/heads/"))
            .map(str::to_owned))
    }

    /// Resolve a full reference name (`refs/...`) to the object it points at.
    pub fn resolve_ref(&self, refname: &str) -> Result<Option<GitId>> {
        validate_refname(refname)?;
        let out = self
            .git("rev-parse")
            .read_only()
            .args(["rev-parse", "--verify", "--quiet", refname])
            .output()?;
        if out.status.success() {
            self.parse_id("rev-parse", &out.stdout).map(Some)
        } else if out.status.code() == Some(1) && out.stdout.is_empty() {
            Ok(None)
        } else {
            Err(GitError::Failed {
                op: "rev-parse",
                exit_code: out.status.code(),
                stderr: out.stderr,
            })
        }
    }

    /// Resolve a full hex id or a full reference name to a commit id.
    pub fn resolve_commit(&self, rev: &str) -> Result<GitId> {
        if GitId::parse(self.format, rev).is_err() {
            validate_refname(rev)?;
        }
        let out = self
            .git("rev-parse")
            .read_only()
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("{rev}^{{commit}}"))
            .output()?;
        if out.status.success() {
            self.parse_id("rev-parse", &out.stdout)
        } else {
            Err(GitError::NotFound { what: "commit" })
        }
    }

    /// Create a commit object with the fixed LADE identity. Never signs, never runs hooks.
    pub fn commit_tree(&self, spec: &CommitSpec) -> Result<GitId> {
        self.check_id(&spec.tree)?;
        let mut cmd =
            self.git("commit-tree")
                .args(["commit-tree", "--no-gpg-sign", spec.tree.hex()]);
        for parent in &spec.parents {
            self.check_id(parent)?;
            cmd = cmd.arg("-p").arg(parent.hex());
        }
        let mut message = spec.message.clone().into_bytes();
        if !message.ends_with(b"\n") {
            message.push(b'\n');
        }
        cmd = cmd
            .args(["-F", "-"])
            .stdin(message)
            .env("GIT_AUTHOR_NAME", LADE_IDENTITY_NAME)
            .env("GIT_AUTHOR_EMAIL", LADE_IDENTITY_EMAIL)
            .env("GIT_COMMITTER_NAME", LADE_IDENTITY_NAME)
            .env("GIT_COMMITTER_EMAIL", LADE_IDENTITY_EMAIL);
        if let Some(ts) = spec.timestamp {
            let date = format!("@{ts} +0000");
            cmd = cmd
                .env("GIT_AUTHOR_DATE", &date)
                .env("GIT_COMMITTER_DATE", &date);
        }
        let out = cmd.run()?;
        self.parse_id("commit-tree", &out)
    }

    /// Compare-and-swap reference update. `old = None` requires that the ref does not exist.
    /// A rejected expectation is [`GitError::StaleRef`] with the observed current value.
    pub fn update_ref_cas(&self, refname: &str, new: &GitId, old: Option<&GitId>) -> Result<()> {
        validate_refname(refname)?;
        self.check_id(new)?;
        let old_hex = match old {
            Some(id) => {
                self.check_id(id)?;
                id.hex.clone()
            }
            None => self.format.zero(),
        };
        let out = self
            .git("update-ref")
            .args(["update-ref", "--no-deref", "-m", "lade", refname, new.hex()])
            .arg(old_hex)
            .output()?;
        self.cas_result(refname, old, out)
    }

    /// Compare-and-swap reference deletion.
    pub fn delete_ref_cas(&self, refname: &str, old: &GitId) -> Result<()> {
        validate_refname(refname)?;
        self.check_id(old)?;
        let out = self
            .git("update-ref")
            .args(["update-ref", "--no-deref", "-d", refname, old.hex()])
            .output()?;
        self.cas_result(refname, Some(old), out)
    }

    fn cas_result(
        &self,
        refname: &str,
        expected: Option<&GitId>,
        out: command::GitOutput,
    ) -> Result<()> {
        if out.status.success() {
            return Ok(());
        }
        let current = self.resolve_ref(refname)?;
        if current.as_ref() != expected {
            return Err(GitError::StaleRef {
                refname: refname.to_owned(),
                current,
            });
        }
        Err(GitError::Failed {
            op: "update-ref",
            exit_code: out.status.code(),
            stderr: out.stderr,
        })
    }

    /// The retention reference for a service-owned id (`[A-Za-z0-9_-]{1,128}`).
    pub fn retention_ref(id: &str) -> Result<String> {
        let valid = !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if valid {
            Ok(format!("{RETENTION_PREFIX}{id}"))
        } else {
            Err(GitError::InvalidRefName)
        }
    }

    /// Pin `object` (commit, tree or blob) against garbage collection under
    /// `refs/lade/retain/<id>`. Idempotent for the same object; a different existing object is
    /// [`GitError::StaleRef`].
    pub fn retain(&self, id: &str, object: &GitId) -> Result<String> {
        let refname = Self::retention_ref(id)?;
        match self.update_ref_cas(&refname, object, None) {
            Err(GitError::StaleRef { current, .. }) if current.as_ref() == Some(object) => {
                Ok(refname)
            }
            other => other.map(|()| refname),
        }
    }

    /// Drop a retention reference if it still points at `expected`.
    pub fn release_retention(&self, id: &str, expected: &GitId) -> Result<()> {
        self.delete_ref_cas(&Self::retention_ref(id)?, expected)
    }

    /// Capture the complete working tree content (tracked, untracked-not-ignored, deletions,
    /// renames, modes, symlinks) of `worktree` as a tree object. Uses a temporary index inside
    /// `scratch_dir`; the worktree's real index and files are never written.
    pub fn capture_working_tree(&self, worktree: &Worktree, scratch_dir: &Path) -> Result<GitId> {
        let index = scratch_dir.join(format!(
            "lade-capture-{}-{}.index",
            std::process::id(),
            unique_suffix()
        ));
        let mut lock = index.clone().into_os_string();
        lock.push(".lock");
        let _cleanup = RemoveOnDrop(vec![index.clone(), PathBuf::from(lock)]);
        let overrides = Self::driver_overrides(worktree.admin_dir(), worktree.path())?;
        let base = |op| {
            worktree
                .git(op)
                .configs(&overrides)
                .env("GIT_INDEX_FILE", &index)
        };
        let head = worktree
            .git("rev-parse")
            .read_only()
            .args(["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])
            .output()?;
        if head.status.success() {
            let head = self.parse_id("rev-parse", &head.stdout)?;
            base("read-tree").args(["read-tree", head.hex()]).run()?;
        } else {
            base("read-tree").args(["read-tree", "--empty"]).run()?;
        }
        base("add")
            .config("core.fileMode", "true")
            .config("core.symlinks", "true")
            .config("core.ignoreCase", "false")
            .config("core.excludesFile", "/dev/null")
            .config("advice.addEmbeddedRepo", "false")
            .args(["add", "--all", "--no-ignore-errors"])
            .run()?;
        let out = base("write-tree").arg("write-tree").run()?;
        self.parse_id("write-tree", &out)
    }

    /// Command-scope overrides that disable every filter and merge driver defined in the
    /// (untrusted) configuration visible from `git_dir`, including included files and worktree
    /// configuration. Filters become no-ops (content passes through unconverted); custom merge
    /// drivers become `false`, so affected paths report conflicts rather than running code.
    pub(crate) fn driver_overrides(
        git_dir: &Path,
        work_tree: &Path,
    ) -> Result<Vec<(OsString, OsString)>> {
        let out = GitCommand::new("config")
            .git_dir(git_dir)
            .work_tree(work_tree)
            .read_only()
            .args(["config", "--list", "-z", "--name-only"])
            .stdout_limit(4 << 20)
            .run()?;
        let mut filters = BTreeSet::new();
        let mut merges = BTreeSet::new();
        for name in out.split(|b| *b == 0).filter(|n| !n.is_empty()) {
            let split = |rest: &[u8]| {
                rest.iter()
                    .rposition(|b| *b == b'.')
                    .map(|dot| (rest[..dot].to_vec(), rest[dot + 1..].to_ascii_lowercase()))
            };
            if let Some((sub, key)) = name.strip_prefix(b"filter.").and_then(split)
                && matches!(&key[..], b"clean" | b"smudge" | b"process" | b"required")
            {
                filters.insert(sub);
            } else if let Some((sub, key)) = name.strip_prefix(b"merge.").and_then(split)
                && matches!(&key[..], b"driver" | b"recursive")
            {
                merges.insert(sub);
            }
        }
        let key = |section: &[u8], sub: &[u8], name: &[u8]| {
            OsString::from_vec([section, b".", sub, b".", name].concat())
        };
        let mut pairs = Vec::new();
        for sub in &filters {
            for (name, value) in [
                (&b"clean"[..], ""),
                (b"smudge", ""),
                (b"process", ""),
                (b"required", "false"),
            ] {
                pairs.push((key(b"filter", sub, name), OsString::from(value)));
            }
        }
        for sub in &merges {
            pairs.push((key(b"merge", sub, b"driver"), OsString::from("false")));
            pairs.push((key(b"merge", sub, b"recursive"), OsString::from("binary")));
        }
        Ok(pairs)
    }
}

/// Validate a full reference name with a conservative subset of `git check-ref-format`.
pub fn validate_refname(name: &str) -> Result<()> {
    let valid = name.starts_with("refs/")
        && name.len() <= 1024
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'+' | b'=')
        })
        && !name.contains("..")
        && !name.contains("@{")
        && name.split('/').all(|c| {
            !c.is_empty() && !c.starts_with('.') && !c.ends_with('.') && !c.ends_with(".lock")
        });
    if valid {
        Ok(())
    } else {
        Err(GitError::InvalidRefName)
    }
}

/// Validate a short branch name (the part after `refs/heads/`).
pub fn validate_branch_name(name: &str) -> Result<()> {
    if name.starts_with('-') || name == "HEAD" {
        return Err(GitError::InvalidRefName);
    }
    validate_refname(&format!("refs/heads/{name}"))
}

fn lines(bytes: &[u8]) -> Vec<&[u8]> {
    bytes
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .collect()
}

fn unique_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{}-{nanos}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

struct RemoveOnDrop(Vec<PathBuf>);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}
