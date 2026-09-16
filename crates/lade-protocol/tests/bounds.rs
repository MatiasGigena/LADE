use lade_protocol::{
    Decoder, Framing, MAX_CHUNK, MAX_TRANSFER, Reassembler, RequestHistory, WireError, decode,
    dto::{Body, OperationState, Response},
    encode,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn scope() -> Value {
    json!({"workspace_id":id(1),"objective_id":id(2),"session_id":id(3),"association_revision":"1"})
}
fn id(n: usize) -> String {
    format!("00000000-0000-4000-8000-{n:012x}")
}
fn message(body: Value) -> lade_protocol::dto::Envelope {
    decode(
        &serde_json::to_vec(&json!({"protocol":1,"bundle":lade_protocol::BUNDLE,"body":body}))
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn logical_content_larger_than_a_frame_is_preserved_at_reassembly_limit() {
    let bytes = vec![b'a'; MAX_TRANSFER];
    let digest = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let mut chunks = Reassembler::new(
        serde_json::from_value(scope()).unwrap(),
        "1".to_string().try_into().unwrap(),
    );
    chunks.accept(&message(json!({"kind":"chunk.start","transfer_id":id(4),"owner":scope(),"execution_generation":"1","total_bytes":MAX_TRANSFER.to_string(),"sha256":digest})).body).unwrap();
    for index in 0..MAX_TRANSFER / MAX_CHUNK {
        let data = message(
            json!({"kind":"chunk.data","transfer_id":id(4),"index":index.to_string(),"hex":"61".repeat(MAX_CHUNK)}),
        );
        let mut decoder = Decoder::new(Framing::Worker);
        decoder
            .push(&encode(&data, Framing::Worker).unwrap(), |envelope| {
                assert!(chunks.accept(&envelope.body)?.is_none());
                Ok(())
            })
            .unwrap();
        decoder.finish().unwrap();
    }
    assert_eq!(
        chunks
            .accept(&message(json!({"kind":"chunk.end","transfer_id":id(4)})).body)
            .unwrap()
            .unwrap(),
        bytes
    );
}

#[test]
fn cancellation_releases_capacity_and_disconnect_revokes_the_reassembler() {
    let mut chunks = Reassembler::new(
        serde_json::from_value(scope()).unwrap(),
        "1".to_string().try_into().unwrap(),
    );
    for n in 0..20 {
        let start = message(
            json!({"kind":"chunk.start","transfer_id":id(4+n),"owner":scope(),"execution_generation":"1","total_bytes":"1","sha256":"0".repeat(64)}),
        );
        chunks.accept(&start.body).unwrap();
        chunks
            .accept(&message(json!({"kind":"chunk.cancel","transfer_id":id(4+n)})).body)
            .unwrap();
    }
    chunks.close();
    assert_eq!(
        chunks
            .accept(&message(json!({"kind":"chunk.end","transfer_id":id(4)})).body)
            .unwrap_err()
            .0,
        "closed"
    );
}

#[test]
fn correlation_overflow_never_evicts_the_original_outcome() {
    let mut history = RequestHistory::default();
    let command = |n: usize| {
        message(
            json!({"kind":"command","request_id":id(1000+n),"operation_id":id(2000+n),"expected_service_epoch":id(6),"scope":scope(),"command":{"name":"operation.get","payload":{"operation_id":id(5)}}}),
        )
    };
    let original = Response::Operation {
        state: OperationState::Uncertain,
    };
    for n in 0..256 {
        history.observe(&command(n).body, original.clone()).unwrap();
    }
    assert_eq!(
        history
            .observe(&command(256).body, original.clone())
            .unwrap_err()
            .0,
        "limit_reached"
    );
    assert_eq!(
        history
            .observe(
                &command(0).body,
                Response::Operation {
                    state: OperationState::Completed
                }
            )
            .unwrap(),
        original
    );
}

#[test]
fn downstream_backpressure_stops_decoding_without_delivering_later_frames() {
    let hello = message(json!({"kind":"hello"}));
    for framing in [Framing::Client, Framing::Worker] {
        let mut bytes = encode(&hello, framing).unwrap();
        bytes.extend(encode(&hello, framing).unwrap());
        let mut decoder = Decoder::new(framing);
        let mut calls = 0;
        assert_eq!(
            decoder
                .push(&bytes, |_| {
                    calls += 1;
                    Err(WireError("limit_reached"))
                })
                .unwrap_err()
                .0,
            "limit_reached"
        );
        assert_eq!(calls, 1);
        assert_eq!(decoder.push(&bytes, |_| Ok(())).unwrap_err().0, "closed");
    }
}

#[test]
fn oversized_outbound_content_is_rejected() {
    let mut frame =
        message(json!({"kind":"chunk.data","transfer_id":id(4),"index":"0","hex":"61"}));
    if let Body::ChunkData { hex, .. } = &mut frame.body {
        *hex = "a".repeat(2 * lade_protocol::MAX_FRAME);
    }
    assert_eq!(
        encode(&frame, Framing::Client).unwrap_err().0,
        "invalid_schema"
    );
}
