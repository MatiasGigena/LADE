use crate::{
    MAX_CHUNK, MAX_TRANSFER, MAX_TRANSFERS, WireError,
    client::{ClientBody as Body, Scope},
    types::{Counter, Digest as Sha256Digest, Id},
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

struct Transfer {
    total: usize,
    digest: Sha256Digest,
    next: u64,
    bytes: Vec<u8>,
}

pub struct Reassembler {
    owner: Scope,
    generation: Counter,
    transfers: BTreeMap<Id, Transfer>,
    failed: bool,
}
impl Reassembler {
    #[must_use]
    pub fn new(owner: Scope, generation: Counter) -> Self {
        Self {
            owner,
            generation,
            transfers: BTreeMap::new(),
            failed: false,
        }
    }
    /// Applies one chunk frame and returns the reassembled bytes when a transfer completes. Any
    /// refusal closes the reassembler.
    ///
    /// # Errors
    ///
    /// - `WireError::Closed` after an earlier refusal.
    /// - `WireError::StaleExecution` for a transfer of another owner or execution generation.
    /// - `WireError::ChunkOrder` for a duplicate or unknown transfer, or an out-of-order chunk.
    /// - `WireError::LimitReached` for an oversized or excess transfer, or a malformed or oversized
    ///   chunk.
    /// - `WireError::ChunkLength` or `WireError::ChunkChecksum` when a finished transfer's size or
    ///   SHA-256 differs from its start.
    /// - `WireError::InvalidSchema` for a body that is not a chunk frame.
    pub fn accept(&mut self, body: &Body) -> Result<Option<Vec<u8>>, WireError> {
        if self.failed {
            return Err(WireError::Closed);
        }
        let result = self.apply(body);
        if result.is_err() {
            self.close();
        }
        result
    }
    pub fn close(&mut self) {
        self.transfers.clear();
        self.failed = true;
    }
    fn apply(&mut self, body: &Body) -> Result<Option<Vec<u8>>, WireError> {
        match body {
            Body::ChunkStart {
                transfer_id,
                owner,
                execution_generation,
                total_bytes,
                sha256,
            } => {
                if owner != &self.owner || execution_generation != &self.generation {
                    return Err(WireError::StaleExecution);
                }
                if self.transfers.contains_key(transfer_id) {
                    return Err(WireError::ChunkOrder);
                }
                let total =
                    usize::try_from(total_bytes.value()).map_err(|_| WireError::LimitReached)?;
                if total == 0 || total > MAX_TRANSFER || self.transfers.len() == MAX_TRANSFERS {
                    return Err(WireError::LimitReached);
                }
                self.transfers.insert(
                    transfer_id.clone(),
                    Transfer {
                        total,
                        digest: sha256.clone(),
                        next: 0,
                        bytes: Vec::new(),
                    },
                );
            }
            Body::ChunkData {
                transfer_id,
                index,
                hex,
            } => {
                let t = self
                    .transfers
                    .get_mut(transfer_id)
                    .ok_or(WireError::ChunkOrder)?;
                if index.value() != t.next {
                    return Err(WireError::ChunkOrder);
                }
                if !crate::schema::format(hex, "chunk-hex")
                    || hex.len() / 2 > MAX_CHUNK
                    || t.bytes.len() + hex.len() / 2 > t.total
                {
                    return Err(WireError::LimitReached);
                }
                if t.bytes.capacity() < t.bytes.len() + hex.len() / 2 {
                    let capacity = (t.bytes.capacity() * 2)
                        .max(t.bytes.len() + hex.len() / 2)
                        .min(t.total);
                    t.bytes.reserve_exact(capacity - t.bytes.len());
                }
                for pair in hex.as_bytes().as_chunks::<2>().0 {
                    let digit = |v: u8| if v <= b'9' { v - b'0' } else { v - b'a' + 10 };
                    t.bytes.push(digit(pair[0]) * 16 + digit(pair[1]));
                }
                t.next += 1;
            }
            Body::ChunkEnd { transfer_id } => {
                let t = self
                    .transfers
                    .remove(transfer_id)
                    .ok_or(WireError::ChunkOrder)?;
                if t.bytes.len() != t.total {
                    return Err(WireError::ChunkLength);
                }
                if Sha256Digest::from_bytes(Sha256::digest(&t.bytes).into()) != t.digest {
                    return Err(WireError::ChunkChecksum);
                }
                return Ok(Some(t.bytes));
            }
            Body::ChunkCancel { transfer_id } => {
                self.transfers
                    .remove(transfer_id)
                    .ok_or(WireError::ChunkOrder)?;
            }
            _ => return Err(WireError::InvalidSchema),
        }
        Ok(None)
    }
}
