use crate::{MAX_CHUNK, WireError};
use serde_json::Value;
use std::sync::LazyLock;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Contract {
    Client,
    Worker,
}

static CLIENT: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../contracts/v1/schema.json"))
        .expect("checked-in schema")
});
static WORKER: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../contracts/v1/worker-schema.json"))
        .expect("checked-in worker schema")
});

pub fn validate(value: &Value, contract: Contract) -> Result<(), WireError> {
    let root: &Value = match contract {
        Contract::Client => &CLIENT,
        Contract::Worker => &WORKER,
    };
    if matches(value, root, root) {
        Ok(())
    } else {
        Err(WireError("invalid_schema"))
    }
}

fn length(schema: &Value, key: &str) -> Option<usize> {
    schema.get(key).and_then(Value::as_u64).map(|n| n as usize)
}

fn matches(value: &Value, schema: &Value, root: &Value) -> bool {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        return root
            .pointer(&reference[1..])
            .is_some_and(|s| matches(value, s, root));
    }
    if let Some(choices) = schema.get("oneOf").and_then(Value::as_array) {
        return choices.iter().filter(|s| matches(value, s, root)).count() == 1;
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
            let required = schema["required"].as_array().expect("schema required");
            required
                .iter()
                .all(|k| k.as_str().is_some_and(|k| obj.contains_key(k)))
                && obj
                    .iter()
                    .all(|(k, v)| props.get(k).is_some_and(|s| matches(v, s, root)))
        }
        Some("array") => {
            let Some(items) = value.as_array() else {
                return false;
            };
            let Some(max) = length(schema, "maxItems") else {
                return false;
            };
            items.len() <= max
                && items.len() >= length(schema, "minItems").unwrap_or(0)
                && items.iter().all(|v| matches(v, &schema["items"], root))
        }
        Some("string") => value.as_str().is_some_and(|s| {
            let count = || s.chars().count();
            length(schema, "minLength").is_none_or(|min| count() >= min)
                && length(schema, "maxLength").is_none_or(|max| count() <= max)
                && schema
                    .get("format")
                    .and_then(Value::as_str)
                    .is_none_or(|kind| format(s, kind))
        }),
        Some("boolean") => value.is_boolean(),
        _ => false,
    }
}

fn lower_hex(b: u8) -> bool {
    b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
}

fn canonical_decimal(s: &str) -> Option<u64> {
    if s.is_empty() || (s != "0" && s.starts_with('0')) || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u64>().ok()
}

pub(crate) fn timestamp_parts(s: &str) -> Option<[u32; 7]> {
    let b = s.as_bytes();
    if b.len() != 24 {
        return None;
    }
    let layout = b"dddd-dd-ddTdd:dd:dd.dddZ";
    if !b.iter().zip(layout).all(|(c, l)| {
        if *l == b'd' {
            c.is_ascii_digit()
        } else {
            c == l
        }
    }) {
        return None;
    }
    let n = |range: std::ops::Range<usize>| s[range].parse::<u32>().expect("digits");
    let parts = [
        n(0..4),
        n(5..7),
        n(8..10),
        n(11..13),
        n(14..16),
        n(17..19),
        n(20..23),
    ];
    let [year, month, day, hour, minute, second, _] = parts;
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    ((1970..=9999).contains(&year)
        && (1..=days).contains(&day)
        && hour < 24
        && minute < 60
        && second < 60)
        .then_some(parts)
}

pub(crate) fn format(s: &str, kind: &str) -> bool {
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
        "u64" => canonical_decimal(s).is_some(),
        "page-limit" => canonical_decimal(s).is_some_and(|n| (1..=200).contains(&n)),
        "read-limit" => canonical_decimal(s).is_some_and(|n| (1..=262_144).contains(&n)),
        "sha256" | "capability" => s.len() == 64 && s.bytes().all(lower_hex),
        "git-sha1" => s.len() == 40 && s.bytes().all(lower_hex),
        "chunk-hex" => {
            !s.is_empty()
                && s.len() <= MAX_CHUNK * 2
                && s.len().is_multiple_of(2)
                && s.bytes().all(lower_hex)
        }
        "timestamp" => timestamp_parts(s).is_some(),
        "path-b64" => crate::base64::decode(s)
            .is_some_and(|b| (1..=crate::types::MAX_PATH_BYTES).contains(&b.len())),
        _ => false,
    }
}
