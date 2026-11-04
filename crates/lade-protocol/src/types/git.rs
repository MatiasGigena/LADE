//! Git object ids, bound to their hash algorithm.

use crate::{WireError, schema};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitAlgorithm {
    Sha1,
    Sha256,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawGitId", into = "RawGitId")]
pub struct GitId {
    algorithm: GitAlgorithm,
    hex: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGitId {
    algorithm: GitAlgorithm,
    hex: String,
}
impl GitId {
    /// # Errors
    ///
    /// `WireError::InvalidSchema` unless `hex` is a lowercase object id of the algorithm's length.
    pub fn new(algorithm: GitAlgorithm, hex: impl Into<String>) -> Result<Self, WireError> {
        Self::try_from(RawGitId {
            algorithm,
            hex: hex.into(),
        })
    }
    /// An object id whose algorithm is inferred from its length: 40 digits are SHA-1, others
    /// SHA-256.
    ///
    /// # Errors
    ///
    /// `WireError::InvalidSchema` unless `hex` is a valid lowercase object id.
    pub fn from_hex(hex: &str) -> Result<Self, WireError> {
        let algorithm = if hex.len() == 40 {
            GitAlgorithm::Sha1
        } else {
            GitAlgorithm::Sha256
        };
        Self::new(algorithm, hex)
    }
    #[must_use]
    pub fn algorithm(&self) -> GitAlgorithm {
        self.algorithm
    }
    #[must_use]
    pub fn hex(&self) -> &str {
        &self.hex
    }
}
impl TryFrom<RawGitId> for GitId {
    type Error = WireError;
    fn try_from(raw: RawGitId) -> Result<Self, WireError> {
        let format = match raw.algorithm {
            GitAlgorithm::Sha1 => "git-sha1",
            GitAlgorithm::Sha256 => "sha256",
        };
        if schema::format(&raw.hex, format) {
            Ok(Self {
                algorithm: raw.algorithm,
                hex: raw.hex,
            })
        } else {
            Err(WireError::InvalidSchema)
        }
    }
}
impl From<GitId> for RawGitId {
    fn from(id: GitId) -> Self {
        Self {
            algorithm: id.algorithm,
            hex: id.hex,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_ids_bind_length_to_algorithm() {
        assert!(GitId::new(GitAlgorithm::Sha1, "a".repeat(40)).is_ok());
        assert!(GitId::new(GitAlgorithm::Sha1, "a".repeat(64)).is_err());
        assert!(GitId::new(GitAlgorithm::Sha256, "a".repeat(40)).is_err());
        assert!(GitId::new(GitAlgorithm::Sha1, "A".repeat(40)).is_err());
        assert_eq!(
            GitId::from_hex(&"b".repeat(64)).unwrap().algorithm(),
            GitAlgorithm::Sha256
        );
    }
}
