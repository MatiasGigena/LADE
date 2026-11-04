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

use crate::{WireError, schema};
use serde::{Deserialize, Serialize};

macro_rules! wire_string {
    ($(#[$meta:meta])* $name:ident, $format:literal) => {
        $(#[$meta])*
        #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);
        impl $name {
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

wire_string!(Id, "uuid-v4");
wire_string!(Counter, "u64");
wire_string!(Digest, "sha256");
wire_string!(Capability, "capability");

/// File name of the control socket inside the service runtime directory.
pub const CONTROL_SOCKET_NAME: &str = "control.sock";
/// File name of the admission capability inside the service runtime directory.
pub const CONTROL_CAPABILITY_NAME: &str = "control.cap";
wire_string!(Timestamp, "timestamp");
wire_string!(PageLimit, "page-limit");
wire_string!(ReadLimit, "read-limit");

impl Id {
    pub fn from_random_bytes(mut bytes: [u8; 16]) -> Self {
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
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
    pub fn new(value: u64) -> Self {
        Self(value.to_string())
    }
    pub fn value(&self) -> u64 {
        self.0.parse().expect("validated u64")
    }
}
impl From<u64> for Counter {
    fn from(value: u64) -> Self {
        Self::new(value)
    }
}

impl PageLimit {
    pub fn value(&self) -> u32 {
        self.0.parse().expect("validated page limit")
    }
}
impl ReadLimit {
    pub fn value(&self) -> u32 {
        self.0.parse().expect("validated read limit")
    }
}

impl Digest {
    pub fn from_bytes(digest: [u8; 32]) -> Self {
        Self(digest.iter().map(|b| format!("{b:02x}")).collect())
    }
}

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

pub fn truncate_chars(text: &str, max_chars: usize) -> &str {
    text.char_indices()
        .nth(max_chars)
        .map_or(text, |(end, _)| &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

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
