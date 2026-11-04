use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use std::fmt;

use crate::{MAX_DEPTH, MAX_ENTRIES, MAX_FRAME, MAX_NODES, MAX_STRING, WireError};

struct Budget {
    nodes: usize,
    error: Option<WireError>,
}
struct Seed<'a> {
    budget: &'a mut Budget,
    depth: usize,
}

impl Seed<'_> {
    fn reject<E: de::Error>(&mut self, error: WireError) -> E {
        self.budget.error = Some(error);
        E::custom(error)
    }
}
impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = Value;
    fn deserialize<D: de::Deserializer<'de>>(mut self, d: D) -> Result<Value, D::Error> {
        self.budget.nodes += 1;
        if self.budget.nodes > MAX_NODES {
            return Err(self.reject(WireError::LimitReached));
        }
        d.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Seed<'_> {
    type Value = Value;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bounded JSON")
    }
    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }
    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_i64<E: de::Error>(mut self, v: i64) -> Result<Value, E> {
        if v.unsigned_abs() > 9_007_199_254_740_991 {
            return Err(self.reject(WireError::InvalidJson));
        }
        Ok(v.into())
    }
    fn visit_u64<E: de::Error>(mut self, v: u64) -> Result<Value, E> {
        if v > 9_007_199_254_740_991 {
            return Err(self.reject(WireError::InvalidJson));
        }
        Ok(v.into())
    }
    fn visit_f64<E: de::Error>(mut self, _: f64) -> Result<Value, E> {
        Err(self.reject(WireError::InvalidJson))
    }
    fn visit_str<E: de::Error>(mut self, v: &str) -> Result<Value, E> {
        if v.len() > MAX_STRING {
            return Err(self.reject(WireError::LimitReached));
        }
        Ok(v.into())
    }
    fn visit_string<E: de::Error>(mut self, v: String) -> Result<Value, E> {
        if v.len() > MAX_STRING {
            return Err(self.reject(WireError::LimitReached));
        }
        Ok(Value::String(v))
    }
    fn visit_seq<A: SeqAccess<'de>>(mut self, mut seq: A) -> Result<Value, A::Error> {
        if self.depth >= MAX_DEPTH {
            return Err(self.reject(WireError::LimitReached));
        }
        let mut values = Vec::new();
        while let Some(value) = seq.next_element_seed(Seed {
            budget: self.budget,
            depth: self.depth + 1,
        })? {
            if values.len() == MAX_ENTRIES {
                return Err(self.reject(WireError::LimitReached));
            }
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(mut self, mut map: A) -> Result<Value, A::Error> {
        if self.depth >= MAX_DEPTH {
            return Err(self.reject(WireError::LimitReached));
        }
        let mut values = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if key.len() > MAX_STRING || values.len() == MAX_ENTRIES {
                return Err(self.reject(WireError::LimitReached));
            }
            if values.contains_key(&key) {
                return Err(self.reject(WireError::DuplicateKey));
            }
            let value = map.next_value_seed(Seed {
                budget: self.budget,
                depth: self.depth + 1,
            })?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}

/// Parses one JSON frame within the node, depth and size limits, refusing duplicate keys.
///
/// # Errors
///
/// - `WireError::FrameSize` for an empty or oversized frame.
/// - `WireError::InvalidUtf8` or `WireError::InvalidJson` for malformed text.
/// - `WireError::DuplicateKey` for an object with a repeated key.
/// - `WireError::LimitReached` when the frame exceeds the node, depth, string or entry limits.
pub fn bounded_json(bytes: &[u8]) -> Result<Value, WireError> {
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err(WireError::FrameSize);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| WireError::InvalidUtf8)?;
    let mut budget = Budget {
        nodes: 0,
        error: None,
    };
    let mut decoder = serde_json::Deserializer::from_str(text);
    let value = Seed {
        budget: &mut budget,
        depth: 0,
    }
    .deserialize(&mut decoder)
    .map_err(|_| budget.error.unwrap_or(WireError::InvalidJson))?;
    decoder.end().map_err(|_| WireError::InvalidJson)?;
    Ok(value)
}
