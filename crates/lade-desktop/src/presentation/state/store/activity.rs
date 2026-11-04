//! An objective's activity log, merged from history pages and appended events.

use std::collections::HashMap;

use crate::presentation::state::model::{ActivityItem, ActivityItemId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActivityChange {
    Reset,
    Appended { from_index: usize },
    Delta { index: usize },
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ActivityLog {
    pub items: Vec<ActivityItem>,
    pub has_more: bool,
    pub loaded: bool,
}

impl ActivityLog {
    pub(super) fn merge_page(&mut self, items: Vec<ActivityItem>, has_more: bool) {
        let known_oldest = self.items.first().map(|i| i.seq);
        let page_oldest = items.iter().map(|i| i.seq).min();
        let decides = match (known_oldest, page_oldest) {
            (Some(known), Some(page)) => page <= known,
            _ => true,
        };
        let index: HashMap<ActivityItemId, usize> = self
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| (item.item_id.clone(), i))
            .collect();
        let mut fresh = Vec::new();
        for item in items {
            match index.get(&item.item_id) {
                Some(&i) => self.items[i] = item,
                None => fresh.push(item),
            }
        }
        self.items.extend(fresh);
        self.items.sort_by_key(|i| i.seq);
        if decides || !self.loaded {
            self.has_more = has_more;
        }
        self.loaded = true;
    }

    pub(super) fn append(&mut self, items: Vec<ActivityItem>) -> ActivityChange {
        let from_index = self.items.len();
        let mut out_of_order = false;
        for item in items {
            if let Some(existing) = self.items.iter_mut().find(|i| i.item_id == item.item_id) {
                *existing = item;
                out_of_order = true;
            } else {
                out_of_order |= self.items.last().is_some_and(|last| last.seq > item.seq);
                self.items.push(item);
            }
        }
        if out_of_order {
            self.items.sort_by_key(|i| i.seq);
            ActivityChange::Reset
        } else {
            ActivityChange::Appended { from_index }
        }
    }

    pub(super) fn append_text(&mut self, item_id: &ActivityItemId, text: &str) -> Option<usize> {
        let index = self.items.iter().position(|i| &i.item_id == item_id)?;
        self.items[index].text.push_str(text);
        Some(index)
    }
}
