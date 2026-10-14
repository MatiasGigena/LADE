use core::fmt;

use crate::domain::ids::{CandidateId, CaptureId, is_lower_hex};
use crate::error::DomainError;

pub const MAX_TEXT_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ArtifactDigest(String);

impl ArtifactDigest {
    pub fn parse(hex: &str) -> Result<Self, DomainError> {
        if !is_lower_hex(hex, 64) {
            return Err(DomainError::InvalidRequest(
                "artifact digest is not 64 lowercase hex digits",
            ));
        }
        Ok(Self(hex.to_string()))
    }

    pub fn hex(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ArtifactDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RepoPath(Vec<u8>);

pub const MAX_PATH_BYTES: usize = 4096;

impl RepoPath {
    pub fn new(bytes: Vec<u8>) -> Result<Self, DomainError> {
        let valid = !bytes.is_empty()
            && bytes.len() <= MAX_PATH_BYTES
            && !bytes.contains(&0)
            && bytes
                .split(|&byte| byte == b'/')
                .all(|component| !component.is_empty() && component != b"." && component != b"..");
        if !valid {
            return Err(DomainError::InvalidRequest(
                "path is not a normalized repository-relative path",
            ));
        }
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn display(&self) -> String {
        String::from_utf8_lossy(&self.0).into_owned()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InspectedVersion {
    Working { capture_id: CaptureId },
    Candidate { candidate_id: CandidateId },
}

impl InspectedVersion {
    pub fn label(self) -> String {
        match self {
            Self::Working { capture_id } => {
                format!("working capture {}", capture_id.uuid().short())
            }
            Self::Candidate { candidate_id } => {
                format!("candidate {}", candidate_id.uuid().short())
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Old,
    New,
}

impl Side {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Old => "old",
            Self::New => "new",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ByteRange {
    start: u64,
    end: u64,
}

impl ByteRange {
    pub fn new(start: u64, end: u64) -> Result<Self, DomainError> {
        if start >= end {
            return Err(DomainError::InvalidRequest(
                "byte range must be non-empty and ordered",
            ));
        }
        Ok(Self { start, end })
    }

    pub const fn start(self) -> u64 {
        self.start
    }

    pub const fn end(self) -> u64 {
        self.end
    }

    pub const fn byte_len(self) -> u64 {
        self.end - self.start
    }

    fn slice(self, bytes: &[u8]) -> Option<&[u8]> {
        let start = usize::try_from(self.start).ok()?;
        let end = usize::try_from(self.end).ok()?;
        bytes.get(start..end)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    artifact_digest: ArtifactDigest,
    path: RepoPath,
    version: InspectedVersion,
    side: Side,
    byte_range: ByteRange,
    selected_text: String,
    line_hint: u32,
}

impl Reference {
    pub fn new(
        artifact_digest: ArtifactDigest,
        path: RepoPath,
        version: InspectedVersion,
        side: Side,
        byte_range: ByteRange,
        selected_text: String,
        line_hint: u32,
    ) -> Result<Self, DomainError> {
        if u64::try_from(selected_text.len()).ok() != Some(byte_range.byte_len()) {
            return Err(DomainError::InvalidRequest(
                "selected text length differs from byte range",
            ));
        }
        if selected_text.len() > MAX_TEXT_BYTES {
            return Err(DomainError::LimitReached("selected text too large"));
        }
        if line_hint == 0 {
            return Err(DomainError::InvalidRequest("line hint is 1-based"));
        }
        Ok(Self {
            artifact_digest,
            path,
            version,
            side,
            byte_range,
            selected_text,
            line_hint,
        })
    }

    pub fn artifact_digest(&self) -> &ArtifactDigest {
        &self.artifact_digest
    }

    pub fn path(&self) -> &RepoPath {
        &self.path
    }

    pub const fn version(&self) -> InspectedVersion {
        self.version
    }

    pub const fn side(&self) -> Side {
        self.side
    }

    pub const fn byte_range(&self) -> ByteRange {
        self.byte_range
    }

    pub fn selected_text(&self) -> &str {
        &self.selected_text
    }

    pub const fn line_hint(&self) -> u32 {
        self.line_hint
    }

    pub fn staleness(&self, current: CurrentContent<'_>) -> Staleness {
        match current {
            CurrentContent::Missing => Staleness::Missing,
            CurrentContent::Present { digest, .. } if *digest == self.artifact_digest => {
                Staleness::Fresh
            }
            CurrentContent::Present { bytes, .. } => {
                if self.byte_range.slice(bytes) == Some(self.selected_text.as_bytes()) {
                    Staleness::Fresh
                } else {
                    Staleness::Changed
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum CurrentContent<'a> {
    Missing,
    Present {
        digest: &'a ArtifactDigest,
        bytes: &'a [u8],
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Staleness {
    Fresh,
    Changed,
    Missing,
}

impl Staleness {
    pub const fn is_stale(self) -> bool {
        !matches!(self, Self::Fresh)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{capture_id, digest, reference_at};

    #[test]
    fn repo_paths_are_lossless_and_normalized() {
        let raw = vec![b's', b'r', b'c', b'/', 0xff, b'.', b'r', b's'];
        let path = RepoPath::new(raw.clone()).expect("non-UTF-8 is allowed");
        assert_eq!(path.as_bytes(), raw.as_slice());
        assert_eq!(path.display(), "src/\u{fffd}.rs");
        for bad in [
            &b""[..],
            b"/abs",
            b"a//b",
            b"a/./b",
            b"../x",
            b"a/",
            b"a\0b",
        ] {
            assert!(RepoPath::new(bad.to_vec()).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn digests_are_validated() {
        assert!(ArtifactDigest::parse(&"a".repeat(64)).is_ok());
        assert!(ArtifactDigest::parse(&"a".repeat(63)).is_err());
        assert!(ArtifactDigest::parse(&"A".repeat(64)).is_err());
    }

    #[test]
    fn byte_ranges_are_non_empty() {
        assert!(ByteRange::new(3, 3).is_err());
        assert!(ByteRange::new(4, 3).is_err());
        assert_eq!(ByteRange::new(3, 5).map(ByteRange::byte_len), Ok(2));
    }

    #[test]
    fn selected_text_must_match_range_in_bytes() {
        let text = "héllo";
        assert!(reference_at(1, 0, 6, text).is_ok());
        assert!(reference_at(1, 0, 5, text).is_err());
    }

    #[test]
    fn same_digest_is_fresh_without_byte_comparison() {
        let reference = reference_at(1, 0, 3, "abc").expect("valid");
        let same = digest(1);
        assert_eq!(
            reference.staleness(CurrentContent::Present {
                digest: &same,
                bytes: b""
            }),
            Staleness::Fresh
        );
    }

    #[test]
    fn changed_file_is_fresh_while_selected_bytes_stay_at_offsets() {
        let file = "fn é() {}\r\n\tlet x = 1;\r\n";
        let start = file.find("\tlet").expect("present");
        let end = start + "\tlet x".len();
        let reference = reference_at(1, start, end, "\tlet x").expect("valid");
        let other = digest(2);
        let edited_after = format!("{file}// trailing edit\n");
        assert_eq!(
            reference.staleness(CurrentContent::Present {
                digest: &other,
                bytes: edited_after.as_bytes()
            }),
            Staleness::Fresh
        );
        let shifted = file.replacen("é", "éé", 1);
        assert_eq!(
            reference.staleness(CurrentContent::Present {
                digest: &other,
                bytes: shifted.as_bytes()
            }),
            Staleness::Changed
        );
        let lf = file.replace("\r\n", "\n");
        assert_eq!(
            reference.staleness(CurrentContent::Present {
                digest: &other,
                bytes: lf.as_bytes()
            }),
            Staleness::Changed
        );
        assert_eq!(
            reference.staleness(CurrentContent::Present {
                digest: &other,
                bytes: &file.as_bytes()[..start]
            }),
            Staleness::Changed
        );
        assert_eq!(
            reference.staleness(CurrentContent::Missing),
            Staleness::Missing
        );
        assert!(
            Staleness::Missing.is_stale()
                && Staleness::Changed.is_stale()
                && !Staleness::Fresh.is_stale()
        );
    }

    #[test]
    fn version_labels_are_short_and_distinct() {
        let label = InspectedVersion::Working {
            capture_id: capture_id(7),
        }
        .label();
        assert!(label.starts_with("working capture "));
        assert_eq!(label.len(), "working capture ".len() + 8);
    }
}
