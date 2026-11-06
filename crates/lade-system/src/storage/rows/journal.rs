//! The write-ahead integration journal.

use lade_core::domain::ids::{ApprovalId, CandidateId, GitId, ObjectiveId, Uuid};
use rusqlite::{Connection, params};

use super::{Sql, corrupt, git, uuid};

/// Progress of one integration in the write-ahead integration journal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalState {
    /// Recorded before the squash ran; restart reconciliation resolves it.
    Intent,
    Done,
    Failed,
    ReconciledDone,
    ReconciledAborted,
}

impl JournalState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::ReconciledDone => "reconciled_done",
            Self::ReconciledAborted => "reconciled_aborted",
        }
    }

    pub(super) fn parse(code: &str) -> Sql<Self> {
        Ok(match code {
            "intent" => Self::Intent,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "reconciled_done" => Self::ReconciledDone,
            "reconciled_aborted" => Self::ReconciledAborted,
            _ => return Err(corrupt("journal state")),
        })
    }
}

#[derive(Clone, Debug)]
pub struct JournalRow {
    pub id: Uuid,
    pub objective_id: ObjectiveId,
    pub approval_id: ApprovalId,
    pub candidate_id: CandidateId,
    pub target_branch: String,
    pub expected_target: GitId,
    pub candidate_tree: GitId,
    pub state: JournalState,
    pub result_commit: Option<GitId>,
    /// The squash commit message and time, so reconciliation can rebuild the exact commit.
    pub message: String,
    pub commit_time: i64,
}

/// Records an integration before its squash runs.
///
/// # Errors
///
/// Returns the SQLite error when the insert fails, for example when the Objective is not recorded.
pub fn insert_journal(conn: &Connection, row: &JournalRow, now: i64) -> Sql<()> {
    conn.execute(
        "INSERT INTO integration_journal (id, objective_id, approval_id, candidate_id,
            target_branch, expected_target, candidate_tree, state, created_at, updated_at,
            message, commit_time)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10, ?11)",
        params![
            row.id.to_string(),
            row.objective_id.to_string(),
            row.approval_id.to_string(),
            row.candidate_id.to_string(),
            row.target_branch,
            row.expected_target.hex(),
            row.candidate_tree.hex(),
            row.state.as_str(),
            now,
            row.message,
            row.commit_time
        ],
    )?;
    Ok(())
}

/// Records how integration `id` ended.
///
/// # Errors
///
/// Returns the SQLite error when the update fails.
pub fn update_journal(
    conn: &Connection,
    id: Uuid,
    state: JournalState,
    result: Option<&GitId>,
    detail: Option<&str>,
    now: i64,
) -> Sql<()> {
    conn.execute(
        "UPDATE integration_journal SET state = ?2, result_commit = ?3, detail = ?4,
            updated_at = ?5 WHERE id = ?1",
        params![
            id.to_string(),
            state.as_str(),
            result.map(GitId::hex),
            detail,
            now
        ],
    )?;
    Ok(())
}

/// Every integration still in the `intent` state, oldest first, for restart
/// reconciliation.
///
/// # Errors
///
/// Returns the SQLite error when the query fails, or `InvalidColumnType` naming the field when a
/// stored id or state no longer parses.
pub fn open_journal(conn: &Connection) -> Sql<Vec<JournalRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, objective_id, approval_id, candidate_id, target_branch, expected_target,
            candidate_tree, state, result_commit, message, commit_time
         FROM integration_journal WHERE state = 'intent' ORDER BY created_at",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(JournalRow {
            id: uuid(&row.get::<_, String>(0)?)?,
            objective_id: ObjectiveId::new(uuid(&row.get::<_, String>(1)?)?),
            approval_id: ApprovalId::new(uuid(&row.get::<_, String>(2)?)?),
            candidate_id: CandidateId::new(uuid(&row.get::<_, String>(3)?)?),
            target_branch: row.get(4)?,
            expected_target: git(&row.get::<_, String>(5)?)?,
            candidate_tree: git(&row.get::<_, String>(6)?)?,
            state: JournalState::parse(&row.get::<_, String>(7)?)?,
            result_commit: row
                .get::<_, Option<String>>(8)?
                .map(|hex| git(&hex))
                .transpose()?,
            message: row.get(9)?,
            commit_time: row.get(10)?,
        })
    })?;
    rows.collect()
}
