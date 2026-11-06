#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code fails loudly on broken fixtures"
)]

mod support;

#[test]
fn shared_contract_corpus() {
    support::report();
}

#[test]
fn unicode_decodes_without_normalization() {
    let bytes = "\"á\\r\\n\\t漢字😀e\u{301}\u{2028}\u{2029}\"".as_bytes();
    assert_eq!(
        lade_protocol::bounded_json(bytes).unwrap(),
        "á\r\n\t漢字😀e\u{301}\u{2028}\u{2029}"
    );
}
