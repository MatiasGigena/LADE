//! LADE's versioned wire contracts: the client protocol between the desktop client and the
//! service, and the worker protocol between the service and the pi worker.
//!
//! It holds the DTOs ([`client`], [`worker`], [`types`]), validation against the closed JSON
//! schemas with bounded parsing, frame encoding and decoding, chunked transfers, and the canonical
//! request digest that recognises retried operations. Every refusal below the request layer is a
//! [`WireError`] with a stable wire code.

pub mod base64;
mod chunks;
pub mod client;
mod framing;
mod io;
mod json;
mod schema;
pub mod types;
pub mod worker;

pub use chunks::Reassembler;
pub use client::{ClientBody, ClientEnvelope};
pub use framing::{Decoder, Deframer, Framing, encode};
pub use io::{FrameIoError, FrameReader, FrameWriter};
pub use json::{bounded_json, json_len};
pub use schema::Contract;
pub use worker::{WorkerBody, WorkerEnvelope};

use serde::{Serialize, de::DeserializeOwned};
use types::Id;

pub const VERSION: u32 = 1;
pub const BUNDLE: &str = "0.2.0";
pub const MAX_FRAME: usize = 1024 * 1024;
pub const MAX_STRING: usize = 256 * 1024;
pub const MAX_NODES: usize = 16_384;
pub const MAX_DEPTH: usize = 32;
pub const MAX_ENTRIES: usize = 1024;
pub const MAX_CHUNK: usize = 64 * 1024;
pub const MAX_TRANSFER: usize = 8 * 1024 * 1024;
pub const MAX_TRANSFERS: usize = 2;

macro_rules! wire_errors {
    ($($variant:ident = $code:literal,)*) => {
        /// Why a frame or transfer was refused below the request layer; `as_str` is the stable wire code.
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub enum WireError { $($variant),* }
        impl WireError {
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $code),* }
            }
        }
    };
}
wire_errors! {
    Closed = "closed",
    TruncatedFrame = "truncated_frame",
    FrameSize = "frame_size",
    InvalidUtf8 = "invalid_utf8",
    InvalidJson = "invalid_json",
    DuplicateKey = "duplicate_key",
    InvalidSchema = "invalid_schema",
    VersionMismatch = "version_mismatch",
    LimitReached = "limit_reached",
    StaleExecution = "stale_execution",
    ChunkOrder = "chunk_order",
    ChunkLength = "chunk_length",
    ChunkChecksum = "chunk_checksum",
    DuplicateConflict = "duplicate_conflict",
}
impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
impl std::error::Error for WireError {}

pub trait Message: Serialize + DeserializeOwned + Sized {
    const CONTRACT: Contract;
    fn identity(&self) -> (u32, &str);
    /// Extra checks a frame must pass before it is encoded; none by default.
    ///
    /// # Errors
    ///
    /// Implementations return the `WireError` for the first violated rule.
    fn precheck(&self) -> Result<(), WireError> {
        Ok(())
    }
    /// Decodes one frame body under the message's contract.
    ///
    /// # Errors
    ///
    /// - Any refusal from `bounded_json`.
    /// - `WireError::VersionMismatch` for another protocol version or bundle.
    /// - `WireError::InvalidSchema` when the frame does not match the contract.
    fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let value = bounded_json(bytes)?;
        check_version(&value)?;
        schema::validate(&value, Self::CONTRACT)?;
        serde_json::from_value(value).map_err(|_| WireError::InvalidSchema)
    }
}

fn check_version(value: &serde_json::Value) -> Result<(), WireError> {
    if value.get("protocol") != Some(&serde_json::json!(VERSION))
        || value.get("bundle").and_then(|v| v.as_str()) != Some(BUNDLE)
    {
        return Err(WireError::VersionMismatch);
    }
    Ok(())
}

impl Message for ClientEnvelope {
    const CONTRACT: Contract = Contract::Client;
    fn identity(&self) -> (u32, &str) {
        (self.protocol, &self.bundle)
    }
    fn precheck(&self) -> Result<(), WireError> {
        match &self.body {
            ClientBody::ChunkData { hex, .. } if !schema::format(hex, "chunk-hex") => {
                Err(WireError::InvalidSchema)
            }
            _ => Ok(()),
        }
    }
}
impl Message for WorkerEnvelope {
    const CONTRACT: Contract = Contract::Worker;
    fn identity(&self) -> (u32, &str) {
        (self.protocol, &self.bundle)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameRejection {
    pub error: WireError,
    pub request_id: Option<Id>,
}

/// Decodes a client frame, keeping the request id of a well-formed request envelope whose payload
/// was refused so the refusal can be answered.
///
/// # Errors
///
/// `FrameRejection` with the refusal, and the request id when it could be read.
pub fn decode_client_frame(bytes: &[u8]) -> Result<ClientEnvelope, FrameRejection> {
    let fatal = |error| FrameRejection {
        error,
        request_id: None,
    };
    let value = bounded_json(bytes).map_err(fatal)?;
    check_version(&value).map_err(fatal)?;
    let error = match schema::validate(&value, Contract::Client) {
        Ok(()) => match serde_json::from_value(value.clone()) {
            Ok(envelope) => return Ok(envelope),
            Err(_) => WireError::InvalidSchema,
        },
        Err(error) => error,
    };
    let envelope_shape = value.as_object().is_some_and(|o| o.len() == 3);
    let body = value.get("body");
    let request_id = body
        .filter(|b| envelope_shape && b.get("kind").and_then(|k| k.as_str()) == Some("request"))
        .and_then(|b| b.get("request_id"))
        .and_then(|v| v.as_str())
        .and_then(|id| Id::parse(id).ok());
    Err(FrameRejection { error, request_id })
}
