use core::fmt;

use crate::error::DomainError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Uuid(u128);

const VERSION_SHIFT: u32 = 76;
const VARIANT_SHIFT: u32 = 62;

impl Uuid {
    /// Parses a lowercase hyphenated version 4 UUID.
    ///
    /// # Errors
    ///
    /// `DomainError::InvalidRequest` for any other text.
    pub fn parse(text: &str) -> Result<Self, DomainError> {
        const INVALID: DomainError =
            DomainError::InvalidRequest("identifier is not a lowercase UUIDv4");
        let bytes = text.as_bytes();
        if bytes.len() != 36 {
            return Err(INVALID);
        }
        let mut value: u128 = 0;
        for (index, &byte) in bytes.iter().enumerate() {
            if matches!(index, 8 | 13 | 18 | 23) {
                if byte != b'-' {
                    return Err(INVALID);
                }
                continue;
            }
            let nibble = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                _ => return Err(INVALID),
            };
            value = (value << 4) | u128::from(nibble);
        }
        if (value >> VERSION_SHIFT) & 0xf != 4 || (value >> VARIANT_SHIFT) & 0b11 != 0b10 {
            return Err(INVALID);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn from_random_bytes(bytes: [u8; 16]) -> Self {
        let mut value = u128::from_be_bytes(bytes);
        value = (value & !(0xf << VERSION_SHIFT)) | (0x4 << VERSION_SHIFT);
        value = (value & !(0b11 << VARIANT_SHIFT)) | (0b10 << VARIANT_SHIFT);
        Self(value)
    }

    #[must_use]
    pub fn short(self) -> String {
        let mut text = self.to_string();
        text.truncate(8);
        text
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for digit in 0..32u32 {
            if matches!(digit, 8 | 12 | 16 | 20) {
                formatter.write_str("-")?;
            }
            let nibble = ((self.0 >> ((31 - digit) * 4)) & 0xf) as u32;
            let character = char::from_digit(nibble, 16).unwrap_or('0');
            fmt::Write::write_char(formatter, character)?;
        }
        Ok(())
    }
}

macro_rules! uuid_ids {
    ($($(#[$meta:meta])* $name:ident),+ $(,)?) => {$(
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Uuid);

        impl $name {
            pub const fn new(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// # Errors
            ///
            /// `DomainError::InvalidRequest` unless `text` is a lowercase hyphenated version 4
            /// UUID.
            pub fn parse(text: &str) -> Result<Self, DomainError> {
                Uuid::parse(text).map(Self)
            }

            pub const fn uuid(self) -> Uuid {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, formatter)
            }
        }
    )+};
}

uuid_ids!(
    ObjectiveId,
    WorkspaceId,
    SessionId,
    DraftId,
    InputId,
    CandidateId,
    VerificationId,
    ApprovalId,
    OperationId,
    CaptureId,
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GitAlgorithm {
    Sha1,
    Sha256,
}

impl GitAlgorithm {
    #[must_use]
    pub const fn hex_len(self) -> usize {
        match self {
            Self::Sha1 => 40,
            Self::Sha256 => 64,
        }
    }

    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GitId {
    algorithm: GitAlgorithm,
    hex: String,
}

impl GitId {
    /// A full object id in `algorithm`'s hex form.
    ///
    /// # Errors
    ///
    /// `DomainError::InvalidRequest` unless `hex` has the algorithm's length in lowercase hex and
    /// is not all zeros.
    pub fn new(algorithm: GitAlgorithm, hex: &str) -> Result<Self, DomainError> {
        if !is_lower_hex(hex, algorithm.hex_len()) || hex.bytes().all(|byte| byte == b'0') {
            return Err(DomainError::InvalidRequest(
                "git id is not a full lowercase object id",
            ));
        }
        Ok(Self {
            algorithm,
            hex: hex.to_string(),
        })
    }

    #[must_use]
    pub const fn algorithm(&self) -> GitAlgorithm {
        self.algorithm
    }

    #[must_use]
    pub fn hex(&self) -> &str {
        &self.hex
    }
}

impl fmt::Display for GitId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.hex)
    }
}

pub(crate) fn is_lower_hex(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(i64);

impl Timestamp {
    #[must_use]
    pub const fn from_unix_ms(millis: i64) -> Self {
        Self(millis)
    }

    #[must_use]
    pub const fn unix_ms(self) -> i64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord, Hash)]
pub struct Revision(u64);

impl Revision {
    pub const INITIAL: Self = Self(1);

    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }

    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }

    pub(crate) fn advance(self) -> Result<Self, DomainError> {
        self.next()
            .ok_or(DomainError::LimitReached("revision exhausted"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "1b4e28ba-2fa1-41d2-883f-0016d3cca427";

    #[test]
    fn revisions_advance_without_wrapping() {
        assert_eq!(Revision::new(0).next(), Some(Revision::new(1)));
        assert_eq!(Revision::new(u64::MAX).next(), None);
        assert!(matches!(
            Revision::new(u64::MAX).advance(),
            Err(DomainError::LimitReached(_))
        ));
    }

    #[test]
    fn uuid_round_trips_canonical_text() {
        let uuid = Uuid::parse(SAMPLE).expect("valid");
        assert_eq!(uuid.to_string(), SAMPLE);
        assert_eq!(uuid.short(), "1b4e28ba");
        assert_eq!(
            ObjectiveId::parse(SAMPLE)
                .map(|id| id.to_string())
                .as_deref(),
            Ok(SAMPLE)
        );
    }

    #[test]
    fn uuid_rejects_non_canonical_text() {
        for text in [
            "",
            "1B4E28BA-2FA1-41D2-883F-0016D3CCA427",
            "1b4e28ba2fa141d2883f0016d3cca427",
            "1b4e28ba-2fa1-11d2-883f-0016d3cca427",
            "1b4e28ba-2fa1-41d2-c83f-0016d3cca427",
            "1b4e28ba-2fa1-41d2-883f-0016d3cca42",
            "1b4e28ba-2fa1-41d2-883f-0016d3cca4277",
            "1b4e28ba_2fa1-41d2-883f-0016d3cca427",
            "1b4e28ba-2fa1-41d2-883f-0016d3cca42g",
            "1b4e28ba-2fa1-41d2-883f-0016d3cca4é",
        ] {
            assert!(Uuid::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn random_bytes_become_valid_v4() {
        for bytes in [[0u8; 16], [0xff; 16], [0x5a; 16]] {
            let uuid = Uuid::from_random_bytes(bytes);
            assert_eq!(Uuid::parse(&uuid.to_string()), Ok(uuid));
        }
    }

    #[test]
    fn git_ids_are_validated_per_algorithm() {
        let sha1 = "a".repeat(40);
        let sha256 = "0123456789abcdef".repeat(4);
        assert!(GitId::new(GitAlgorithm::Sha1, &sha1).is_ok());
        assert!(GitId::new(GitAlgorithm::Sha256, &sha256).is_ok());
        assert!(GitId::new(GitAlgorithm::Sha1, &sha256).is_err());
        assert!(GitId::new(GitAlgorithm::Sha256, &sha1).is_err());
        assert!(GitId::new(GitAlgorithm::Sha1, &"A".repeat(40)).is_err());
        assert!(GitId::new(GitAlgorithm::Sha1, &"0".repeat(40)).is_err());
        assert!(GitId::new(GitAlgorithm::Sha1, "abc").is_err());
        assert_eq!(GitAlgorithm::Sha256.code(), "sha256");
    }
}
