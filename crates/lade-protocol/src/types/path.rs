//! Byte-exact file paths with a lossy display form.

use crate::{WireError, base64};
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};

pub const MAX_PATH_BYTES: usize = 4096;
const MAX_DISPLAY_CHARS: usize = 4096;

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
    /// A path from its raw bytes, with a lossy UTF-8 display form.
    ///
    /// # Errors
    ///
    /// `WireError::InvalidSchema` for an empty path or one longer than `MAX_PATH_BYTES`.
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Result<Self, WireError> {
        let bytes = bytes.into();
        if bytes.is_empty() || bytes.len() > MAX_PATH_BYTES {
            return Err(WireError::InvalidSchema);
        }
        let display = String::from_utf8_lossy(&bytes)
            .chars()
            .take(MAX_DISPLAY_CHARS)
            .collect();
        Ok(Self { bytes, display })
    }
    /// # Errors
    ///
    /// `WireError::InvalidSchema` for an empty path or one longer than `MAX_PATH_BYTES`.
    #[cfg(unix)]
    pub fn from_os_str(path: &OsStr) -> Result<Self, WireError> {
        use std::os::unix::ffi::OsStrExt;
        Self::from_bytes(path.as_bytes())
    }
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
    #[cfg(unix)]
    #[must_use]
    pub fn to_os_string(&self) -> OsString {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(self.bytes.clone())
    }
    #[must_use]
    pub fn display(&self) -> &str {
        &self.display
    }
    #[must_use]
    pub fn b64(&self) -> String {
        base64::encode(&self.bytes)
    }
}
impl TryFrom<RawPath> for WirePath {
    type Error = WireError;
    fn try_from(raw: RawPath) -> Result<Self, WireError> {
        let bytes = base64::decode(&raw.b64).ok_or(WireError::InvalidSchema)?;
        if bytes.is_empty()
            || bytes.len() > MAX_PATH_BYTES
            || raw.display.chars().count() > MAX_DISPLAY_CHARS
        {
            return Err(WireError::InvalidSchema);
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
