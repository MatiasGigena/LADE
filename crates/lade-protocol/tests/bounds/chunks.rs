use super::{id, message, scope};
use lade_protocol::{
    ClientEnvelope, Decoder, Framing, MAX_CHUNK, MAX_TRANSFER, Reassembler, encode,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn reassembler() -> Reassembler {
    Reassembler::new(
        serde_json::from_value(scope()).unwrap(),
        "1".to_string().try_into().unwrap(),
    )
}
fn start(transfer: usize, total_bytes: usize, sha256: &str) -> Value {
    json!({"kind":"chunk.start","transfer_id":id(transfer),"owner":scope(),
        "execution_generation":"1","total_bytes":total_bytes.to_string(),"sha256":sha256})
}

#[test]
fn logical_content_larger_than_a_frame_is_preserved_at_reassembly_limit() {
    let bytes = vec![b'a'; MAX_TRANSFER];
    let digest = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let mut chunks = reassembler();
    chunks
        .accept(&message(start(4, MAX_TRANSFER, &digest)).body)
        .unwrap();
    for index in 0..MAX_TRANSFER / MAX_CHUNK {
        let data = message(
            json!({"kind":"chunk.data","transfer_id":id(4),"index":index.to_string(),"hex":"61".repeat(MAX_CHUNK)}),
        );
        let mut decoder = Decoder::<ClientEnvelope>::new(Framing::Worker);
        decoder
            .push(&encode(&data, Framing::Worker).unwrap(), |envelope| {
                assert!(chunks.accept(&envelope.body)?.is_none());
                Ok(())
            })
            .unwrap();
        decoder.finish().unwrap();
    }
    let end = message(json!({"kind":"chunk.end","transfer_id":id(4)}));
    assert_eq!(chunks.accept(&end.body).unwrap().unwrap(), bytes);
}

#[test]
fn cancellation_releases_capacity_and_disconnect_revokes_the_reassembler() {
    let mut chunks = reassembler();
    for n in 0..20 {
        chunks
            .accept(&message(start(4 + n, 1, &"0".repeat(64))).body)
            .unwrap();
        chunks
            .accept(&message(json!({"kind":"chunk.cancel","transfer_id":id(4+n)})).body)
            .unwrap();
    }
    chunks.close();
    let end = message(json!({"kind":"chunk.end","transfer_id":id(4)}));
    assert_eq!(chunks.accept(&end.body).unwrap_err().as_str(), "closed");
}
