use lade_protocol::{
    ClientEnvelope, Decoder, Framing, Message, Reassembler, RequestHistory, WireError,
    WorkerEnvelope, bounded_json,
    client::{ClientBody, ErrorCode, Rejection, ReplyResult, SafeAction, Scope},
    encode,
    types::Counter,
};
use serde_json::{Value, json};
use std::fmt::Debug;

pub fn corpus() -> Vec<Value> {
    serde_json::from_str(include_str!("../../../../contracts/v1/fixtures.json")).unwrap()
}
fn unhex(s: &str) -> Vec<u8> {
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}
pub fn source(s: &Value) -> Vec<u8> {
    if let Some(v) = s.get("value") {
        return serde_json::to_vec(v).unwrap();
    }
    if let Some(v) = s.get("text") {
        return v.as_str().unwrap().as_bytes().to_vec();
    }
    if let Some(v) = s.get("hex") {
        return unhex(v.as_str().unwrap());
    }
    if let Some(v) = s.get("depth") {
        let n = v.as_u64().unwrap() as usize;
        return format!("{}0{}", "[".repeat(n), "]".repeat(n)).into_bytes();
    }
    if let Some(v) = s.get("array") {
        return serde_json::to_vec(&vec![0; v.as_u64().unwrap() as usize]).unwrap();
    }
    if let Some(v) = s.get("string") {
        return serde_json::to_vec(&"a".repeat(v.as_u64().unwrap() as usize)).unwrap();
    }
    if s.get("nodes").is_some() {
        return serde_json::to_vec(&vec![vec![0; 1024]; 16]).unwrap();
    }
    if let Some(v) = s.get("padding") {
        let mut bytes = b"{}".to_vec();
        bytes.resize(v.as_u64().unwrap() as usize, b' ');
        return bytes;
    }
    panic!("unknown fixture recipe")
}
fn framed(bytes: &[u8], framing: Framing) -> Vec<u8> {
    match framing {
        Framing::Client => {
            let mut v = (bytes.len() as u32).to_be_bytes().to_vec();
            v.extend(bytes);
            v
        }
        Framing::Worker => {
            let mut v = bytes.to_vec();
            v.push(b'\n');
            v
        }
    }
}

pub fn valid_frame<M: Message>() -> Vec<u8> {
    let body = match M::CONTRACT {
        lade_protocol::Contract::Client => json!({"kind":"hello"}),
        lade_protocol::Contract::Worker => json!({"kind":"session.abort"}),
    };
    serde_json::to_vec(&json!({"protocol":1,"bundle":lade_protocol::BUNDLE,"body":body})).unwrap()
}

fn stream<M: Message + PartialEq + Debug>(
    bytes: &[u8],
    framing: Framing,
    stride: usize,
    expected_count: usize,
) -> Result<(), WireError> {
    let mut decoder = Decoder::<M>::new(framing);
    let mut count = 0;
    for part in bytes.chunks(stride) {
        if let Err(error) = decoder.push(part, |message| {
            count += 1;
            let encoded = encode(&message, framing)?;
            let mut other = Decoder::<M>::new(framing);
            other.push(&encoded, |decoded| {
                assert_eq!(message, decoded);
                Ok(())
            })?;
            other.finish()
        }) {
            assert_eq!(count, 0, "invalid input must not emit a command");
            let valid = framed(&valid_frame::<M>(), framing);
            assert_eq!(
                decoder.push(&valid, |_| {
                    count += 1;
                    Ok(())
                }),
                Err(WireError::Closed)
            );
            assert_eq!(decoder.finish(), Err(WireError::Closed));
            assert_eq!(
                count, 0,
                "a failed connection must not deliver later valid messages"
            );
            return Err(error);
        }
    }
    decoder.finish()?;
    assert_eq!(count, expected_count);
    Ok(())
}

fn frame_case<M: Message + PartialEq + Debug>(bytes: &[u8]) -> Result<(), WireError> {
    let mut outcome = M::decode(bytes).map(|message| {
        assert_eq!(
            serde_json::to_value(message).unwrap(),
            bounded_json(bytes).unwrap()
        );
    });
    for framing in [Framing::Client, Framing::Worker] {
        let mut wire = framed(bytes, framing);
        wire.extend(framed(bytes, framing));
        wire.extend(framed(&valid_frame::<M>(), framing));
        for stride in [1, 2, 3, 7, 65537, wire.len()] {
            let result = stream::<M>(&wire, framing, stride, 3);
            assert_eq!(outcome, result);
            outcome = result;
        }
    }
    outcome
}

fn transport_case<M: Message + PartialEq + Debug>(
    bytes: &[u8],
    framing: Framing,
) -> Result<(), WireError> {
    let mut outcome = None;
    for stride in [1, 3, 65537, bytes.len()] {
        let result = stream::<M>(bytes, framing, stride, 0);
        if let Some(previous) = outcome {
            assert_eq!(previous, result);
        }
        outcome = Some(result);
    }
    outcome.unwrap()
}

fn rejected(code: ErrorCode) -> ReplyResult {
    ReplyResult::Rejected {
        name: None,
        rejection: Rejection::new(code, SafeAction::QueryOperation, "fixture"),
    }
}

pub fn run(case: &Value) -> Result<(), WireError> {
    let s = &case["source"];
    let worker = case["contract"] == "worker";
    match case["mode"].as_str().unwrap() {
        "json" => {
            bounded_json(&source(s))?;
        }
        "frame" if worker => frame_case::<WorkerEnvelope>(&source(s))?,
        "frame" => frame_case::<ClientEnvelope>(&source(s))?,
        "transport" => {
            let framing = if case["framing"] == "client" {
                Framing::Client
            } else {
                Framing::Worker
            };
            if worker {
                transport_case::<WorkerEnvelope>(&source(s), framing)?
            } else {
                transport_case::<ClientEnvelope>(&source(s), framing)?
            }
        }
        "chunks" => {
            let owner: Scope = serde_json::from_value(case["owner"].clone()).unwrap();
            let generation: Counter = serde_json::from_value(case["generation"].clone()).unwrap();
            let mut chunks = Reassembler::new(owner, generation);
            let mut output = Vec::new();
            for step in s["steps"].as_array().unwrap() {
                let frame = ClientEnvelope::decode(&serde_json::to_vec(step).unwrap())?;
                match chunks.accept(&frame.body) {
                    Ok(Some(bytes)) => output.extend(bytes),
                    Ok(None) => (),
                    Err(e) => {
                        assert_eq!(chunks.accept(&frame.body).unwrap_err(), WireError::Closed);
                        return Err(e);
                    }
                }
            }
            assert_eq!(output, unhex(case["output_hex"].as_str().unwrap()));
            chunks.close();
        }
        "history" => {
            let mut history = RequestHistory::default();
            for (index, step) in s["steps"].as_array().unwrap().iter().enumerate() {
                let frame = ClientEnvelope::decode(&serde_json::to_vec(step).unwrap())?;
                let ClientBody::Request(request) = frame.body else {
                    panic!("history fixtures carry requests");
                };
                let outcome = rejected(if index == 0 {
                    ErrorCode::Interrupted
                } else {
                    ErrorCode::DurabilityFailure
                });
                assert_eq!(
                    history.observe(&request, outcome)?,
                    rejected(ErrorCode::Interrupted)
                );
            }
        }
        _ => panic!("unknown fixture mode"),
    }
    Ok(())
}
pub fn report() -> Vec<Value> {
    corpus()
        .iter()
        .map(|case| {
            let actual = run(case).map(|_| "ok").unwrap_or_else(WireError::as_str);
            assert_eq!(actual, case["expected"].as_str().unwrap(), "{}", case["id"]);
            json!({"id":case["id"], "outcome":actual})
        })
        .collect()
}
