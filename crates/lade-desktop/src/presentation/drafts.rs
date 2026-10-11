//! Draft editing and autosave scheduling (presentation only; durability is the service's).
//!
//! Timing targets: save after 300 ms of quiet, and at least every 2 s while typing continues. Only
//! one save is in flight per draft because `draft.save` is a compare-and-set on the revision.
//! "Saved" is shown only when the service acknowledged a revision carrying the latest local text.

use super::model::{Draft, DraftId, DraftState, ObjectiveId, Reference, SessionId};

/// Monotonic milliseconds supplied by the caller (GTK: `glib::monotonic_time() / 1000`).
pub type Millis = u64;

pub const QUIET_MS: Millis = 300;
pub const MAX_INTERVAL_MS: Millis = 2_000;
/// How long closing the window waits for the service to acknowledge flushed drafts before it
/// asks the developer (the durable acknowledgement target is 1 s after the request).
pub const CLOSE_WAIT_MS: Millis = 2_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveStatus {
    /// Created locally; nothing typed yet.
    Empty,
    Unsaved,
    Saving,
    Saved {
        revision: u64,
    },
    Failed {
        message: String,
    },
    Disconnected,
    /// The service holds a newer revision with different text; never overwritten silently.
    Conflict {
        revision: u64,
    },
}

impl SaveStatus {
    pub fn label(&self) -> String {
        match self {
            Self::Empty => "Not saved yet".to_owned(),
            Self::Unsaved => "Unsaved".to_owned(),
            Self::Saving => "Saving…".to_owned(),
            Self::Saved { revision } => format!("Saved r{revision}"),
            Self::Failed { message } => format!("Save failed — {message}"),
            Self::Disconnected => "Disconnected — not saved".to_owned(),
            Self::Conflict { revision } => {
                format!("Newer saved revision r{revision} exists — choose which text to keep")
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveRequest {
    pub objective_id: ObjectiveId,
    pub draft_id: DraftId,
    pub expected_revision: u64,
    pub reference: Reference,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InFlight {
    generation: u64,
    expected_revision: u64,
    body: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftEditor {
    pub draft_id: DraftId,
    pub objective_id: ObjectiveId,
    pub destination_session_id: SessionId,
    pub reference: Reference,
    pub body: String,
    pub stale: bool,
    pub state: DraftState,
    durable_revision: u64,
    durable_body: String,
    edit_generation: u64,
    saved_generation: u64,
    in_flight: Option<InFlight>,
    first_unsaved_edit: Option<Millis>,
    last_edit: Option<Millis>,
    failed: Option<String>,
    conflict: Option<u64>,
    awaiting_reconcile: bool,
}

impl DraftEditor {
    /// A new local draft anchored to `reference`; nothing is saved until text is typed.
    pub fn new_local(
        draft_id: DraftId,
        objective_id: ObjectiveId,
        destination_session_id: SessionId,
        reference: Reference,
    ) -> Self {
        Self {
            draft_id,
            objective_id,
            destination_session_id,
            reference,
            body: String::new(),
            stale: false,
            state: DraftState::Unsent,
            durable_revision: 0,
            durable_body: String::new(),
            edit_generation: 0,
            saved_generation: 0,
            in_flight: None,
            first_unsaved_edit: None,
            last_edit: None,
            failed: None,
            conflict: None,
            awaiting_reconcile: false,
        }
    }

    /// A draft restored from the service's durable state.
    pub fn from_durable(draft: &Draft) -> Self {
        let mut editor = Self::new_local(
            draft.draft_id.clone(),
            draft.objective_id.clone(),
            draft.destination_session_id.clone(),
            draft.reference.clone(),
        );
        editor.body = draft.body.clone();
        editor.durable_body = draft.body.clone();
        editor.durable_revision = draft.revision;
        editor.stale = draft.stale;
        editor.state = draft.state;
        editor
    }

    pub fn durable_revision(&self) -> u64 {
        self.durable_revision
    }

    fn dirty(&self) -> bool {
        let pending = self.in_flight.as_ref().map_or(0, |f| f.generation);
        self.edit_generation > self.saved_generation.max(pending)
    }

    /// Whether the latest local text is durably acknowledged.
    pub fn is_saved(&self) -> bool {
        !self.dirty()
            && self.in_flight.is_none()
            && self.conflict.is_none()
            && self.durable_revision > 0
    }

    /// Whether closing now would lose text the developer typed: the latest text is not durably
    /// acknowledged (unsaved, saving, failed, disconnected or in conflict). An untouched new draft
    /// has nothing to lose.
    pub fn has_unsaved_text(&self) -> bool {
        let untouched = self.durable_revision == 0 && self.body.is_empty();
        !untouched && (self.dirty() || self.in_flight.is_some() || self.conflict.is_some())
    }

    /// Makes a pending save due now (closing the window flushes instead of waiting for the quiet
    /// period). A failed save stays failed until an explicit Retry; a conflict or an outcome
    /// awaiting reconciliation is never overwritten.
    pub fn flush(&mut self, now: Millis) {
        if self.dirty() && self.failed.is_none() {
            self.last_edit = Some(now.saturating_sub(QUIET_MS));
        }
    }

    pub fn edit(&mut self, body: &str, now: Millis) {
        if self.body == body {
            return;
        }
        body.clone_into(&mut self.body);
        self.edit_generation += 1;
        self.last_edit = Some(now);
        self.first_unsaved_edit.get_or_insert(now);
    }

    /// When the next save should be attempted, if one is due.
    pub fn next_deadline(&self) -> Option<Millis> {
        if !self.dirty()
            || self.in_flight.is_some()
            || self.failed.is_some()
            || self.conflict.is_some()
            || self.awaiting_reconcile
        {
            return None;
        }
        let quiet = self.last_edit? + QUIET_MS;
        let max = self.first_unsaved_edit? + MAX_INTERVAL_MS;
        Some(quiet.min(max))
    }

    /// Returns a save request when one is due now and the service is reachable.
    pub fn poll(&mut self, now: Millis, connected: bool) -> Option<SaveRequest> {
        if !connected || now < self.next_deadline()? {
            return None;
        }
        self.in_flight = Some(InFlight {
            generation: self.edit_generation,
            expected_revision: self.durable_revision,
            body: self.body.clone(),
        });
        self.first_unsaved_edit = None;
        Some(SaveRequest {
            objective_id: self.objective_id.clone(),
            draft_id: self.draft_id.clone(),
            expected_revision: self.durable_revision,
            reference: self.reference.clone(),
            body: self.body.clone(),
        })
    }

    /// Applies an authoritative draft (save reply, `draft.changed` event or `drafts.list`).
    pub fn acknowledge(&mut self, draft: &Draft) {
        self.stale = draft.stale;
        self.state = draft.state;
        self.reference = draft.reference.clone();
        self.awaiting_reconcile = false;
        if draft.revision <= self.durable_revision {
            return; // Duplicate or out-of-date acknowledgement.
        }
        self.durable_revision = draft.revision;
        self.durable_body = draft.body.clone();
        if draft.body == self.body {
            self.saved_generation = self.edit_generation;
            self.in_flight = None;
            self.conflict = None;
            self.first_unsaved_edit = None;
        } else if let Some(flight) = self
            .in_flight
            .take_if(|f| f.body == draft.body && draft.revision > f.expected_revision)
        {
            // Our request landed; newer local edits remain dirty and will be scheduled.
            self.saved_generation = flight.generation;
            self.first_unsaved_edit
                .get_or_insert(self.last_edit.unwrap_or(0));
        } else if self.dirty() || self.in_flight.is_some() {
            self.in_flight = None;
            self.conflict = Some(draft.revision);
        } else {
            draft.body.clone_into(&mut self.body);
            self.saved_generation = self.edit_generation;
        }
    }

    pub fn fail(&mut self, message: &str) {
        self.in_flight = None;
        self.failed = Some(message.to_owned());
    }

    /// The link dropped: an in-flight save's outcome is unknown until drafts are re-queried.
    pub fn connection_lost(&mut self) {
        // Keep the text dirty; a later acknowledgement with this body clears it. Until drafts are
        // re-queried, no new save is sent so a landed write is never blindly overwritten.
        if self.in_flight.take().is_some() {
            self.awaiting_reconcile = true;
        }
    }

    /// Drafts were re-queried after reconnect and this draft's durable revision is unchanged.
    pub fn reconciled(&mut self) {
        self.awaiting_reconcile = false;
    }

    pub fn retry(&mut self, now: Millis) {
        self.failed = None;
        self.last_edit = Some(now.saturating_sub(QUIET_MS));
        self.first_unsaved_edit = Some(now.saturating_sub(MAX_INTERVAL_MS));
    }

    /// Resolves a conflict: keep local text (save against the newer revision) or adopt the saved one.
    pub fn resolve_conflict(&mut self, keep_local: bool, now: Millis) {
        if self.conflict.take().is_none() {
            return;
        }
        if keep_local {
            self.retry(now);
        } else {
            self.durable_body.clone_into(&mut self.body);
            self.edit_generation += 1;
            self.saved_generation = self.edit_generation;
        }
    }

    pub fn status(&self, connected: bool) -> SaveStatus {
        if let Some(revision) = self.conflict {
            return SaveStatus::Conflict { revision };
        }
        if let Some(message) = &self.failed {
            return SaveStatus::Failed {
                message: message.clone(),
            };
        }
        if self.in_flight.is_some() || self.dirty() {
            return match (connected, self.in_flight.is_some()) {
                (false, _) => SaveStatus::Disconnected,
                (true, true) => SaveStatus::Saving,
                (true, false) => SaveStatus::Unsaved,
            };
        }
        if self.durable_revision == 0 {
            SaveStatus::Empty
        } else {
            SaveStatus::Saved {
                revision: self.durable_revision,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation::model::{ArtifactDigest, CaptureId, ReferenceVersion, RepoPath, Side};

    fn reference() -> Reference {
        Reference {
            artifact_digest: ArtifactDigest::new("digest"),
            path: RepoPath::utf8("src/a.rs"),
            version: ReferenceVersion::Working {
                capture_id: CaptureId::new("cap"),
            },
            side: Side::New,
            byte_start: 0,
            byte_end: 3,
            selected_text: "abc".into(),
            line_hint: Some(1),
        }
    }

    fn editor() -> DraftEditor {
        DraftEditor::new_local(
            DraftId::new("d1"),
            ObjectiveId::new("o1"),
            SessionId::new("s1"),
            reference(),
        )
    }

    fn durable(body: &str, revision: u64) -> Draft {
        Draft {
            draft_id: DraftId::new("d1"),
            objective_id: ObjectiveId::new("o1"),
            destination_session_id: SessionId::new("s1"),
            reference: reference(),
            body: body.into(),
            revision,
            state: DraftState::Unsent,
            stale: false,
            input_id: None,
        }
    }

    #[test]
    fn saves_after_quiet_period() {
        let mut e = editor();
        assert_eq!(e.status(true), SaveStatus::Empty);
        e.edit("h", 1_000);
        assert_eq!(e.status(true), SaveStatus::Unsaved);
        assert_eq!(e.next_deadline(), Some(1_300));
        assert!(e.poll(1_299, true).is_none());
        let request = e.poll(1_300, true).expect("due");
        assert_eq!(request.expected_revision, 0);
        assert_eq!(request.body, "h");
        assert_eq!(e.status(true), SaveStatus::Saving);
        assert!(e.poll(5_000, true).is_none(), "one save in flight");
        e.acknowledge(&durable("h", 1));
        assert_eq!(e.status(true), SaveStatus::Saved { revision: 1 });
    }

    #[test]
    fn continuous_typing_saves_at_least_every_two_seconds() {
        let mut e = editor();
        let mut saved_at = Vec::new();
        let mut now = 0;
        let mut text = String::new();
        while now <= 4_100 {
            text.push('x');
            e.edit(&text, now);
            if let Some(request) = e.poll(now, true) {
                saved_at.push(now);
                let revision = e.durable_revision() + 1;
                e.acknowledge(&durable(&request.body, revision));
            }
            now += 100; // typing faster than the quiet period
        }
        assert_eq!(saved_at, vec![2_000, 4_100]);
    }

    #[test]
    fn saved_only_when_ack_matches_latest_text() {
        let mut e = editor();
        e.edit("first", 0);
        let request = e.poll(300, true).unwrap();
        e.edit("first and more", 350);
        e.acknowledge(&durable(&request.body, 1));
        assert_eq!(e.status(true), SaveStatus::Unsaved);
        let second = e.poll(650, true).unwrap();
        assert_eq!(second.expected_revision, 1);
        // A duplicate stale acknowledgement must not mark the newer text saved.
        e.acknowledge(&durable("first", 1));
        assert_eq!(e.status(true), SaveStatus::Saving);
        e.acknowledge(&durable("first and more", 2));
        assert_eq!(e.status(true), SaveStatus::Saved { revision: 2 });
    }

    #[test]
    fn failure_waits_for_retry() {
        let mut e = editor();
        e.edit("x", 0);
        e.poll(300, true).unwrap();
        e.fail("storage full");
        assert_eq!(
            e.status(true),
            SaveStatus::Failed {
                message: "storage full".into()
            }
        );
        assert!(e.poll(10_000, true).is_none());
        e.retry(10_000);
        assert!(e.poll(10_000, true).is_some());
    }

    #[test]
    fn disconnected_edits_are_not_saved_and_reconcile_on_reconnect() {
        let mut e = editor();
        e.edit("x", 0);
        assert!(e.poll(300, false).is_none());
        assert_eq!(e.status(false), SaveStatus::Disconnected);
        let request = e.poll(400, true).unwrap();
        e.connection_lost();
        assert_eq!(e.status(false), SaveStatus::Disconnected);
        assert!(
            e.poll(5_000, true).is_none(),
            "no blind resend before reconciliation"
        );
        // drafts.list after reconnect shows the save did land.
        e.acknowledge(&durable(&request.body, 1));
        assert_eq!(e.status(true), SaveStatus::Saved { revision: 1 });
    }

    #[test]
    fn newer_foreign_revision_is_a_conflict_not_an_overwrite() {
        let mut e = DraftEditor::from_durable(&durable("saved", 3));
        e.edit("mine", 0);
        e.acknowledge(&durable("theirs", 4));
        assert_eq!(e.status(true), SaveStatus::Conflict { revision: 4 });
        assert!(e.poll(5_000, true).is_none());
        e.resolve_conflict(true, 5_000);
        let request = e.poll(5_000, true).unwrap();
        assert_eq!(request.expected_revision, 4);
        assert_eq!(request.body, "mine");
    }

    #[test]
    fn flush_saves_without_waiting_and_unsaved_text_is_reported_until_acknowledged() {
        let mut e = editor();
        assert!(!e.has_unsaved_text(), "an untouched draft has nothing to lose");
        e.edit("close soon", 1_000);
        assert!(e.has_unsaved_text());
        assert!(e.poll(1_010, true).is_none(), "quiet period not over");
        e.flush(1_010);
        let request = e.poll(1_010, true).expect("flushed save is due now");
        assert!(e.has_unsaved_text(), "in flight is not saved");
        e.acknowledge(&durable(&request.body, 1));
        assert!(!e.has_unsaved_text());
        // A failed save is not retried by a flush; it stays visibly unsaved.
        e.edit("close soon!", 2_000);
        e.flush(2_000);
        e.poll(2_000, true).unwrap();
        e.fail("durability_failure");
        e.flush(2_100);
        assert!(e.poll(2_100, true).is_none());
        assert!(e.has_unsaved_text());
    }

    #[test]
    fn clean_editor_adopts_newer_revision() {
        let mut e = DraftEditor::from_durable(&durable("saved", 3));
        e.acknowledge(&durable("updated", 4));
        assert_eq!(e.body, "updated");
        assert_eq!(e.status(true), SaveStatus::Saved { revision: 4 });
    }
}
