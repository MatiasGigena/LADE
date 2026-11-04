//! Typed rows of the service's SQLite store, one submodule per aggregate. Every loader
//! rebuilds domain values through their validating constructors, so a stored value that no
//! longer parses surfaces as a [`rusqlite::Error::InvalidColumnType`] naming the bad field
//! instead of an invalid domain object.

mod activity;
mod capture;
mod feedback;
mod journal;
mod objective;
mod operation;
mod review;
mod workspace;

use lade_core::Revision;
use lade_core::domain::ids::{GitAlgorithm, GitId, Timestamp, Uuid};

pub use activity::{
    ActivityRow, activity_page, load_activity_item, max_activity_seq, upsert_activity,
};
pub use capture::{
    CaptureRow, capture_retention_refs, insert_capture, latest_captures, load_capture,
};
pub use feedback::{
    delete_draft, insert_input, load_drafts, load_inputs, save_delivery, save_draft,
};
pub use journal::{JournalRow, JournalState, insert_journal, open_journal, update_journal};
pub use objective::{
    ObjectiveRow, load_objectives, pi_session_file, record_pi_session, save_objective, save_session,
};
pub use operation::{
    OperationKey, OperationRow, OperationState, PendingOperation, load_operation,
    pending_operations, record_operation,
};
pub use review::{load_review, load_verification_tails, save_review, set_verification_tail};
pub use workspace::{
    WorkspaceRow, insert_workspace, load_settings, load_workspaces, save_settings,
    workspace_by_path,
};

pub(super) type Sql<T> = rusqlite::Result<T>;

/// The error a loader reports when the stored value for `what` no longer parses.
pub(super) fn corrupt(what: &'static str) -> rusqlite::Error {
    rusqlite::Error::InvalidColumnType(0, what.to_string(), rusqlite::types::Type::Null)
}

fn uuid(text: &str) -> Sql<Uuid> {
    Uuid::parse(text).map_err(|_| corrupt("uuid"))
}

/// Git ids are stored as bare hex; the length tells SHA-1 (40) from SHA-256 (64).
fn git(text: &str) -> Sql<GitId> {
    let algorithm = if text.len() == 40 {
        GitAlgorithm::Sha1
    } else {
        GitAlgorithm::Sha256
    };
    GitId::new(algorithm, text).map_err(|_| corrupt("git id"))
}

fn count(value: i64, what: &'static str) -> Sql<u64> {
    u64::try_from(value).map_err(|_| corrupt(what))
}

fn rev(value: i64) -> Sql<Revision> {
    u64::try_from(value)
        .map(Revision::new)
        .map_err(|_| corrupt("revision"))
}

fn u(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn ts(value: i64) -> Timestamp {
    Timestamp::from_unix_ms(value)
}
