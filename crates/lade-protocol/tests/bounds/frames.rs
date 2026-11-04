use super::{id, message, typed_id};
use lade_protocol::{
    ClientBody, ClientEnvelope, Decoder, Framing, MAX_FRAME, Message, WireError, WorkerBody,
    WorkerEnvelope,
    client::{Reply, ReplyData, ReplyResult},
    encode,
};
use serde_json::{Value, json};

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
                    Err(WireError::LimitReached)
                })
                .unwrap_err()
                .as_str(),
            "limit_reached"
        );
        assert_eq!(calls, 1);
        assert_eq!(
            decoder.push(&bytes, |_| Ok(())).unwrap_err().as_str(),
            "closed"
        );
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
        encode(&frame, Framing::Client).unwrap_err().as_str(),
        "invalid_schema"
    );
    let text = "\u{1}".repeat(lade_protocol::MAX_STRING);
    let item = WorkerEnvelope::new(WorkerBody::ActivityDelta {
        item_id: typed_id(4),
        append_text: text,
    });
    assert_eq!(
        encode(&item, Framing::Worker).unwrap_err().as_str(),
        "frame_size"
    );
}

/// A reply page of records whose text field can be padded to fill a frame exactly.
struct RecordPage {
    name: &'static str,
    list: &'static str,
    field: &'static str,
    field_max: usize,
    records: Vec<Value>,
    next_after: Value,
}

fn draft_page() -> RecordPage {
    let records = (0..2)
        .map(|n| {
            json!({
                "draft_id":id(1000+n),"objective_id":id(2),"destination_session_id":id(3),
                "body":"","revision":"1","state":"unsent","stale":false,
                "reference":{"artifact_digest":"a".repeat(64),"path":{"b64":"YQ==","display":"a"},
                    "version":{"kind":"working","capture_id":id(4)},"side":"new",
                    "byte_start":"0","byte_end":"1","selected_text":"a"}
            })
        })
        .collect();
    RecordPage {
        name: "drafts.list",
        list: "drafts",
        field: "body",
        field_max: 256 * 1024,
        records,
        next_after: json!({"created_at":"2026-10-10T12:34:56.789Z","draft_id":id(1001)}),
    }
}

fn input_page() -> RecordPage {
    let records = (0..200)
        .map(|n| {
            json!({
                "input_id":id(1000+n),"objective_id":id(2),"operation_id":id(3),
                "mode":"prompt","text_preview":"","accepted_at":"2026-10-10T12:34:56.789Z",
                "delivery":"queued"
            })
        })
        .collect();
    RecordPage {
        name: "inputs.list",
        list: "inputs",
        field: "text_preview",
        field_max: 1024,
        records,
        next_after: json!("200"),
    }
}

/// The page as one client frame of exactly `MAX_FRAME` bytes. Each record's field takes as many
/// control characters (six bytes once escaped) and then plain bytes as it may, until the frame is
/// full.
fn at_frame_bound(page: RecordPage) -> Vec<u8> {
    let data = json!({page.list: page.records, "next_after": page.next_after});
    let mut frame = json!({"protocol":1,"bundle":lade_protocol::BUNDLE,
        "body":{"kind":"reply","request_id":id(1),"name":page.name,"result":{"status":"ok","data":data}}});
    let mut remaining = MAX_FRAME - serde_json::to_vec(&frame).unwrap().len();
    for record in frame["body"]["result"]["data"][page.list]
        .as_array_mut()
        .unwrap()
    {
        let controls = page.field_max.min(remaining / 6);
        let plain = (page.field_max - controls).min(remaining - controls * 6);
        record[page.field] = json!(format!("{}{}", "\u{1}".repeat(controls), "a".repeat(plain)));
        remaining -= controls * 6 + plain;
    }
    assert_eq!(remaining, 0, "{} cannot fill a frame", page.name);
    let bytes = serde_json::to_vec(&frame).unwrap();
    assert_eq!(bytes.len(), MAX_FRAME);
    bytes
}

/// A page at the frame bound decodes and re-encodes to the bound; one byte more is refused both
/// when encoding and when decoding.
fn assert_exact_frame_bound(page: RecordPage, grow_by_one_byte: impl FnOnce(&mut ReplyData)) {
    let mut decoded = ClientEnvelope::decode(&at_frame_bound(page)).unwrap();
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
    grow_by_one_byte(data);
    assert_eq!(
        encode(&decoded, Framing::Client).unwrap_err(),
        WireError::FrameSize
    );
    assert_eq!(
        ClientEnvelope::decode(&serde_json::to_vec(&decoded).unwrap()).unwrap_err(),
        WireError::FrameSize
    );
}

#[test]
fn draft_pages_round_trip_at_the_exact_frame_bound_and_reject_one_byte_over() {
    assert_exact_frame_bound(draft_page(), |data| match data {
        ReplyData::DraftsList(page) => page.drafts.last_mut().unwrap().body.push('a'),
        _ => panic!("expected a draft page"),
    });
}

#[test]
fn input_pages_round_trip_at_the_exact_frame_bound_and_reject_one_byte_over() {
    assert_exact_frame_bound(input_page(), |data| match data {
        ReplyData::InputsList(page) => page.inputs.last_mut().unwrap().text_preview.push('a'),
        _ => panic!("expected an input page"),
    });
}
