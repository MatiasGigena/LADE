use super::{id, typed_id, wire};
use lade_protocol::{
    ClientBody, ClientEnvelope, Framing, Message, RequestHistory,
    client::{
        Empty, ErrorCode, MessageSendCommand, ObjectiveSelector, Rejection, Reply, ReplyData,
        ReplyResult, Request, RequestFrame, RequestName, SafeAction,
    },
    decode_client_frame, encode,
    types::DeliveryMode,
};
use serde_json::json;

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
            .as_str(),
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
        ReplyData::DraftDelete(Empty {}),
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
    assert_eq!(rejection.error.as_str(), "invalid_schema");
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
        (version.error.as_str(), version.request_id),
        ("version_mismatch", None)
    );
    assert!(decode_client_frame(&wire(json!({"kind":"hello"}))).is_ok());
}
