//! Limits and failure modes at the protocol's edges: chunked transfers, request correlation,
//! frame size bounds and the blocking transport.

mod chunks;
mod frames;
mod requests;
mod transport;

use lade_protocol::{ClientEnvelope, Message, types::Id};
use serde_json::{Value, json};

fn id(n: usize) -> String {
    format!("00000000-0000-4000-8000-{n:012x}")
}
fn typed_id(n: usize) -> Id {
    Id::parse(id(n)).unwrap()
}
fn scope() -> Value {
    json!({"workspace_id":id(1),"objective_id":id(2),"session_id":id(3),"association_revision":"1"})
}
fn wire(body: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"protocol":1,"bundle":lade_protocol::BUNDLE,"body":body})).unwrap()
}
fn message(body: Value) -> ClientEnvelope {
    ClientEnvelope::decode(&wire(body)).unwrap()
}
