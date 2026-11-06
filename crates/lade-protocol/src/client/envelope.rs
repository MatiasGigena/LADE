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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::fixtures::{counter, digest, id, objective_detail};
    use crate::client::{Empty, Event, ReplyData, Request};
    use crate::{BUNDLE, Framing, Message, VERSION, WireError, encode};
    use serde_json::json;

    fn scope() -> Scope {
        Scope {
            workspace_id: id(1),
            objective_id: id(2),
            session_id: id(3),
            association_revision: counter(1),
        }
    }

    fn bodies() -> Vec<(ClientBody, &'static str, Direction)> {
        let request = RequestFrame::new(id(4), None, Request::Snapshot(Empty {})).expect("read");
        let reply = Reply::ok(id(4), ReplyData::DraftDelete(Empty {}));
        let event = EventFrame {
            service_epoch: id(5),
            sequence: counter(1),
            event: Event::ObjectiveChanged(objective_detail().summary()),
        };
        let capability = Capability::parse(digest(7).as_str()).expect("capability");
        vec![
            (ClientBody::Hello, "hello", Direction::Both),
            (
                ClientBody::Auth { capability },
                "auth",
                Direction::ClientToService,
            ),
            (
                ClientBody::Welcome {
                    service_epoch: id(5),
                    lease_id: id(6),
                },
                "welcome",
                Direction::ServiceToClient,
            ),
            (
                ClientBody::Request(request),
                "request",
                Direction::ClientToService,
            ),
            (
                ClientBody::Reply(reply),
                "reply",
                Direction::ServiceToClient,
            ),
            (
                ClientBody::Event(event),
                "event",
                Direction::ServiceToClient,
            ),
            (
                ClientBody::ChunkStart {
                    transfer_id: id(8),
                    owner: scope(),
                    execution_generation: counter(1),
                    total_bytes: counter(2),
                    sha256: digest(9),
                },
                "chunk.start",
                Direction::Both,
            ),
            (
                ClientBody::ChunkData {
                    transfer_id: id(8),
                    index: counter(0),
                    hex: "6162".into(),
                },
                "chunk.data",
                Direction::Both,
            ),
            (
                ClientBody::ChunkEnd { transfer_id: id(8) },
                "chunk.end",
                Direction::Both,
            ),
            (
                ClientBody::ChunkCancel { transfer_id: id(8) },
                "chunk.cancel",
                Direction::Both,
            ),
        ]
    }

    #[test]
    fn every_body_kind_frames_under_the_current_version_and_round_trips() {
        for (body, kind, direction) in bodies() {
            assert_eq!(body.direction(), direction, "{kind}");
            let envelope = ClientEnvelope::new(body);
            assert_eq!(
                (envelope.protocol, envelope.bundle.as_str()),
                (VERSION, BUNDLE)
            );
            let frame = encode(&envelope, Framing::Worker).expect("encode");
            let json: serde_json::Value =
                serde_json::from_slice(frame.strip_suffix(b"\n").expect("line")).expect("json");
            assert_eq!(json["body"]["kind"], json!(kind));
            assert_eq!(
                ClientEnvelope::decode(frame.strip_suffix(b"\n").expect("line")),
                Ok(envelope),
                "{kind}"
            );
        }
    }

    #[test]
    fn envelopes_and_scopes_refuse_unknown_fields_and_kinds() {
        let mut json = serde_json::to_value(ClientEnvelope::new(ClientBody::Hello)).expect("json");
        json["extra"] = json!(true);
        assert!(serde_json::from_value::<ClientEnvelope>(json).is_err());
        let mut owner = serde_json::to_value(scope()).expect("json");
        owner["generation"] = json!("1");
        assert!(serde_json::from_value::<Scope>(owner).is_err());
        let unknown = json!({"protocol": VERSION, "bundle": BUNDLE, "body": {"kind": "goodbye"}});
        assert_eq!(
            ClientEnvelope::decode(&serde_json::to_vec(&unknown).expect("bytes")),
            Err(WireError::InvalidSchema)
        );
    }
}
