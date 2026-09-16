//! Effect-free wire primitives. Parsing an envelope never authenticates a caller.
mod chunks;
pub mod dto;
mod framing;
mod history;
mod json;
mod schema;

pub use chunks::Reassembler;
pub use framing::{Decoder, Framing, encode};
pub use history::RequestHistory;
pub use json::bounded_json;

pub const VERSION: u32 = 1;
pub const BUNDLE: &str = "0.1.0-imp001";
pub const MAX_FRAME: usize = 1024 * 1024;
pub const MAX_STRING: usize = 256 * 1024;
pub const MAX_NODES: usize = 16_384;
pub const MAX_DEPTH: usize = 32;
pub const MAX_ENTRIES: usize = 1024;
pub const MAX_CHUNK: usize = 64 * 1024;
pub const MAX_TRANSFER: usize = 8 * 1024 * 1024;
pub const MAX_TRANSFERS: usize = 2;

/// Stable, body-free diagnostics. Never include peer bytes in Display or logs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WireError(pub &'static str);
impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for WireError {}

pub fn decode(bytes: &[u8]) -> Result<dto::Envelope, WireError> {
    let value = bounded_json(bytes)?;
    if value.get("protocol") != Some(&serde_json::json!(VERSION))
        || value.get("bundle").and_then(|v| v.as_str()) != Some(BUNDLE)
    {
        return Err(WireError("version_mismatch"));
    }
    schema::validate(&value)?;
    serde_json::from_value(value).map_err(|_| WireError("invalid_schema"))
}
