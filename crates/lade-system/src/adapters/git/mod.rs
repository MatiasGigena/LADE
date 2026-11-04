mod command;
mod diff;
mod error;
mod integrate;
mod tree;
mod worktree;

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

pub const MIN_GIT_VERSION: GitVersion = GitVersion::new(2, 36, 0);
pub const MIN_MERGE_TREE_VERSION: GitVersion = GitVersion::new(2, 38, 0);
pub const MIN_CHECK_ATTR_SOURCE_VERSION: GitVersion = GitVersion::new(2, 40, 0);
pub const RETENTION_PREFIX: &str = "refs/lade/retain/";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObjectFormat {
    Sha1,
    Sha256,
}

impl ObjectFormat {
    #[must_use]
    pub fn hex_len(self) -> usize {
        match self {
            Self::Sha1 => 40,
            Self::Sha256 => 64,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
        }
    }

    #[must_use]
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

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GitId {
    format: ObjectFormat,
    hex: String,
}

impl GitId {
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

    #[must_use]
    pub fn format(&self) -> ObjectFormat {
        self.format
    }

    #[must_use]
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

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    #[must_use]
    pub fn as_os_str(&self) -> &OsStr {
        OsStr::from_bytes(&self.0)
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    #[must_use]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GitVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl GitVersion {
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InProgress {
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Bisect,
    Sequencer,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    pub raw: Vec<u8>,
    pub commit: GitId,
}

impl Branch {
    #[must_use]
    pub fn is_utf8(&self) -> bool {
        std::str::from_utf8(&self.raw).is_ok()
    }
}

#[derive(Clone, Debug)]
pub struct CommitSpec {
    pub tree: GitId,
    pub parents: Vec<GitId>,
    pub message: String,
    pub timestamp: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct Repository {
    work_tree: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
    format: ObjectFormat,
    version: GitVersion,
    excludes_file: Option<PathBuf>,
}

impl Repository {
    pub fn open(path: &Path) -> Result<Self> {
        let top = path.canonicalize().map_err(|_| GitError::NotRepository)?;
        let version = GitCommand::new("version")
            .without_config_scan()
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
            excludes_file: None,
        })
    }

    #[must_use]
    pub fn work_tree(&self) -> &Path {
        &self.work_tree
    }

    #[must_use]
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    #[must_use]
    pub fn common_dir(&self) -> &Path {
        &self.common_dir
    }

    #[must_use]
    pub fn object_format(&self) -> ObjectFormat {
        self.format
    }

    #[must_use]
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
            branches.push(Branch {
                name: escape_branch_name(short),
                raw: short.to_vec(),
                commit: self.parse_id("for-each-ref", id)?,
            });
        }
        branches.sort_by(|a, b| a.raw.cmp(&b.raw));
        Ok(branches)
    }

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

    pub fn retain(&self, id: &str, object: &GitId) -> Result<String> {
        let refname = Self::retention_ref(id)?;
        match self.update_ref_cas(&refname, object, None) {
            Err(GitError::StaleRef { current, .. }) if current.as_ref() == Some(object) => {
                Ok(refname)
            }
            other => other.map(|()| refname),
        }
    }

    pub fn release_retention(&self, id: &str, expected: &GitId) -> Result<()> {
        self.delete_ref_cas(&Self::retention_ref(id)?, expected)
    }

    pub fn capture_working_tree(
        &self,
        worktree: &Worktree,
        scratch_dir: &Path,
    ) -> Result<WorkingCapture> {
        let index = scratch_dir.join(format!(
            "lade-capture-{}-{}.index",
            std::process::id(),
            unique_suffix()
        ));
        let mut lock = index.clone().into_os_string();
        lock.push(".lock");
        let _cleanup = RemoveOnDrop(vec![index.clone(), PathBuf::from(lock)]);
        let base = |op| worktree.git(op).env("GIT_INDEX_FILE", &index);
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
        let excludes = self
            .excludes_file
            .as_deref()
            .map_or_else(|| OsStr::new("/dev/null"), Path::as_os_str);
        let mut skipped_nested: Vec<RepoPath> = Vec::new();
        loop {
            let mut add = base("add")
                .config("core.fileMode", "true")
                .config("core.symlinks", "true")
                .config("core.ignoreCase", "false")
                .config("core.excludesFile", excludes)
                .config("advice.addEmbeddedRepo", "false")
                .args(["add", "--all", "--no-ignore-errors"]);
            if !skipped_nested.is_empty() {
                add = add.env("GIT_LITERAL_PATHSPECS", "0").arg("--");
                for path in &skipped_nested {
                    let mut spec = b":(exclude,literal)".to_vec();
                    spec.extend_from_slice(path.as_bytes());
                    add = add.arg(OsStr::from_bytes(&spec));
                }
            }
            let out = add.output()?;
            if out.status.success() {
                break;
            }
            match nested_without_commit(&out.stderr) {
                Some(path)
                    if skipped_nested.len() < MAX_SKIPPED_NESTED
                        && !skipped_nested.contains(&path) =>
                {
                    skipped_nested.push(path);
                }
                _ => {
                    return Err(GitError::Failed {
                        op: "add",
                        exit_code: out.status.code(),
                        stderr: out.stderr,
                    });
                }
            }
        }
        let out = base("write-tree").arg("write-tree").run()?;
        Ok(WorkingCapture {
            tree: self.parse_id("write-tree", &out)?,
            skipped_nested,
        })
    }

    #[must_use]
    pub fn with_excludes_file(mut self, path: Option<PathBuf>) -> Self {
        self.excludes_file = path;
        self
    }

    pub fn filtered_paths(&self, tree: &GitId, paths: &[&RepoPath]) -> Result<Vec<RepoPath>> {
        self.check_id(tree)?;
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let mut input = Vec::new();
        for path in paths {
            input.extend_from_slice(path.as_bytes());
            input.push(0);
        }
        let mut cmd = self
            .git("check-attr")
            .read_only()
            .args(["check-attr", "-z", "--stdin"]);
        if self.version >= MIN_CHECK_ATTR_SOURCE_VERSION {
            cmd = cmd.arg(format!("--source={}", tree.hex()));
        }
        let out = cmd.args(["filter"]).stdin(input).run()?;
        let mut fields = out.split(|b| *b == 0);
        let mut filtered = Vec::new();
        while let (Some(path), Some(_), Some(info)) = (fields.next(), fields.next(), fields.next())
        {
            if !matches!(info, b"unspecified" | b"unset" | b"set") && !path.is_empty() {
                let path = RepoPath::from_git("check-attr", path)?;
                if !filtered.contains(&path) {
                    filtered.push(path);
                }
            }
        }
        Ok(filtered)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkingCapture {
    pub tree: GitId,
    pub skipped_nested: Vec<RepoPath>,
}

pub const MAX_SKIPPED_NESTED: usize = 64;

fn nested_without_commit(stderr: &Diagnostics) -> Option<RepoPath> {
    const PREFIX: &[u8] = b"error: '";
    const SUFFIX: &[u8] = b"' does not have a commit checked out";
    stderr.as_bytes().split(|b| *b == b'\n').find_map(|line| {
        let inner = line.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
        RepoPath::new(inner.strip_suffix(b"/").unwrap_or(inner).to_vec()).ok()
    })
}

pub fn user_excludes_file() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| h.is_absolute())?;
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|x| x.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    let mut configured = None;
    for file in [xdg.join("git/config"), home.join(".gitconfig")] {
        if !file.is_file() {
            continue;
        }
        let out = GitCommand::new("config-excludes")
            .without_config_scan()
            .read_only()
            .env("HOME", &home)
            .args(["config", "--file"])
            .arg(&file)
            .args(["--includes", "--type=path", "--get", "core.excludesFile"])
            .output()
            .ok()?;
        if out.status.success() {
            let value = out.stdout.strip_suffix(b"\n").unwrap_or(&out.stdout);
            configured = Some(PathBuf::from(OsString::from_vec(value.to_vec())));
        }
    }
    let path = configured.unwrap_or_else(|| xdg.join("git/ignore"));
    (path.is_absolute() && path.is_file()).then_some(path)
}

#[must_use]
pub fn escape_branch_name(raw: &[u8]) -> String {
    let mut out = String::new();
    for chunk in raw.utf8_chunks() {
        out.push_str(chunk.valid());
        for byte in chunk.invalid() {
            out.push_str(&format!("\\x{byte:02x}"));
        }
    }
    out
}

#[must_use]
pub fn unescape_branch_name(name: &str) -> Option<Vec<u8>> {
    let bytes = name.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 4)?).ok()?;
            let value = u8::from_str_radix(hex.strip_prefix('x')?, 16).ok()?;
            out.push(value);
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

pub fn validate_refname(name: &str) -> Result<()> {
    let valid = name.starts_with("refs/")
        && name.len() <= 1024
        && !name.bytes().any(|b| {
            b <= b' ' || b == 0x7f || matches!(b, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
        })
        && !name.ends_with('.')
        && !name.contains("..")
        && !name.contains("@{")
        && name
            .split('/')
            .all(|c| !c.is_empty() && !c.starts_with('.') && !c.ends_with(".lock"));
    if valid {
        Ok(())
    } else {
        Err(GitError::InvalidRefName)
    }
}

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
