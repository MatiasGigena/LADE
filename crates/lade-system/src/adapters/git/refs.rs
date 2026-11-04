//! Branches and refs: listing and resolving them, compare-and-swap updates, retention refs
//! that keep objects alive, and branch-name escaping and validation.

use std::fmt::Write as _;

use super::command::GitOutput;
use super::{
    GitError, GitId, LADE_IDENTITY_EMAIL, LADE_IDENTITY_NAME, RETENTION_PREFIX, Repository, Result,
    lines,
};

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

impl Repository {
    /// Every local branch with its commit, sorted by its raw name.
    ///
    /// # Errors
    ///
    /// Returns a command error when `git for-each-ref` fails, or [`GitError::Parse`] when its output
    /// is malformed.
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

    /// The branch `HEAD` points at, or `None` when `HEAD` is detached or not a UTF-8 branch.
    ///
    /// # Errors
    ///
    /// Returns a command error when `git symbolic-ref` cannot be run.
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

    /// The object `refname` points at, or `None` when the ref does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`GitError::InvalidRefName`] for a malformed `refname`, [`GitError::Failed`] when
    /// `git rev-parse` fails for another reason, or [`GitError::Parse`] when its output is not an id.
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

    /// The commit that `rev` (a full object id or a ref name) names.
    ///
    /// # Errors
    ///
    /// Returns [`GitError::InvalidRefName`] when `rev` is neither an id nor a valid ref name,
    /// [`GitError::NotFound`] when it names no commit, or a command error when `git` cannot run.
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

    /// Writes a commit of `spec.tree` with `spec.parents` and LADE's identity, without
    /// moving any ref.
    ///
    /// # Errors
    ///
    /// Returns [`GitError::InvalidId`] when an id has another object format than the repository, or
    /// a command error when `git commit-tree` fails.
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

    /// Points `refname` at `new` only if it still points at `old` (or does not exist when
    /// `old` is `None`).
    ///
    /// # Errors
    ///
    /// Returns [`GitError::StaleRef`] with the ref's current value when it moved,
    /// [`GitError::InvalidRefName`] or [`GitError::InvalidId`] for malformed input, or
    /// [`GitError::Failed`] when `git update-ref` fails for another reason.
    pub fn update_ref_cas(&self, refname: &str, new: &GitId, old: Option<&GitId>) -> Result<()> {
        validate_refname(refname)?;
        self.check_id(new)?;
        let old_hex = match old {
            Some(id) => {
                self.check_id(id)?;
                id.hex().to_owned()
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

    /// Deletes `refname` only if it still points at `old`.
    ///
    /// # Errors
    ///
    /// Fails as [`Repository::update_ref_cas`] does.
    pub fn delete_ref_cas(&self, refname: &str, old: &GitId) -> Result<()> {
        validate_refname(refname)?;
        self.check_id(old)?;
        let out = self
            .git("update-ref")
            .args(["update-ref", "--no-deref", "-d", refname, old.hex()])
            .output()?;
        self.cas_result(refname, Some(old), out)
    }

    fn cas_result(&self, refname: &str, expected: Option<&GitId>, out: GitOutput) -> Result<()> {
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

    /// The ref name under which LADE retains objects for `id`.
    ///
    /// # Errors
    ///
    /// Returns [`GitError::InvalidRefName`] when `id` is empty, longer than 128 bytes, or holds
    /// anything but ASCII letters, digits, `-` and `_`.
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

    /// Creates the retention ref for `id` pointing at `object`, so Git keeps it; an existing
    /// ref that already points there is accepted.
    ///
    /// # Errors
    ///
    /// Returns [`GitError::StaleRef`] when the retention ref exists for another object, or the errors
    /// of [`Repository::retention_ref`] and [`Repository::update_ref_cas`].
    pub fn retain(&self, id: &str, object: &GitId) -> Result<String> {
        let refname = Self::retention_ref(id)?;
        match self.update_ref_cas(&refname, object, None) {
            Err(GitError::StaleRef { current, .. }) if current.as_ref() == Some(object) => {
                Ok(refname)
            }
            other => other.map(|()| refname),
        }
    }

    /// Deletes the retention ref for `id` if it still points at `expected`.
    ///
    /// # Errors
    ///
    /// Fails as [`Repository::retention_ref`] and [`Repository::delete_ref_cas`] do.
    pub fn release_retention(&self, id: &str, expected: &GitId) -> Result<()> {
        self.delete_ref_cas(&Self::retention_ref(id)?, expected)
    }
}

#[must_use]
pub fn escape_branch_name(raw: &[u8]) -> String {
    let mut out = String::new();
    for chunk in raw.utf8_chunks() {
        out.push_str(chunk.valid());
        for byte in chunk.invalid() {
            let _ = write!(out, "\\x{byte:02x}");
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

/// Checks that `name` is a full ref name under `refs/` that `git check-ref-format` would
/// accept.
///
/// # Errors
///
/// Returns [`GitError::InvalidRefName`] when it is not.
#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "a ref component may not end in `.lock`, which Git checks case-sensitively"
)]
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

/// Checks that `name` is a valid short branch name.
///
/// # Errors
///
/// Returns [`GitError::InvalidRefName`] when it starts with `-`, is `HEAD`, or does not form a
/// valid ref under `refs/heads/`.
pub fn validate_branch_name(name: &str) -> Result<()> {
    if name.starts_with('-') || name == "HEAD" {
        return Err(GitError::InvalidRefName);
    }
    validate_refname(&format!("refs/heads/{name}"))
}
