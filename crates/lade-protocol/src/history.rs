use crate::{
    WireError,
    client::{ReplyResult, RequestFrame},
    types::Id,
};
use std::collections::BTreeMap;

/// Bounded in-memory correlation only, NOT durable acceptance or execution authority.
/// The admitted transport must check caller/version/epoch before consulting history. The
/// durable service journal applies the same rules using `RequestFrame::canonical_digest`.
#[derive(Default)]
pub struct RequestHistory {
    requests: BTreeMap<Id, (Id, [u8; 32])>,
    operations: BTreeMap<Id, ([u8; 32], ReplyResult)>,
}
impl RequestHistory {
    pub const CAPACITY: usize = 256;

    /// Records a mutating request's outcome, or returns the original outcome of a matching
    /// duplicate. A changed payload under one operation, or one request id reused for different
    /// content, rejects `duplicate_conflict`; a full ledger fails `limit_reached`, never evicts.
    pub fn observe(
        &mut self,
        request: &RequestFrame,
        outcome: ReplyResult,
    ) -> Result<ReplyResult, WireError> {
        let Some(operation_id) = &request.operation_id else {
            return Err(WireError("invalid_schema"));
        };
        let request_id = &request.request_id;
        let digest = request.canonical_digest();
        if let Some((op, previous)) = self.requests.get(request_id)
            && (op != operation_id || previous != &digest)
        {
            return Err(WireError("duplicate_conflict"));
        }
        if let Some((previous, original)) = self.operations.get(operation_id) {
            if previous != &digest {
                return Err(WireError("duplicate_conflict"));
            }
            if !self.requests.contains_key(request_id) && self.requests.len() == Self::CAPACITY {
                return Err(WireError("limit_reached"));
            }
            let original = original.clone();
            self.requests
                .insert(request_id.clone(), (operation_id.clone(), digest));
            return Ok(original);
        }
        if self.operations.len() == Self::CAPACITY || self.requests.len() == Self::CAPACITY {
            return Err(WireError("limit_reached"));
        }
        self.requests
            .insert(request_id.clone(), (operation_id.clone(), digest));
        self.operations
            .insert(operation_id.clone(), (digest, outcome.clone()));
        Ok(outcome)
    }
}
