pub const TEXT_LIMIT: TextLimit = TextLimit::Bytes(lade_protocol::MAX_STRING);
pub const OVERRIDE_REASON_LIMIT: TextLimit = TextLimit::Chars(2048);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextLimit {
    Bytes(usize),
    Chars(usize),
}

impl TextLimit {
    pub fn measure(self, text: &str) -> usize {
        match self {
            Self::Bytes(_) => text.len(),
            Self::Chars(_) => text.chars().count(),
        }
    }

    pub fn max(self) -> usize {
        match self {
            Self::Bytes(max) | Self::Chars(max) => max,
        }
    }

    pub fn describe(self, size: usize) -> String {
        match self {
            Self::Bytes(_) if size >= 1024 && size.is_multiple_of(1024) => {
                format!("{} KiB", size / 1024)
            }
            Self::Bytes(_) if size >= 1024 => {
                let tenths = (size * 10).div_ceil(1024);
                format!("{}.{} KiB", tenths / 10, tenths % 10)
            }
            Self::Bytes(_) => format!("{size} bytes"),
            Self::Chars(_) => format!("{size} characters"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub text: String,
    pub cursor: i32,
    pub anchor: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertDecision {
    Allow,
    Reject { attempted: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EndDecision {
    Commit,
    Restore {
        snapshot: Snapshot,
        attempted: usize,
    },
}

#[derive(Clone, Debug)]
pub struct EditGuard {
    limit: TextLimit,
    snapshot: Option<Snapshot>,
    rejected: Option<usize>,
    generation: u64,
}

impl EditGuard {
    pub fn new(limit: TextLimit) -> Self {
        Self {
            limit,
            snapshot: None,
            rejected: None,
            generation: 0,
        }
    }

    #[expect(
        dead_code,
        reason = "not wired into any view yet; kept until the owner decides whether to surface or delete it"
    )]
    pub fn limit(&self) -> TextLimit {
        self.limit
    }

    pub fn begin(&mut self, snapshot: Snapshot) {
        self.generation = self.generation.wrapping_add(1);
        self.rejected = None;
        self.snapshot = Some(snapshot);
    }

    pub fn in_action(&self) -> bool {
        self.snapshot.is_some()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn insert(&mut self, current: &str, inserted: &str) -> InsertDecision {
        let attempted = self
            .limit
            .measure(current)
            .saturating_add(self.limit.measure(inserted));
        if let Some(earlier) = self.rejected {
            return InsertDecision::Reject {
                attempted: earlier.max(attempted),
            };
        }
        if attempted <= self.limit.max() {
            return InsertDecision::Allow;
        }
        if self.in_action() {
            self.rejected = Some(attempted);
        }
        InsertDecision::Reject { attempted }
    }

    pub fn end(&mut self) -> EndDecision {
        let snapshot = self.snapshot.take();
        match (self.rejected.take(), snapshot) {
            (Some(attempted), Some(snapshot)) => EndDecision::Restore {
                snapshot,
                attempted,
            },
            _ => EndDecision::Commit,
        }
    }

    pub fn rejection(&self, what: &str, attempted: usize) -> String {
        format!(
            "Not inserted: {what} would be {}, over the limit of {}. Nothing was changed; shorten the text or split it.",
            self.limit.describe(attempted),
            self.limit.describe(self.limit.max())
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Buffer {
        text: String,
        guard: EditGuard,
        commits: Vec<String>,
    }

    impl Buffer {
        fn new(limit: TextLimit, text: &str) -> Self {
            Self {
                text: text.to_owned(),
                guard: EditGuard::new(limit),
                commits: Vec::new(),
            }
        }

        fn paste(&mut self, selection: std::ops::Range<usize>, payload: &str) -> Option<usize> {
            self.guard.begin(Snapshot {
                text: self.text.clone(),
                cursor: 3,
                anchor: 1,
            });
            self.text.replace_range(selection.clone(), "");
            let mut rejected = None;
            match self.guard.insert(&self.text, payload) {
                InsertDecision::Allow => self.text.insert_str(selection.start, payload),
                InsertDecision::Reject { attempted } => rejected = Some(attempted),
            }
            match self.guard.end() {
                EndDecision::Commit => self.commits.push(self.text.clone()),
                EndDecision::Restore { snapshot, .. } => {
                    assert_eq!((snapshot.cursor, snapshot.anchor), (3, 1));
                    self.text = snapshot.text;
                }
            }
            rejected
        }
    }

    #[test]
    fn limits_are_the_protocol_bounds() {
        assert_eq!(TEXT_LIMIT, TextLimit::Bytes(256 * 1024));
        assert_eq!(TEXT_LIMIT.describe(TEXT_LIMIT.max()), "256 KiB");
        assert_eq!(TextLimit::Bytes(0).describe(300 * 1024 + 1), "300.1 KiB");
        assert_eq!(TextLimit::Bytes(0).describe(12), "12 bytes");
        assert_eq!(OVERRIDE_REASON_LIMIT.describe(2049), "2049 characters");
    }

    #[test]
    fn oversized_paste_preserves_text_and_selection_with_no_commit() {
        let limit = TextLimit::Bytes(16);
        for selection in [0..0, 0..11, 2..5] {
            let mut buffer = Buffer::new(limit, "hello world");
            let attempted = buffer.paste(selection.clone(), &"x".repeat(17));
            assert!(attempted.is_some(), "{selection:?}");
            assert_eq!(buffer.text, "hello world", "{selection:?}");
            assert!(buffer.commits.is_empty(), "no intermediate commit");
        }
        let mut buffer = Buffer::new(limit, "hello world");
        assert!(buffer.paste(0..0, &"x".repeat(17)).is_some());
        assert_eq!(buffer.paste(11..11, "!"), None);
        assert_eq!(buffer.commits, ["hello world!"]);
    }

    #[test]
    fn exact_limit_is_accepted_in_one_commit_counting_the_replaced_selection() {
        let limit = TextLimit::Bytes(16);
        let mut buffer = Buffer::new(limit, "hello world");
        assert_eq!(buffer.paste(0..5, "0123456789"), None);
        assert_eq!(buffer.text, "0123456789 world");
        assert_eq!(buffer.commits.len(), 1);
        assert_eq!(buffer.paste(16..16, "x"), Some(17));
        assert_eq!(buffer.text, "0123456789 world");
    }

    #[test]
    fn utf8_bytes_and_code_points_are_measured_in_their_own_unit() {
        let mut bytes = Buffer::new(TextLimit::Bytes(8), "");
        assert_eq!(bytes.paste(0..0, "éé日"), None, "7 bytes");
        assert_eq!(bytes.paste(7..7, "é"), Some(9));
        let mut chars = Buffer::new(TextLimit::Chars(4), "");
        assert_eq!(chars.paste(0..0, "🦀🦀🦀🦀"), None);
        assert_eq!(chars.paste(16..16, "a"), Some(5));
        assert_eq!(chars.text, "🦀🦀🦀🦀");
    }

    #[test]
    fn insertions_after_a_rejection_in_the_same_action_are_stopped() {
        let mut guard = EditGuard::new(TextLimit::Bytes(4));
        guard.begin(Snapshot {
            text: "ab".into(),
            cursor: 2,
            anchor: 2,
        });
        assert_eq!(
            guard.insert("ab", "xyz"),
            InsertDecision::Reject { attempted: 5 }
        );
        assert!(matches!(
            guard.insert("ab", "x"),
            InsertDecision::Reject { .. }
        ));
        assert!(matches!(
            guard.end(),
            EndDecision::Restore { attempted: 5, .. }
        ));
        let generation = guard.generation();
        guard.begin(Snapshot {
            text: "ab".into(),
            cursor: 2,
            anchor: 2,
        });
        assert_eq!(guard.generation(), generation + 1);
        assert_eq!(guard.insert("ab", "x"), InsertDecision::Allow);
        assert_eq!(guard.end(), EndDecision::Commit);
    }

    #[test]
    fn insertion_outside_a_user_action_is_decided_without_a_snapshot() {
        let mut guard = EditGuard::new(TextLimit::Bytes(4));
        assert!(!guard.in_action());
        assert!(matches!(
            guard.insert("abcd", "e"),
            InsertDecision::Reject { .. }
        ));
        assert_eq!(guard.insert("abc", "d"), InsertDecision::Allow);
        assert!(
            guard
                .rejection("this draft", 5)
                .contains("over the limit of 4 bytes")
        );
    }
}
