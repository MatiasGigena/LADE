use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use lade_protocol::types::Id;

/// Pages are private until complete. Changes received between replies overlay the
/// earlier pages; a later page already reflects earlier changes on the FIFO link.
pub(super) struct Pages<K, T, C> {
    pub request_id: Id,
    pub after: Option<C>,
    records: Vec<(K, T)>,
    positions: HashMap<K, usize>,
    changes: HashMap<K, T>,
    change_order: Vec<K>,
    changed: HashSet<K>,
    removed: HashSet<K>,
}

impl<K: Clone + Eq + Hash, T, C> Pages<K, T, C> {
    pub fn new(request_id: Id) -> Self {
        Self {
            request_id,
            after: None,
            records: Vec::new(),
            positions: HashMap::new(),
            changes: HashMap::new(),
            change_order: Vec::new(),
            changed: HashSet::new(),
            removed: HashSet::new(),
        }
    }

    pub fn page(&mut self, records: impl IntoIterator<Item = (K, T)>) {
        for (id, record) in records {
            self.changes.remove(&id);
            if let Some(position) = self.positions.get(&id) {
                self.records[*position].1 = record;
            } else {
                self.positions.insert(id.clone(), self.records.len());
                self.records.push((id, record));
            }
        }
    }

    pub fn change(&mut self, id: K, record: T) {
        self.removed.remove(&id);
        if self.changed.insert(id.clone()) {
            self.change_order.push(id.clone());
        }
        self.changes.insert(id, record);
    }

    pub fn remove(&mut self, id: K) {
        self.changes.remove(&id);
        self.removed.insert(id);
    }

    pub fn complete(mut self) -> Vec<T> {
        let mut complete = Vec::with_capacity(self.records.len() + self.changes.len());
        for (id, record) in self.records {
            if !self.removed.contains(&id) {
                complete.push(self.changes.remove(&id).unwrap_or(record));
            }
        }
        // Event-only additions follow the historical page order, including when
        // an event arrived before an older record's page was requested.
        for id in self.change_order {
            if let Some(record) = self.changes.remove(&id) {
                complete.push(record);
            }
        }
        complete
    }
}
