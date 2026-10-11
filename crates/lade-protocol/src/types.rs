//! Validated wire primitives shared by the client and worker contracts.
//! A value of these types always satisfies its schema format; construction validates.
use crate::{WireError, base64, schema};
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};

macro_rules! wire_string {
    ($(#[$meta:meta])* $name:ident, $format:literal) => {
        $(#[$meta])*
        #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);
        impl $name {
            /// Validates `value` against the schema format.
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
                    Err(WireError("invalid_schema"))
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

wire_string!(
    /// Canonical lowercase UUIDv4 (RFC variant). Public correlation, never a capability.
    Id,
    "uuid-v4"
);
wire_string!(
    /// Canonical unsigned 64-bit decimal string (no sign, no leading zero except `0`).
    Counter,
    "u64"
);
wire_string!(
    /// Lowercase hex SHA-256 digest.
    Digest,
    "sha256"
);
wire_string!(
    /// 64 lowercase hex characters (256 bits). Secret: never log or echo.
    Capability,
    "capability"
);
wire_string!(
    /// RFC 3339 UTC timestamp in the single canonical form `YYYY-MM-DDTHH:MM:SS.mmmZ`.
    Timestamp,
    "timestamp"
);
wire_string!(
    /// Page size: canonical decimal 1..=200.
    PageLimit,
    "page-limit"
);
wire_string!(
    /// Read window: canonical decimal 1..=262144 bytes.
    ReadLimit,
    "read-limit"
);

impl Id {
    /// Formats 16 random bytes (e.g. from `/dev/urandom`) as a UUIDv4.
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

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

impl Timestamp {
    /// Milliseconds since the Unix epoch; `None` outside 1970-01-01..=9999-12-31.
    pub fn from_unix_ms(ms: i64) -> Option<Self> {
        if ms < 0 {
            return None;
        }
        let (days, rem) = (ms / 86_400_000, ms % 86_400_000);
        let (year, month, day) = civil_from_days(days);
        if year > 9999 {
            return None;
        }
        Some(Self(format!(
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
            rem / 3_600_000,
            rem / 60_000 % 60,
            rem / 1000 % 60,
            rem % 1000
        )))
    }
    pub fn unix_ms(&self) -> i64 {
        let [y, mo, d, h, mi, s, ms] = schema::timestamp_parts(&self.0)
            .expect("validated timestamp")
            .map(i64::from);
        days_from_civil(y, mo, d) * 86_400_000 + h * 3_600_000 + mi * 60_000 + s * 1000 + ms
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitAlgorithm {
    Sha1,
    Sha256,
}

/// Git object id with its object format. The hex length always matches the algorithm.
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
    pub fn new(algorithm: GitAlgorithm, hex: impl Into<String>) -> Result<Self, WireError> {
        Self::try_from(RawGitId {
            algorithm,
            hex: hex.into(),
        })
    }
    /// Infers the object format from a lowercase hex id (40 = SHA-1, 64 = SHA-256).
    pub fn from_hex(hex: &str) -> Result<Self, WireError> {
        let algorithm = if hex.len() == 40 {
            GitAlgorithm::Sha1
        } else {
            GitAlgorithm::Sha256
        };
        Self::new(algorithm, hex)
    }
    pub fn algorithm(&self) -> GitAlgorithm {
        self.algorithm
    }
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
            Err(WireError("invalid_schema"))
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

pub const MAX_PATH_BYTES: usize = 4096;
const MAX_DISPLAY_CHARS: usize = 4096;

/// Lossless repository path: raw bytes travel as canonical base64; `display` is a lossy UTF-8
/// label for presentation only and is never authority.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawPath", into = "RawPath")]
pub struct WirePath {
    bytes: Vec<u8>,
    display: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPath {
    b64: String,
    display: String,
}
impl WirePath {
    /// Raw bytes (1..=4096) with a derived lossy display label.
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Result<Self, WireError> {
        let bytes = bytes.into();
        if bytes.is_empty() || bytes.len() > MAX_PATH_BYTES {
            return Err(WireError("invalid_schema"));
        }
        let display = String::from_utf8_lossy(&bytes)
            .chars()
            .take(MAX_DISPLAY_CHARS)
            .collect();
        Ok(Self { bytes, display })
    }
    #[cfg(unix)]
    pub fn from_os_str(path: &OsStr) -> Result<Self, WireError> {
        use std::os::unix::ffi::OsStrExt;
        Self::from_bytes(path.as_bytes())
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
    #[cfg(unix)]
    pub fn to_os_string(&self) -> OsString {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(self.bytes.clone())
    }
    pub fn display(&self) -> &str {
        &self.display
    }
    pub fn b64(&self) -> String {
        base64::encode(&self.bytes)
    }
}
impl TryFrom<RawPath> for WirePath {
    type Error = WireError;
    fn try_from(raw: RawPath) -> Result<Self, WireError> {
        let bytes = base64::decode(&raw.b64).ok_or(WireError("invalid_schema"))?;
        if bytes.is_empty()
            || bytes.len() > MAX_PATH_BYTES
            || raw.display.chars().count() > MAX_DISPLAY_CHARS
        {
            return Err(WireError("invalid_schema"));
        }
        Ok(Self {
            bytes,
            display: raw.display,
        })
    }
}
impl From<WirePath> for RawPath {
    fn from(path: WirePath) -> Self {
        Self {
            b64: base64::encode(&path.bytes),
            display: path.display,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub provider: String,
    pub id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    Prompt,
    Steer,
    FollowUp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityRole {
    User,
    Assistant,
    Thinking,
    Tool,
    System,
}

/// pi thinking level (`ThinkingLevel` in pi-agent-core 0.85.1). The worker clamps a requested
/// level to the selected model's capabilities.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ThinkingLevel {
    pub const ALL: [Self; 7] = [
        Self::Off,
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::Xhigh,
        Self::Max,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.as_str() == text)
    }
}

/// Kind of one pi session-tree entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TreeEntryKind {
    User,
    Assistant,
    ToolResult,
    CustomMessage,
    Compaction,
    BranchSummary,
    ModelChange,
    ThinkingChange,
    Label,
    SessionInfo,
    Custom,
    Other,
}

/// One entry of the pi session tree (labels are short previews, never full content).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeEntry {
    pub entry_id: String,
    pub kind: TreeEntryKind,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bookmark: Option<String>,
}

/// A semantic extension dialog. There is no automatic answer for any kind.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiPrompt {
    Confirm { title: String, message: String },
    Select { title: String, options: Vec<String> },
    Input { title: String, placeholder: String },
    Editor { title: String, prefill: String },
}

/// The developer's answer to a [`UiPrompt`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiAnswer {
    Confirm { confirmed: bool },
    Value { value: String },
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiCloseReason {
    Timeout,
    Aborted,
    Replaced,
    Answered,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionChangeReason {
    Start,
    Navigate,
    Fork,
    Import,
    Reload,
    Compact,
    Configure,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForkPosition {
    Before,
    At,
}

/// Longest prefix of `text` within `max_bytes`, cut at a UTF-8 boundary, and whether it was cut.
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

/// Longest prefix of `text` with at most `max_chars` code points (the schema `maxLength` unit).
pub fn truncate_chars(text: &str, max_chars: usize) -> &str {
    text.char_indices()
        .nth(max_chars)
        .map_or(text, |(end, _)| &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_round_trip_unix_milliseconds() {
        for (ms, text) in [
            (0, "1970-01-01T00:00:00.000Z"),
            (951_782_400_000, "2000-02-29T00:00:00.000Z"),
            (1_791_635_696_789, "2026-10-10T12:34:56.789Z"),
            (253_402_300_799_999, "9999-12-31T23:59:59.999Z"),
        ] {
            let t = Timestamp::from_unix_ms(ms).unwrap();
            assert_eq!(t.as_str(), text);
            assert_eq!(t.unix_ms(), ms);
            assert_eq!(Timestamp::parse(text).unwrap(), t);
        }
        assert_eq!(Timestamp::from_unix_ms(-1), None);
        assert_eq!(Timestamp::from_unix_ms(253_402_300_800_000), None);
        for bad in [
            "2026-02-29T00:00:00.000Z",
            "2026-10-10T24:00:00.000Z",
            "2026-10-10T12:34:60.000Z",
            "2026-10-10T12:34:56Z",
            "2026-10-10T12:34:56.789z",
            "2026-10-10t12:34:56.789Z",
            "2026-10-10T12:34:56.789+00:00",
            "1969-12-31T23:59:59.999Z",
            "2026-13-01T00:00:00.000Z",
            "2026-00-01T00:00:00.000Z",
            "2026-10-00T00:00:00.000Z",
        ] {
            assert!(Timestamp::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn paths_are_lossless_for_non_utf8_bytes() {
        let raw = b"src/\xff\xfe/lib\n.rs".to_vec();
        let path = WirePath::from_bytes(raw.clone()).unwrap();
        assert_eq!(path.display(), "src/\u{fffd}\u{fffd}/lib\n.rs");
        let json = serde_json::to_value(&path).unwrap();
        let back: WirePath = serde_json::from_value(json).unwrap();
        assert_eq!(back.as_bytes(), raw);
        assert_eq!(WirePath::from_os_str(&back.to_os_string()).unwrap(), path);
        assert!(WirePath::from_bytes(Vec::new()).is_err());
        assert!(WirePath::from_bytes(vec![b'a'; MAX_PATH_BYTES + 1]).is_err());
    }

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
