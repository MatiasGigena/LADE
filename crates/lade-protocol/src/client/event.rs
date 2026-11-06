//! Event frames and the event payloads that have no other home.

use super::{ActivityItem, Event, EventName, NoticeCode, NoticeLevel};
use crate::{
    WireError,
    types::{Counter, Id},
};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ClientBody;
    use crate::client::fixtures::{counter, id, objective_detail, round_trip};
    use serde_json::json;

    fn frame(event: Event) -> EventFrame {
        EventFrame {
            service_epoch: id(9),
            sequence: counter(3),
            event,
        }
    }

    #[test]
    fn event_names_parse_and_serialize_as_their_wire_names() {
        for name in EventName::ALL {
            assert_eq!(EventName::parse(name.as_str()), Some(*name));
            assert_eq!(
                serde_json::to_value(name).expect("serialize"),
                json!(name.as_str())
            );
        }
        assert_eq!(EventName::parse("objective.deleted"), None);
        assert!(serde_json::from_value::<EventName>(json!("Objective.Changed")).is_err());
    }

    #[test]
    fn frames_carry_the_name_of_their_typed_payload() {
        let changed = frame(Event::ObjectiveChanged(objective_detail().summary()));
        let json = round_trip(&ClientBody::Event(changed.clone()), "Event");
        assert_eq!(json["name"], json!("objective.changed"));
        assert_eq!(json["sequence"], json!("3"));
        assert_eq!(changed.event.name(), EventName::ObjectiveChanged);
        let delta = frame(Event::ActivityDelta(ActivityDelta {
            objective_id: id(1),
            item_id: id(2),
            append_text: "more".into(),
        }));
        let json = round_trip(&ClientBody::Event(delta), "Event");
        assert_eq!(json["payload"]["append_text"], json!("more"));
        round_trip(
            &ClientBody::Event(frame(Event::WorktreeChanged(WorktreeChanged {
                objective_id: id(1),
                capture_id: id(4),
            }))),
            "Event",
        );
    }

    #[test]
    fn a_payload_of_another_event_or_an_unknown_name_is_refused() {
        let worktree = json!({"objective_id": id(1).as_str(), "capture_id": id(4).as_str()});
        let mismatched = json!({
            "service_epoch": id(9).as_str(), "sequence": "1",
            "name": "draft.changed", "payload": worktree
        });
        assert!(serde_json::from_value::<EventFrame>(mismatched).is_err());
        assert_eq!(
            Event::from_parts(EventName::DraftChanged, worktree.clone()).map(|e| e.name()),
            Err(WireError::InvalidSchema)
        );
        assert_eq!(
            Event::from_parts(EventName::WorktreeChanged, worktree).map(|e| e.name()),
            Ok(EventName::WorktreeChanged)
        );
        let unknown = json!({
            "service_epoch": id(9).as_str(), "sequence": "1",
            "name": "shell.output", "payload": {}
        });
        assert!(serde_json::from_value::<EventFrame>(unknown).is_err());
        let extra = json!({
            "service_epoch": id(9).as_str(), "sequence": "1",
            "name": "activity.appended",
            "payload": {"objective_id": id(1).as_str(), "items": []},
            "replayed": true
        });
        assert!(serde_json::from_value::<EventFrame>(extra).is_err());
    }
}
