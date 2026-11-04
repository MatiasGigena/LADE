//! Request frames: a named request with its payload, request id and operation id.

use super::{Request, RequestName, to_value};
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawRequest", into = "RawRequest")]
pub struct RequestFrame {
    pub request_id: Id,
    pub operation_id: Option<Id>,
    pub request: Request,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRequest {
    request_id: Id,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation_id: Option<Id>,
    name: RequestName,
    payload: Value,
}
impl RequestFrame {
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
    pub fn canonical_digest(&self) -> [u8; 32] {
        let mut content = to_value(self);
        content
            .as_object_mut()
            .expect("request serializes as an object")
            .remove("request_id");
        Sha256::digest(serde_json::to_vec(&content).expect("JSON value serializes")).into()
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
impl From<RequestFrame> for RawRequest {
    fn from(frame: RequestFrame) -> Self {
        Self {
            request_id: frame.request_id,
            operation_id: frame.operation_id,
            name: frame.request.name(),
            payload: frame.request.payload_value(),
        }
    }
}
