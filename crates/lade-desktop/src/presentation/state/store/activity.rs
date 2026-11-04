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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation::state::model::{ActivityRole, Timestamp};

    fn item(id: &str, seq: u64, text: &str) -> ActivityItem {
        ActivityItem {
            item_id: ActivityItemId::new(id),
            seq,
            at: Timestamp(0),
            role: ActivityRole::Assistant,
            title: String::new(),
            text: text.to_owned(),
            tool_name: None,
            is_error: false,
            truncated: false,
        }
    }

    fn texts(log: &ActivityLog) -> Vec<&str> {
        log.items.iter().map(|i| i.text.as_str()).collect()
    }

    #[test]
    fn the_first_page_marks_the_log_loaded_and_takes_its_has_more() {
        let mut log = ActivityLog::default();
        log.merge_page(Vec::new(), true);
        assert!(log.loaded);
        assert!(log.has_more, "an empty first page still decides has_more");
        let mut log = ActivityLog::default();
        log.merge_page(vec![item("b", 2, "two"), item("a", 1, "one")], false);
        assert_eq!(texts(&log), ["one", "two"], "pages are kept in seq order");
        assert!(!log.has_more);
    }

    #[test]
    fn only_a_page_reaching_the_oldest_known_item_decides_has_more() {
        let mut log = ActivityLog::default();
        log.merge_page(vec![item("b", 5, "five")], true);
        log.merge_page(vec![item("c", 9, "nine")], false);
        assert!(log.has_more, "a newer page does not close older history");
        log.merge_page(vec![item("a", 5, "same seq")], false);
        assert!(!log.has_more, "a page as old as the oldest item decides");
        assert_eq!(log.items.len(), 3);
    }

    #[test]
    fn a_page_replaces_known_items_by_id_and_adds_the_rest() {
        let mut log = ActivityLog::default();
        log.merge_page(vec![item("a", 1, "old"), item("b", 2, "b")], false);
        log.merge_page(vec![item("a", 1, "new"), item("z", 0, "first")], true);
        assert_eq!(texts(&log), ["first", "new", "b"]);
        assert!(log.has_more);
    }

    #[test]
    fn in_order_appends_report_where_they_start() {
        let mut log = ActivityLog::default();
        assert_eq!(
            log.append(vec![item("a", 1, "a"), item("b", 2, "b")]),
            ActivityChange::Appended { from_index: 0 }
        );
        assert_eq!(
            log.append(vec![item("c", 3, "c")]),
            ActivityChange::Appended { from_index: 2 }
        );
        assert_eq!(texts(&log), ["a", "b", "c"]);
        assert!(!log.loaded, "appending does not count as loading history");
    }

    #[test]
    fn out_of_order_or_repeated_appends_reset_the_log_in_seq_order() {
        let mut log = ActivityLog::default();
        log.append(vec![item("b", 2, "b")]);
        assert_eq!(log.append(vec![item("a", 1, "a")]), ActivityChange::Reset);
        assert_eq!(texts(&log), ["a", "b"]);
        assert_eq!(
            log.append(vec![item("b", 2, "b replaced")]),
            ActivityChange::Reset
        );
        assert_eq!(texts(&log), ["a", "b replaced"]);
    }

    #[test]
    fn text_appends_land_on_the_named_item_only() {
        let mut log = ActivityLog::default();
        log.append(vec![item("a", 1, "par"), item("b", 2, "x")]);
        assert_eq!(log.append_text(&ActivityItemId::new("a"), "tial"), Some(0));
        assert_eq!(texts(&log), ["partial", "x"]);
        assert_eq!(log.append_text(&ActivityItemId::new("zz"), "lost"), None);
        assert_eq!(texts(&log), ["partial", "x"]);
    }
}
