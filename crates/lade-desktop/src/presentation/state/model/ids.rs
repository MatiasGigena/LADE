//! Identifiers, timestamps, Git object ids, repository paths and model references.

macro_rules! id_type {
    ($($name:ident),* $(,)?) => {$(
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            #[allow(dead_code, reason = "generated for every id type; not every type reads each accessor")]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            #[allow(dead_code, reason = "generated for every id type; not every type reads each accessor")]
            pub fn short(&self) -> &str {
                self.0.get(..8).unwrap_or(&self.0)
            }
        }
    )*};
}

id_type!(
    WorkspaceId,
    ObjectiveId,
    SessionId,
    CandidateId,
    CaptureId,
    DraftId,
    InputId,
    OperationId,
    VerificationId,
    ApprovalId,
    ActivityItemId,
    ArtifactDigest,
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(pub i64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GitAlgorithm {
    Sha1,
    Sha256,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GitId {
    pub algorithm: GitAlgorithm,
    pub hex: String,
}

impl GitId {
    #[cfg(test)]
    pub fn sha1(hex: impl Into<String>) -> Self {
        Self {
            algorithm: GitAlgorithm::Sha1,
            hex: hex.into(),
        }
    }

    pub fn short(&self) -> &str {
        self.hex.get(..7).unwrap_or(&self.hex)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RepoPath {
    pub bytes: Vec<u8>,
    pub display: String,
}

#[cfg(test)]
impl RepoPath {
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        let display = String::from_utf8_lossy(&bytes).into_owned();
        Self { bytes, display }
    }

    pub fn utf8(path: &str) -> Self {
        Self::from_bytes(path.as_bytes().to_vec())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ModelRef {
    pub provider: String,
    pub id: String,
}

impl ModelRef {
    pub fn new(provider: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            id: id.into(),
        }
    }

    pub fn label(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }
}
