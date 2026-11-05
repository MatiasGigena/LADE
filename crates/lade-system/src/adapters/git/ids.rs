//! Git's value types as the adapter validates them: object ids (the domain's [`GitId`]),
//! repository paths and the Git version.

use std::ffi::OsStr;
use std::fmt;
use std::os::unix::ffi::OsStrExt;

use lade_core::domain::ids::{GitAlgorithm, GitId};

use super::{GitError, Result};

/// The id of the empty tree in `algorithm`'s object format.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "both literals are full, non-zero lowercase object ids of their algorithm"
)]
pub fn empty_tree(algorithm: GitAlgorithm) -> GitId {
    let hex = match algorithm {
        GitAlgorithm::Sha1 => "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
        GitAlgorithm::Sha256 => "6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321",
    };
    GitId::new(algorithm, hex).expect("the empty tree id is a full object id")
}

/// The all-zero id Git uses for "no object" in `algorithm`'s object format.
pub(super) fn zero(algorithm: GitAlgorithm) -> String {
    "0".repeat(algorithm.hex_len())
}

/// Parses a full, non-zero lowercase object id of `algorithm`.
///
/// # Errors
///
/// Returns [`GitError::InvalidId`] when `hex` is not exactly as long as `algorithm`'s ids, holds
/// anything but lowercase hex digits, or is all zeros.
pub fn parse_id(algorithm: GitAlgorithm, hex: &str) -> Result<GitId> {
    GitId::new(algorithm, hex).map_err(|_| GitError::InvalidId)
}

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RepoPath(Vec<u8>);

impl RepoPath {
    /// A repository-relative path: non-empty, without NUL bytes and without empty, `.` or
    /// `..` components.
    ///
    /// # Errors
    ///
    /// Returns [`GitError::InvalidPath`] when `bytes` breaks one of those rules.
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

    pub(super) fn from_git(op: &'static str, bytes: &[u8]) -> Result<Self> {
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
        let patch = parts.next().and_then(std::result::Result::ok).unwrap_or(0);
        Some(Self::new(major, minor, patch))
    }
}

impl fmt::Display for GitVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}
