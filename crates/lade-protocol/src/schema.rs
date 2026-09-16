use crate::{MAX_CHUNK, WireError};
use serde_json::Value;
use std::sync::LazyLock;

static SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../contracts/v1/schema.json"))
        .expect("checked-in schema")
});

pub fn validate(value: &Value) -> Result<(), WireError> {
    if matches(value, &SCHEMA) {
        Ok(())
    } else {
        Err(WireError("invalid_schema"))
    }
}

fn matches(value: &Value, schema: &Value) -> bool {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        return SCHEMA
            .pointer(&reference[1..])
            .is_some_and(|s| matches(value, s));
    }
    if let Some(choices) = schema.get("oneOf").and_then(Value::as_array) {
        return choices.iter().filter(|s| matches(value, s)).count() == 1;
    }
    if let Some(expected) = schema.get("const") {
        return value == expected;
    }
    if let Some(choices) = schema.get("enum").and_then(Value::as_array) {
        return choices.contains(value);
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("object") => {
            let Some(obj) = value.as_object() else {
                return false;
            };
            let props = schema["properties"].as_object().expect("schema properties");
            obj.len() == props.len()
                && props
                    .iter()
                    .all(|(k, s)| obj.get(k).is_some_and(|v| matches(v, s)))
        }
        Some("string") => value
            .as_str()
            .is_some_and(|s| format(s, schema["format"].as_str().unwrap_or(""))),
        _ => false, // Unsupported schema constructs fail closed.
    }
}

pub(crate) fn format(s: &str, kind: &str) -> bool {
    let lower_hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
    match kind {
        "uuid-v4" => {
            let b = s.as_bytes();
            b.len() == 36
                && b[14] == b'4'
                && b"89ab".contains(&b[19])
                && b.iter().enumerate().all(|(i, c)| {
                    if [8, 13, 18, 23].contains(&i) {
                        *c == b'-'
                    } else {
                        lower_hex(*c)
                    }
                })
        }
        "u64" => {
            !s.is_empty()
                && (s == "0" || !s.starts_with('0'))
                && s.bytes().all(|b| b.is_ascii_digit())
                && s.parse::<u64>().is_ok()
        }
        "sha256" => s.len() == 64 && s.bytes().all(lower_hex),
        "chunk-hex" => {
            !s.is_empty()
                && s.len() <= MAX_CHUNK * 2
                && s.len().is_multiple_of(2)
                && s.bytes().all(lower_hex)
        }
        _ => false,
    }
}
