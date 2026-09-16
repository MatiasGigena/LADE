use crate::{
    WireError,
    dto::{Body, Id, Response},
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Bounded fixture correlation only, NOT durable acceptance or execution authority.
/// The admitted transport must check caller/version/epoch before consulting history.
#[derive(Default)]
pub struct RequestHistory {
    requests: BTreeMap<Id, (Id, Vec<u8>)>,
    operations: BTreeMap<Id, (Vec<u8>, Response)>,
}
impl RequestHistory {
    pub fn observe(
        &mut self,
        command: &Body,
        fixture_outcome: Response,
    ) -> Result<Response, WireError> {
        let Body::Command {
            request_id,
            operation_id,
            ..
        } = command
        else {
            return Err(WireError("invalid_schema"));
        };
        let mut content = serde_json::to_value(command).map_err(|_| WireError("invalid_schema"))?;
        content
            .as_object_mut()
            .ok_or(WireError("invalid_schema"))?
            .remove("request_id");
        // serde_json's default BTreeMap sorts object keys recursively, not arrays.
        let digest =
            Sha256::digest(serde_json::to_vec(&content).map_err(|_| WireError("invalid_schema"))?)
                .to_vec();
        if let Some((op, previous)) = self.requests.get(request_id)
            && (op != operation_id || previous != &digest)
        {
            return Err(WireError("duplicate_conflict"));
        }
        if let Some((previous, outcome)) = self.operations.get(operation_id) {
            if previous != &digest {
                return Err(WireError("duplicate_conflict"));
            }
            if !self.requests.contains_key(request_id) && self.requests.len() == 256 {
                return Err(WireError("limit_reached"));
            }
            self.requests
                .insert(request_id.clone(), (operation_id.clone(), digest));
            return Ok(outcome.clone());
        }
        if self.operations.len() == 256 || self.requests.len() == 256 {
            return Err(WireError("limit_reached"));
        }
        self.requests
            .insert(request_id.clone(), (operation_id.clone(), digest.clone()));
        self.operations
            .insert(operation_id.clone(), (digest, fixture_outcome.clone()));
        Ok(fixture_outcome)
    }
}
