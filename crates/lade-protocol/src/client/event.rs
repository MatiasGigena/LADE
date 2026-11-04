//! Event frames and the event payloads that have no other home.

use super::{ActivityItem, Event, EventName, NoticeCode, NoticeLevel};
use crate::{
    WireError,
    types::{Counter, Id},
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

impl Serialize for EventName {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
impl<'de> Deserialize<'de> for EventName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::parse(&name).ok_or_else(|| serde::de::Error::custom("invalid_schema"))
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(try_from = "RawEvent")]
pub struct EventFrame {
    pub service_epoch: Id,
    pub sequence: Counter,
    pub event: Event,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEvent {
    service_epoch: Id,
    sequence: Counter,
    name: EventName,
    payload: Value,
}
impl TryFrom<RawEvent> for EventFrame {
    type Error = WireError;
    fn try_from(raw: RawEvent) -> Result<Self, WireError> {
        Ok(Self {
            service_epoch: raw.service_epoch,
            sequence: raw.sequence,
            event: Event::from_parts(raw.name, raw.payload)?,
        })
    }
}
impl TryFrom<&EventFrame> for RawEvent {
    type Error = WireError;
    fn try_from(frame: &EventFrame) -> Result<Self, WireError> {
        Ok(Self {
            service_epoch: frame.service_epoch.clone(),
            sequence: frame.sequence.clone(),
            name: frame.event.name(),
            payload: frame.event.payload_value()?,
        })
    }
}
impl Serialize for EventFrame {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        RawEvent::try_from(self)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

wire_struct!(ActivityAppended {
    objective_id: Id,
    items: Vec<ActivityItem>,
});
wire_struct!(ActivityDelta {
    objective_id: Id,
    item_id: Id,
    append_text: String,
});
wire_struct!(WorktreeChanged {
    objective_id: Id,
    capture_id: Id,
});
wire_struct!(ServiceNotice {
    level: NoticeLevel,
    code: NoticeCode,
    message: String,
});
