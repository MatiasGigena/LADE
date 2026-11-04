//! The client frame envelope and its body kinds.

use super::{EventFrame, Reply, RequestFrame};
use crate::types::{Capability, Counter, Digest, Id};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientEnvelope {
    pub protocol: u32,
    pub bundle: String,
    pub body: ClientBody,
}
impl ClientEnvelope {
    #[must_use]
    pub fn new(body: ClientBody) -> Self {
        Self {
            protocol: crate::VERSION,
            bundle: crate::BUNDLE.to_owned(),
            body,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Both,
    ClientToService,
    ServiceToClient,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum ClientBody {
    #[serde(rename = "hello")]
    Hello,
    #[serde(rename = "auth")]
    Auth { capability: Capability },
    #[serde(rename = "welcome")]
    Welcome { service_epoch: Id, lease_id: Id },
    #[serde(rename = "request")]
    Request(RequestFrame),
    #[serde(rename = "reply")]
    Reply(Reply),
    #[serde(rename = "event")]
    Event(EventFrame),
    #[serde(rename = "chunk.start")]
    ChunkStart {
        transfer_id: Id,
        owner: Scope,
        execution_generation: Counter,
        total_bytes: Counter,
        sha256: Digest,
    },
    #[serde(rename = "chunk.data")]
    ChunkData {
        transfer_id: Id,
        index: Counter,
        hex: String,
    },
    #[serde(rename = "chunk.end")]
    ChunkEnd { transfer_id: Id },
    #[serde(rename = "chunk.cancel")]
    ChunkCancel { transfer_id: Id },
}
impl ClientBody {
    #[must_use]
    pub fn direction(&self) -> Direction {
        match self {
            Self::Hello
            | Self::ChunkStart { .. }
            | Self::ChunkData { .. }
            | Self::ChunkEnd { .. }
            | Self::ChunkCancel { .. } => Direction::Both,
            Self::Auth { .. } | Self::Request(_) => Direction::ClientToService,
            Self::Welcome { .. } | Self::Reply(_) | Self::Event(_) => Direction::ServiceToClient,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub workspace_id: Id,
    pub objective_id: Id,
    pub session_id: Id,
    pub association_revision: Counter,
}
