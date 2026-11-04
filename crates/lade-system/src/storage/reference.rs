//! The stored JSON form of a feedback Reference (draft rows, draft revisions and the items of
//! accepted input). The shape is owned here, not by the client protocol, so a wire change
//! cannot silently change how stored rows decode. It matches the JSON earlier versions wrote,
//! so existing rows keep decoding.

use lade_core::domain::ids::{CandidateId, CaptureId, Uuid};
use lade_core::domain::inspection::{
    ArtifactDigest, ByteRange, InspectedVersion, Reference, RepoPath, Side,
};
use lade_protocol::types::WirePath;
use serde_json::{Map, Value, json};

pub(super) fn encode(reference: &Reference) -> Value {
    let version = match reference.version() {
        InspectedVersion::Working { capture_id } => {
            json!({"kind": "working", "capture_id": capture_id.to_string()})
        }
        InspectedVersion::Candidate { candidate_id } => {
            json!({"kind": "candidate", "candidate_id": candidate_id.to_string()})
        }
    };
    let path = WirePath::from_bytes(reference.path().as_bytes().to_vec())
        .ok()
        .and_then(|p| serde_json::to_value(p).ok())
        .unwrap_or(Value::Null);
    let range = reference.byte_range();
    json!({
        "artifact_digest": reference.artifact_digest().hex(),
        "path": path,
        "version": version,
        "side": reference.side().code(),
        "byte_start": range.start().to_string(),
        "byte_end": range.end().to_string(),
        "selected_text": reference.selected_text(),
        "line_hint": reference.line_hint().to_string(),
    })
}

/// Decodes a stored reference; `None` means the stored value is corrupt.
pub(super) fn decode(value: &Value) -> Option<Reference> {
    let fields = value.as_object()?;
    let text = |map: &Map<String, Value>, name: &str| map.get(name)?.as_str().map(str::to_owned);
    let number = |name: &str| text(fields, name)?.parse::<u64>().ok();
    let version = fields.get("version")?.as_object()?;
    let id = |name: &str| Uuid::parse(&text(version, name)?).ok();
    let version = match version.get("kind")?.as_str()? {
        "working" => InspectedVersion::Working {
            capture_id: CaptureId::new(id("capture_id")?),
        },
        "candidate" => InspectedVersion::Candidate {
            candidate_id: CandidateId::new(id("candidate_id")?),
        },
        _ => return None,
    };
    let side = match fields.get("side")?.as_str()? {
        "old" => Side::Old,
        "new" => Side::New,
        _ => return None,
    };
    let path: WirePath = serde_json::from_value(fields.get("path")?.clone()).ok()?;
    let line_hint = match fields.get("line_hint") {
        None | Some(Value::Null) => 1,
        Some(_) => u32::try_from(number("line_hint")?).ok()?.max(1),
    };
    Reference::new(
        ArtifactDigest::parse(&text(fields, "artifact_digest")?).ok()?,
        RepoPath::new(path.into_bytes()).ok()?,
        version,
        side,
        ByteRange::new(number("byte_start")?, number("byte_end")?).ok()?,
        text(fields, "selected_text")?,
        line_hint,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const CAPTURE: &str = "6f1d6a8e-2c4b-4d6e-9f0a-1b2c3d4e5f60";

    #[test]
    fn rows_written_in_the_original_shape_decode() {
        let stored = json!({
            "artifact_digest": DIGEST,
            "path": {"b64": "c3JjL2EucnM=", "display": "src/a.rs"},
            "version": {"kind": "working", "capture_id": CAPTURE},
            "side": "new",
            "byte_start": "4",
            "byte_end": "7",
            "selected_text": "abc",
            "line_hint": "3",
        });
        let reference = decode(&stored).expect("decodes");
        assert_eq!(reference.path().as_bytes(), b"src/a.rs");
        assert_eq!(reference.side(), Side::New);
        assert_eq!(
            (reference.byte_range().start(), reference.byte_range().end()),
            (4, 7)
        );
        assert_eq!(reference.line_hint(), 3);
        assert_eq!(encode(&reference), stored);
        assert_eq!(decode(&encode(&reference)), Some(reference));
    }

    #[test]
    fn corrupt_rows_do_not_decode() {
        let good = json!({
            "artifact_digest": DIGEST,
            "path": {"b64": "c3JjL2EucnM=", "display": "src/a.rs"},
            "version": {"kind": "candidate", "candidate_id": CAPTURE},
            "side": "old",
            "byte_start": "0",
            "byte_end": "3",
            "selected_text": "abc",
        });
        assert!(decode(&good).is_some_and(|r| r.line_hint() == 1));
        for (field, bad) in [
            ("side", json!("left")),
            ("byte_end", json!("0")),
            ("selected_text", json!("abcd")),
            ("version", json!({"kind": "draft"})),
        ] {
            let mut value = good.clone();
            value[field] = bad;
            assert_eq!(decode(&value), None, "{field}");
        }
    }
}
