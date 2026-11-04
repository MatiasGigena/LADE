//! Git's value types as the adapter validates them: object formats and ids, repository
//! paths and the Git version.

use std::ffi::OsStr;
use std::fmt;
use std::os::unix::ffi::OsStrExt;

use super::{GitError, Result};

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

    pub(super) fn zero(self) -> String {
        "0".repeat(self.hex_len())
    }
}

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GitId {
    format: ObjectFormat,
    hex: String,
}

impl GitId {
    /// Parses a full lowercase object id of `format`.
    ///
    /// # Errors
    ///
    /// Returns [`GitError::InvalidId`] when `hex` is not exactly as long as `format`'s ids or holds
    /// anything but lowercase hex digits.
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

    pub(super) fn is_zero(&self) -> bool {
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
