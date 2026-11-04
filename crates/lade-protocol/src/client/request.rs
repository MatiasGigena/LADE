//! Request frames: a named request with its payload, request id and operation id.

use super::{Request, RequestName};
use crate::{WireError, types::Id};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

impl Serialize for RequestName {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
impl<'de> Deserialize<'de> for RequestName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::parse(&name).ok_or_else(|| serde::de::Error::custom("invalid_schema"))
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(try_from = "RawRequest")]
pub struct RequestFrame {
    pub request_id: Id,
    pub operation_id: Option<Id>,
    pub request: Request,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRequest {
    request_id: Id,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation_id: Option<Id>,
    name: RequestName,
    payload: Value,
}
impl RequestFrame {
    /// A request frame; mutating requests carry an operation id and reads do not.
    ///
    /// # Errors
    ///
    /// `WireError::InvalidSchema` when `operation_id` is present for a read or missing for a
    /// mutating request.
    pub fn new(
        request_id: Id,
        operation_id: Option<Id>,
        request: Request,
    ) -> Result<Self, WireError> {
        if operation_id.is_some() != request.name().is_mutating() {
            return Err(WireError::InvalidSchema);
        }
        Ok(Self {
            request_id,
            operation_id,
            request,
        })
    }
    /// SHA-256 of the frame's JSON form without its `request_id`, so a retried operation hashes the
    /// same however its request id was chosen.
    ///
    /// # Errors
    ///
    /// `WireError::InvalidSchema` when the payload cannot be represented as JSON.
    pub fn canonical_digest(&self) -> Result<[u8; 32], WireError> {
        let mut content = serde_json::Map::new();
        if let Some(operation_id) = &self.operation_id {
            content.insert("operation_id".into(), operation_id.as_str().into());
        }
        content.insert("name".into(), self.request.name().as_str().into());
        content.insert("payload".into(), self.request.payload_value()?);
        Ok(Sha256::digest(Value::Object(content).to_string()).into())
    }
}
impl TryFrom<RawRequest> for RequestFrame {
    type Error = WireError;
    fn try_from(raw: RawRequest) -> Result<Self, WireError> {
        Self::new(
            raw.request_id,
            raw.operation_id,
            Request::from_parts(raw.name, raw.payload)?,
        )
    }
}
impl TryFrom<&RequestFrame> for RawRequest {
    type Error = WireError;
    fn try_from(frame: &RequestFrame) -> Result<Self, WireError> {
        Ok(Self {
            request_id: frame.request_id.clone(),
            operation_id: frame.operation_id.clone(),
            name: frame.request.name(),
            payload: frame.request.payload_value()?,
        })
    }
}
impl Serialize for RequestFrame {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        RawRequest::try_from(self)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{Empty, MessageSendCommand};
    use crate::types::DeliveryMode;

    fn digest_of_serialized_frame(frame: &RequestFrame) -> [u8; 32] {
        let mut json = serde_json::to_value(frame).unwrap();
        json.as_object_mut().unwrap().remove("request_id");
        Sha256::digest(serde_json::to_vec(&json).unwrap()).into()
    }

    #[test]
    fn canonical_digest_is_the_serialized_frame_without_its_request_id() {
        let send = RequestFrame::new(
            Id::from_random_bytes([1; 16]),
            Some(Id::from_random_bytes([2; 16])),
            Request::MessageSend(MessageSendCommand {
                objective_id: Id::from_random_bytes([3; 16]),
                text: "go".into(),
                mode: DeliveryMode::Steer,
            }),
        )
        .unwrap();
        let read = RequestFrame::new(
            Id::from_random_bytes([4; 16]),
            None,
            Request::Snapshot(Empty {}),
        )
        .unwrap();
        for frame in [send, read] {
            assert_eq!(
                frame.canonical_digest().unwrap(),
                digest_of_serialized_frame(&frame)
            );
        }
    }
}
