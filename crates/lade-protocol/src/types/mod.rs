//! Validated scalar wire types shared by the client and worker contracts.

mod git;
mod path;
mod session;
mod timestamp;

pub use git::{GitAlgorithm, GitId};
pub use path::{MAX_PATH_BYTES, WirePath};
pub use session::{
    ActivityRole, DeliveryMode, ForkPosition, Model, SessionChangeReason, ThinkingLevel, TreeEntry,
    TreeEntryKind, UiAnswer, UiCloseReason, UiPrompt,
};
pub use timestamp::Timestamp;

use crate::{WireError, schema};
use serde::{Deserialize, Serialize};

macro_rules! wire_string {
    ($(#[$meta:meta])* $name:ident, $format:literal) => {
        $(#[$meta])*
        #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);
        impl $name {
            /// Parses the validated wire form.
            ///
            /// # Errors
            ///
            /// `WireError::InvalidSchema` when the text does not have the type's wire format.
            pub fn parse(value: impl Into<String>) -> Result<Self, WireError> {
                Self::try_from(value.into())
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl TryFrom<String> for $name {
            type Error = WireError;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                if schema::format(&value, $format) {
                    Ok(Self(value))
                } else {
                    Err(WireError::InvalidSchema)
                }
            }
        }
        impl From<$name> for String {
            fn from(value: $name) -> String {
                value.0
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

macro_rules! wire_number {
    ($(#[$meta:meta])* $name:ident($ty:ty), $format:literal) => {
        $(#[$meta])*
        #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name($ty);
        impl $name {
            /// Parses the canonical decimal wire form.
            ///
            /// # Errors
            ///
            /// [`WireError::InvalidSchema`] when the text is not a canonical decimal in range.
            pub fn parse(value: impl Into<String>) -> Result<Self, WireError> {
                Self::try_from(value.into())
            }
            #[must_use]
            pub const fn value(&self) -> $ty {
                self.0
            }
        }
        impl TryFrom<String> for $name {
            type Error = WireError;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                if !schema::format(&value, $format) {
                    return Err(WireError::InvalidSchema);
                }
                value.parse().map(Self).map_err(|_| WireError::InvalidSchema)
            }
        }
        impl From<$name> for String {
            fn from(value: $name) -> String {
                value.0.to_string()
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

wire_string!(Id, "uuid-v4");
wire_string!(Digest, "sha256");
wire_string!(Capability, "capability");
wire_number!(Counter(u64), "u64");
wire_number!(PageLimit(u32), "page-limit");
wire_number!(ReadLimit(u32), "read-limit");

/// File name of the control socket inside the service runtime directory.
pub const CONTROL_SOCKET_NAME: &str = "control.sock";
/// File name of the admission capability inside the service runtime directory.
pub const CONTROL_CAPABILITY_NAME: &str = "control.cap";

fn lower_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

impl Id {
    /// Builds a version 4 UUID from 16 random bytes, overwriting the version and variant bits.
    #[must_use]
    pub fn from_random_bytes(mut bytes: [u8; 16]) -> Self {
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let hex = lower_hex(&bytes);
        Self(format!(
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        ))
    }
}

impl Counter {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
}
impl From<u64> for Counter {
    fn from(value: u64) -> Self {
        Self::new(value)
    }
}

impl Digest {
    #[must_use]
    pub fn from_bytes(digest: [u8; 32]) -> Self {
        Self(lower_hex(&digest))
    }
}

#[must_use]
pub fn truncate_utf8(text: &str, max_bytes: usize) -> (&str, bool) {
    if text.len() <= max_bytes {
        return (text, false);
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

#[must_use]
pub fn truncate_chars(text: &str, max_chars: usize) -> &str {
    text.char_indices()
        .nth(max_chars)
        .map_or(text, |(end, _)| &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_keep_their_canonical_decimal_form() {
        let counter = Counter::parse("18446744073709551615").unwrap();
        assert_eq!(counter.value(), u64::MAX);
        assert_eq!(counter.to_string(), "18446744073709551615");
        assert_eq!(
            serde_json::to_value(Counter::new(7)).unwrap(),
            serde_json::json!("7")
        );
        assert!(Counter::parse("07").is_err());
        assert!(Counter::parse("18446744073709551616").is_err());
        assert!(
            Counter::new(9) < Counter::new(10),
            "numeric, not lexical, order"
        );
        assert_eq!(PageLimit::parse("200").unwrap().value(), 200);
        assert!(PageLimit::parse("201").is_err());
        assert_eq!(ReadLimit::parse("262144").unwrap().value(), 262_144);
        assert!(ReadLimit::parse("0").is_err());
    }

    #[test]
    fn digests_are_lower_hex() {
        let digest = Digest::from_bytes([0xab; 32]);
        assert_eq!(digest.as_str(), "ab".repeat(32));
        assert!(Digest::parse(digest.as_str()).is_ok());
    }

    #[test]
    fn random_ids_are_v4() {
        let id = Id::from_random_bytes([0xff; 16]);
        assert_eq!(id.as_str(), "ffffffff-ffff-4fff-bfff-ffffffffffff");
        assert!(Id::parse(id.as_str()).is_ok());
    }

    #[test]
    fn truncation_respects_boundaries() {
        assert_eq!(truncate_utf8("aé", 2), ("a", true));
        assert_eq!(truncate_utf8("aé", 3), ("aé", false));
        assert_eq!(truncate_chars("😀😀😀", 2), "😀😀");
        assert_eq!(truncate_chars("ab", 5), "ab");
    }
}
