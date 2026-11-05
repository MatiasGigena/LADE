//! [`Pages`]: a paginated list being loaded, kept private until complete and current with
//! changes received in between.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_overlay_earlier_pages_and_later_pages_win() {
        let mut pages: Pages<u8, &str, ()> = Pages::new(Id::from_random_bytes([1; 16]));
        pages.page([(1, "one"), (2, "two")]);
        pages.change(3, "three from an event");
        pages.change(2, "two from an event");
        pages.page([(2, "two from the next page"), (4, "four")]);
        pages.remove(1);
        assert_eq!(
            pages.complete(),
            ["two from the next page", "four", "three from an event"]
        );
    }

    #[test]
    fn a_change_after_its_page_replaces_the_record_in_place() {
        let mut pages: Pages<u8, &str, ()> = Pages::new(Id::from_random_bytes([2; 16]));
        pages.page([(1, "one"), (2, "two")]);
        pages.change(1, "one changed");
        assert_eq!(pages.complete(), ["one changed", "two"]);
    }

    #[test]
    fn a_removed_then_changed_record_comes_back() {
        let mut pages: Pages<u8, &str, ()> = Pages::new(Id::from_random_bytes([3; 16]));
        pages.page([(1, "one")]);
        pages.remove(1);
        pages.change(1, "one again");
        assert_eq!(pages.complete(), ["one again"]);
    }
}
