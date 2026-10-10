//! Bounded text inputs with atomic edits (presentation only; the service enforces the limits).
//!
//! A paste or other user edit is all-or-nothing. GTK brackets one user edit — including the
//! deletion of a replaced selection, which happens *before* the insertion — between
//! `begin-user-action` and `end-user-action`. [`EditGuard`] models that protocol:
//!
//! 1. `begin` takes a snapshot of the accepted text and the oriented selection (cursor, anchor).
//!    The snapshot is bounded because accepted text is always within the limit.
//! 2. `insert` decides each insertion before it happens. An insertion that would exceed the limit
//!    is stopped, so an oversized payload is never inserted just to be measured; after one
//!    rejection the rest of that action is stopped too.
//! 3. Intermediate changes inside the action are never committed to the model.
//! 4. `end` either commits the one complete revision or asks the view to restore the snapshot
//!    (text, cursor and selection), so a rejected paste preserves the draft exactly.
//!
//! Limits are the protocol's real bounds, never invented ones: every text field the service
//! stores (draft body, message, intent fields) is bounded by the domain's `MAX_TEXT_BYTES`
//! (256 KiB), which equals the wire string bound [`lade_protocol::MAX_STRING`]; the approval
//! override reason has the schema bound `maxLength: 2048` (code points).

/// Draft bodies, direct messages and intent fields (`lade-core` `MAX_TEXT_BYTES`).
pub const TEXT_LIMIT: TextLimit = TextLimit::Bytes(lade_protocol::MAX_STRING);
/// `review.approve` `override_reason` (`contracts/v1/schema.json`: `maxLength: 2048`).
pub const OVERRIDE_REASON_LIMIT: TextLimit = TextLimit::Chars(2048);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextLimit {
    /// UTF-8 bytes.
    Bytes(usize),
    /// Unicode scalar values (JSON Schema `maxLength`).
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

    /// A size in this limit's unit, e.g. `256 KiB` or `2,048 characters`.
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

/// Accepted text and oriented selection before a user edit (character offsets, as GTK uses).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub text: String,
    pub cursor: i32,
    pub anchor: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertDecision {
    Allow,
    /// Stop this insertion; `attempted` is the size the text would have had.
    Reject {
        attempted: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EndDecision {
    /// Commit the buffer's text as one revision.
    Commit,
    /// Restore the snapshot; nothing reaches the model. `attempted` is the rejected size.
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

    pub fn limit(&self) -> TextLimit {
        self.limit
    }

    /// Outermost `begin-user-action`.
    pub fn begin(&mut self, snapshot: Snapshot) {
        self.generation = self.generation.wrapping_add(1);
        self.rejected = None;
        self.snapshot = Some(snapshot);
    }

    /// Whether a user action is open (changes are not committed until it ends).
    pub fn in_action(&self) -> bool {
        self.snapshot.is_some()
    }

    /// Increments with every user action; a deferred selection restore checks it so it never
    /// overrides a later edit.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Decides an insertion into `current` (the buffer as it is now, after any deletion made
    /// earlier in the same action).
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

    /// Outermost `end-user-action`.
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

    /// Visible explanation of a rejected edit to `what` (e.g. "this feedback draft").
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

    /// A minimal model of a GTK buffer driven the way GTK drives it during a paste.
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

        /// Paste `payload` replacing byte range `selection` (GTK: delete, then insert).
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
        // Ordinary editing works afterwards.
        let mut buffer = Buffer::new(limit, "hello world");
        assert!(buffer.paste(0..0, &"x".repeat(17)).is_some());
        assert_eq!(buffer.paste(11..11, "!"), None);
        assert_eq!(buffer.commits, ["hello world!"]);
    }

    #[test]
    fn exact_limit_is_accepted_in_one_commit_counting_the_replaced_selection() {
        let limit = TextLimit::Bytes(16);
        let mut buffer = Buffer::new(limit, "hello world");
        // 11 bytes, replace "hello" (5) with 10 bytes: 16 exactly.
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
        // A later, smaller edit is unaffected.
        assert_eq!(guard.insert("abc", "d"), InsertDecision::Allow);
        assert!(
            guard
                .rejection("this draft", 5)
                .contains("over the limit of 4 bytes")
        );
    }
}
