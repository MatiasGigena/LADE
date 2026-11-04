use lade_protocol::{
    ClientBody, ClientEnvelope, Decoder, FrameReader, FrameWriter, Framing, MAX_CHUNK, MAX_FRAME,
    MAX_TRANSFER, Message, Reassembler, RequestHistory, TransportError, WireError, WorkerBody,
    WorkerEnvelope,
    client::{
        ErrorCode, MessageSendCommand, ObjectiveSelector, Rejection, Reply, ReplyData, ReplyResult,
        Request, RequestFrame, RequestName, SafeAction,
    },
    decode_client_frame, encode,
    types::{DeliveryMode, Id},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn scope() -> Value {
    json!({"workspace_id":id(1),"objective_id":id(2),"session_id":id(3),"association_revision":"1"})
}
fn id(n: usize) -> String {
    format!("00000000-0000-4000-8000-{n:012x}")
}
fn typed_id(n: usize) -> Id {
    Id::parse(id(n)).unwrap()
}
fn wire(body: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"protocol":1,"bundle":lade_protocol::BUNDLE,"body":body})).unwrap()
}
fn message(body: Value) -> ClientEnvelope {
    ClientEnvelope::decode(&wire(body)).unwrap()
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
        let mut decoder = Decoder::<ClientEnvelope>::new(Framing::Worker);
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

fn send(request: usize, operation: usize, text: &str) -> RequestFrame {
    RequestFrame::new(
        typed_id(request),
        Some(typed_id(operation)),
        Request::MessageSend(MessageSendCommand {
            objective_id: typed_id(2),
            text: text.to_owned(),
            mode: DeliveryMode::Steer,
        }),
    )
    .unwrap()
}
fn interrupted(code: ErrorCode) -> ReplyResult {
    ReplyResult::Rejected {
        name: Some(RequestName::MessageSend),
        rejection: Rejection::new(code, SafeAction::QueryOperation, ""),
    }
}

#[test]
fn correlation_overflow_never_evicts_the_original_outcome() {
    let mut history = RequestHistory::default();
    let original = interrupted(ErrorCode::Interrupted);
    for n in 0..256 {
        history
            .observe(&send(1000 + n, 2000 + n, "x"), original.clone())
            .unwrap();
    }
    assert_eq!(
        history
            .observe(&send(1256, 2256, "x"), original.clone())
            .unwrap_err()
            .0,
        "limit_reached"
    );
    assert_eq!(
        history
            .observe(&send(1000, 2000, "x"), interrupted(ErrorCode::Conflict))
            .unwrap(),
        original
    );
}

#[test]
fn canonical_digest_ignores_request_id_only() {
    assert_eq!(
        send(1, 2, "a").canonical_digest(),
        send(9, 2, "a").canonical_digest()
    );
    assert_ne!(
        send(1, 2, "a").canonical_digest(),
        send(1, 3, "a").canonical_digest()
    );
    assert_ne!(
        send(1, 2, "a").canonical_digest(),
        send(1, 2, "b").canonical_digest()
    );
}

#[test]
fn operation_id_presence_is_bound_to_the_request_name() {
    let get = || {
        Request::ObjectiveGet(ObjectiveSelector {
            objective_id: typed_id(2),
        })
    };
    assert!(RequestFrame::new(typed_id(1), None, get()).is_ok());
    assert!(RequestFrame::new(typed_id(1), Some(typed_id(5)), get()).is_err());
    let send = send(1, 2, "x");
    assert!(RequestFrame::new(typed_id(1), None, send.request).is_err());
    for name in RequestName::ALL {
        assert_eq!(RequestName::parse(name.as_str()), Some(*name));
    }
}

#[test]
fn reply_with_typed_data_round_trips() {
    let reply = ClientEnvelope::new(ClientBody::Reply(Reply::ok(
        typed_id(4),
        ReplyData::DraftDelete(lade_protocol::client::Empty {}),
    )));
    let bytes = encode(&reply, Framing::Client).unwrap();
    assert_eq!(
        &bytes[4..],
        br#"{"protocol":1,"bundle":"0.2.0","body":{"kind":"reply","request_id":"00000000-0000-4000-8000-000000000004","name":"draft.delete","result":{"status":"ok","data":{}}}}"#
    );
    assert_eq!(ClientEnvelope::decode(&bytes[4..]).unwrap(), reply);
    let long = Rejection::new(ErrorCode::NotFound, SafeAction::Refresh, &"é".repeat(600));
    assert_eq!(long.detail.chars().count(), 512);
    let rejected = ClientEnvelope::new(ClientBody::Reply(Reply::rejected(typed_id(4), None, long)));
    assert!(encode(&rejected, Framing::Client).is_ok());
}

#[test]
fn well_formed_invalid_requests_are_recoverable_and_others_are_not() {
    let unknown =
        wire(json!({"kind":"request","request_id":id(4),"name":"shell.execute","payload":{}}));
    let rejection = decode_client_frame(&unknown).unwrap_err();
    assert_eq!(rejection.error.0, "invalid_schema");
    assert_eq!(rejection.request_id, Some(typed_id(4)));
    let bad_id = wire(json!({"kind":"request","request_id":"x","name":"snapshot","payload":{}}));
    assert_eq!(decode_client_frame(&bad_id).unwrap_err().request_id, None);
    let not_request = wire(json!({"kind":"welcome","service_epoch":id(1)}));
    assert_eq!(
        decode_client_frame(&not_request).unwrap_err().request_id,
        None
    );
    let old = br#"{"protocol":1,"bundle":"0.1.0-imp001","body":{"kind":"request","request_id":"00000000-0000-4000-8000-000000000004"}}"#;
    let version = decode_client_frame(old).unwrap_err();
    assert_eq!(
        (version.error.0, version.request_id),
        ("version_mismatch", None)
    );
    assert!(decode_client_frame(&wire(json!({"kind":"hello"}))).is_ok());
}

#[test]
fn downstream_backpressure_stops_decoding_without_delivering_later_frames() {
    let hello = message(json!({"kind":"hello"}));
    for framing in [Framing::Client, Framing::Worker] {
        let mut bytes = encode(&hello, framing).unwrap();
        bytes.extend(encode(&hello, framing).unwrap());
        let mut decoder = Decoder::<ClientEnvelope>::new(framing);
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
    if let ClientBody::ChunkData { hex, .. } = &mut frame.body {
        *hex = "a".repeat(2 * MAX_FRAME);
    }
    assert_eq!(
        encode(&frame, Framing::Client).unwrap_err().0,
        "invalid_schema"
    );
    let text = "\u{1}".repeat(lade_protocol::MAX_STRING);
    let item = WorkerEnvelope::new(WorkerBody::ActivityDelta {
        item_id: typed_id(4),
        append_text: text,
    });
    assert_eq!(encode(&item, Framing::Worker).unwrap_err().0, "frame_size");
}

struct Trickle<'a>(&'a [u8], usize);
impl std::io::Read for Trickle<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        self.1 += 1;
        if self.1.is_multiple_of(3) {
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        let n = self.0.len().min(out.len()).min(5);
        out[..n].copy_from_slice(&self.0[..n]);
        self.0 = &self.0[n..];
        Ok(n)
    }
}

#[test]
fn blocking_reader_and_writer_frame_one_message_at_a_time() {
    let bodies = [
        WorkerBody::Received {
            input_id: typed_id(7),
        },
        WorkerBody::Abort,
        WorkerBody::ActivityDelta {
            item_id: typed_id(8),
            append_text: "a\u{2028}b\nc".to_owned(),
        },
    ];
    for framing in [Framing::Client, Framing::Worker] {
        let mut writer = FrameWriter::new(Vec::new(), framing);
        for body in &bodies {
            writer.write(&WorkerEnvelope::new(body.clone())).unwrap();
        }
        let bytes = writer.into_inner();
        let mut reader = FrameReader::new(Trickle(&bytes, 0), framing);
        for body in &bodies {
            let envelope: WorkerEnvelope = reader.read().unwrap().unwrap();
            assert_eq!(&envelope.body, body);
        }
        assert!(reader.read::<WorkerEnvelope>().unwrap().is_none());

        let mut truncated = FrameReader::new(&bytes[..bytes.len() - 1], framing);
        truncated.read_frame().unwrap().unwrap();
        truncated.read_frame().unwrap().unwrap();
        assert!(matches!(
            truncated.read_frame(),
            Err(TransportError::Wire(WireError("truncated_frame")))
        ));
        assert!(matches!(
            truncated.read_frame(),
            Err(TransportError::Wire(WireError("closed")))
        ));
    }
}

#[test]
fn reader_poisons_on_invalid_frames_and_writer_rejects_invalid_messages() {
    let mut bytes = b"{\"protocol\":1}\n".to_vec();
    bytes.extend(encode(&WorkerEnvelope::new(WorkerBody::Shutdown), Framing::Worker).unwrap());
    let mut reader = FrameReader::new(&bytes[..], Framing::Worker);
    assert!(matches!(
        reader.read::<WorkerEnvelope>(),
        Err(TransportError::Wire(WireError("version_mismatch")))
    ));
    assert!(matches!(
        reader.read::<WorkerEnvelope>(),
        Err(TransportError::Wire(WireError("closed")))
    ));

    let mut writer = FrameWriter::new(Vec::new(), Framing::Worker);
    let invalid = WorkerEnvelope::new(WorkerBody::Deliver {
        input_id: typed_id(1),
        mode: DeliveryMode::Prompt,
        text: String::new(),
    });
    assert!(matches!(
        writer.write(&invalid),
        Err(TransportError::Wire(WireError("invalid_schema")))
    ));
    writer
        .write(&WorkerEnvelope::new(WorkerBody::Shutdown))
        .unwrap();
    assert_eq!(
        WorkerEnvelope::decode(writer.get_ref().strip_suffix(b"\n").unwrap())
            .unwrap()
            .body,
        WorkerBody::Shutdown
    );
    let mut stale = WorkerEnvelope::new(WorkerBody::Shutdown);
    stale.bundle = "0.1.0-imp001".to_owned();
    assert!(matches!(
        writer.write(&stale),
        Err(TransportError::Wire(WireError("version_mismatch")))
    ));
}

#[test]
fn oversized_declared_client_length_fails_before_allocation() {
    let header = ((MAX_FRAME + 1) as u32).to_be_bytes();
    let mut reader = FrameReader::new(&header[..], Framing::Client);
    assert!(matches!(
        reader.read_frame(),
        Err(TransportError::Wire(WireError("frame_size")))
    ));
}

#[test]
fn record_pages_round_trip_at_the_exact_frame_bound_and_reject_one_byte_over() {
    for name in ["drafts.list", "inputs.list"] {
        let draft = name == "drafts.list";
        let field = if draft { "body" } else { "text_preview" };
        let records: Vec<Value> = (0..if draft { 2 } else { 200 })
            .map(|n| {
                if draft {
                    json!({
                        "draft_id":id(1000+n),"objective_id":id(2),"destination_session_id":id(3),
                        "body":"","revision":"1","state":"unsent","stale":false,
                        "reference":{"artifact_digest":"a".repeat(64),"path":{"b64":"YQ==","display":"a"},
                            "version":{"kind":"working","capture_id":id(4)},"side":"new",
                            "byte_start":"0","byte_end":"1","selected_text":"a"}
                    })
                } else {
                    json!({
                        "input_id":id(1000+n),"objective_id":id(2),"operation_id":id(3),
                        "mode":"prompt","text_preview":"","accepted_at":"2026-10-10T12:34:56.789Z",
                        "delivery":"queued"
                    })
                }
            })
            .collect();
        let data = if draft {
            json!({"drafts":records,"next_after":{"created_at":"2026-10-10T12:34:56.789Z","draft_id":id(1001)}})
        } else {
            json!({"inputs":records,"next_after":"200"})
        };
        let mut page = json!({"protocol":1,"bundle":lade_protocol::BUNDLE,
            "body":{"kind":"reply","request_id":id(1),"name":name,"result":{"status":"ok","data":data}}});
        let mut remaining = MAX_FRAME - serde_json::to_vec(&page).unwrap().len();
        let records = page["body"]["result"]["data"][if draft { "drafts" } else { "inputs" }]
            .as_array_mut()
            .unwrap();
        for record in records {
            let max = if draft { 256 * 1024 } else { 1024 };
            let controls = max.min(remaining / 6);
            let ascii = (max - controls).min(remaining - controls * 6);
            record[field] = json!(format!("{}{}", "\u{1}".repeat(controls), "a".repeat(ascii)));
            remaining -= controls * 6 + ascii;
        }
        assert_eq!(remaining, 0);
        let bytes = serde_json::to_vec(&page).unwrap();
        assert_eq!(bytes.len(), MAX_FRAME);
        let mut decoded = ClientEnvelope::decode(&bytes).unwrap();
        assert_eq!(
            encode(&decoded, Framing::Client).unwrap().len(),
            MAX_FRAME + 4
        );
        let ClientBody::Reply(Reply {
            result: ReplyResult::Ok(data),
            ..
        }) = &mut decoded.body
        else {
            panic!("expected a record page")
        };
        match data.as_mut() {
            ReplyData::DraftsList(page) => page.drafts.last_mut().unwrap().body.push('a'),
            ReplyData::InputsList(page) => page.inputs.last_mut().unwrap().text_preview.push('a'),
            _ => panic!("expected a record page"),
        }
        assert_eq!(
            encode(&decoded, Framing::Client).unwrap_err().0,
            "frame_size"
        );
        assert_eq!(
            ClientEnvelope::decode(&serde_json::to_vec(&decoded).unwrap())
                .unwrap_err()
                .0,
            "frame_size"
        );
    }
}
