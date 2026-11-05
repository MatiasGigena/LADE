//! Execution activity: the items a worker (or the service) reports, their live deltas, and
//! the bounded rows that are stored and paged.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use lade_core::domain::ids::ObjectiveId;
use lade_protocol::client::{ActivityAppended, ActivityDelta, ActivityItem, Event};
use lade_protocol::types::{ActivityRole, Counter, Id, truncate_utf8};
use lade_protocol::worker::MAX_ITEM_TEXT_BYTES;

use crate::storage::{self, ActivityRow};

use crate::service::Service;
use crate::service::convert::{preview, wire_id};
use crate::support::{new_uuid, now_ms, wire_ts};

const LIVE_ITEM_RETENTION: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(crate) struct LiveActivity {
    items: HashMap<Id, LiveItem>,
    deltas: Vec<(ObjectiveId, Id, String)>,
}

/// One activity item as a worker (or the service itself) reported it, before it is bounded
/// and stored.
#[derive(Clone, Copy)]
pub(super) struct ActivityUpdate<'a> {
    pub item_id: &'a Id,
    pub role: ActivityRole,
    pub title: &'a str,
    pub text: &'a str,
    pub is_error: bool,
    pub truncated: bool,
    pub tool_name: Option<&'a str>,
}

struct LiveItem {
    objective: ObjectiveId,
    row: ActivityRow,
    dirty: bool,
    touched: Instant,
}

pub(crate) fn wire_activity(row: &ActivityRow) -> ActivityItem {
    ActivityItem {
        item_id: row.item_id.clone(),
        seq: Counter::new(row.seq),
        at: wire_ts(row.at),
        role: row.role,
        title: preview(&row.title, 1024),
        text: row.text.clone(),
        is_error: row.is_error,
        truncated: row.truncated,
        tool_name: row.tool_name.as_ref().map(|t| preview(t, 256)),
    }
}

impl Service {
    fn next_activity_seq(&mut self, oid: ObjectiveId) -> u64 {
        let seq = self.activity_seq.entry(oid).or_insert(0);
        *seq += 1;
        *seq
    }

    pub(super) fn activity_item(&mut self, oid: ObjectiveId, update: &ActivityUpdate<'_>) {
        let ActivityUpdate {
            item_id,
            role,
            title,
            text,
            is_error,
            truncated,
            tool_name,
        } = *update;
        let existing = match self.live.items.get(item_id) {
            Some(live) => Some((live.objective, live.row.seq, live.row.at)),
            None => storage::load_activity_item(self.store.conn(), item_id)
                .ok()
                .flatten()
                .map(|(o, row)| (o, row.seq, row.at)),
        };
        let (seq, at) = match existing {
            Some((owner, _, _)) if owner != oid => return,
            Some((_, seq, at)) => (seq, at),
            None => (self.next_activity_seq(oid), now_ms()),
        };
        let (bounded, cut) = truncate_utf8(text, MAX_ITEM_TEXT_BYTES);
        let row = ActivityRow {
            item_id: item_id.clone(),
            seq,
            at,
            role,
            title: preview(title, 1024),
            text: bounded.to_string(),
            tool_name: tool_name.map(|t| preview(t, 256)),
            is_error,
            truncated: truncated || cut,
        };
        if storage::upsert_activity(self.store.conn(), oid, &row).is_err() {
            eprintln!("laded: could not record activity objective={oid}");
        }
        self.live.deltas.retain(|(_, id, _)| id != item_id);
        let item = wire_activity(&row);
        self.live.items.insert(
            item_id.clone(),
            LiveItem {
                objective: oid,
                row,
                dirty: false,
                touched: Instant::now(),
            },
        );
        self.emit(Event::ActivityAppended(ActivityAppended {
            objective_id: wire_id(oid.uuid()),
            items: vec![item],
        }));
    }

    pub(super) fn activity_delta(&mut self, oid: ObjectiveId, item_id: &Id, append: &str) {
        let Some(live) = self.live.items.get_mut(item_id) else {
            return;
        };
        if live.objective != oid || live.row.truncated {
            return;
        }
        let room = MAX_ITEM_TEXT_BYTES.saturating_sub(live.row.text.len());
        let (accepted, cut) = truncate_utf8(append, room);
        live.row.text.push_str(accepted);
        live.row.truncated |= cut;
        live.dirty = true;
        live.touched = Instant::now();
        if accepted.is_empty() {
            return;
        }
        match self.live.deltas.last_mut() {
            Some((o, id, text)) if *o == oid && id == item_id => text.push_str(accepted),
            _ => self
                .live
                .deltas
                .push((oid, item_id.clone(), accepted.to_string())),
        }
    }

    pub(crate) fn system_item(
        &mut self,
        oid: ObjectiveId,
        title: &str,
        text: &str,
        is_error: bool,
    ) {
        let id = wire_id(new_uuid());
        self.activity_item(
            oid,
            &ActivityUpdate {
                item_id: &id,
                role: ActivityRole::System,
                title,
                text,
                is_error,
                truncated: false,
                tool_name: None,
            },
        );
    }

    pub(crate) fn flush_deltas(&mut self) {
        for (oid, item_id, text) in std::mem::take(&mut self.live.deltas) {
            self.emit(Event::ActivityDelta(ActivityDelta {
                objective_id: wire_id(oid.uuid()),
                item_id,
                append_text: text,
            }));
        }
    }

    pub(crate) fn flush_activity(&mut self) {
        for live in self.live.items.values_mut().filter(|l| l.dirty) {
            if storage::upsert_activity(self.store.conn(), live.objective, &live.row).is_ok() {
                live.dirty = false;
            }
        }
        self.live
            .items
            .retain(|_, l| l.dirty || l.touched.elapsed() < LIVE_ITEM_RETENTION);
    }

    pub(crate) fn live_text(&self, item_id: &Id) -> Option<(&str, bool)> {
        self.live
            .items
            .get(item_id)
            .map(|l| (l.row.text.as_str(), l.row.truncated))
    }
}
