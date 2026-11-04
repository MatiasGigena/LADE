//! Captures of an Objective's working tree.

use lade_core::domain::ids::{CaptureId, GitId, ObjectiveId};
use lade_core::domain::review::ChangeCounts;
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{Sql, count, git, u, uuid};

#[derive(Clone, Debug)]
pub struct CaptureRow {
    pub id: CaptureId,
    pub objective_id: ObjectiveId,
    pub tree: GitId,
    pub target_commit: GitId,
    pub counts: ChangeCounts,
    pub retention_ref: String,
    pub created_at: i64,
}

/// Records a capture of an Objective's working tree.
///
/// # Errors
///
/// Returns the SQLite error when the insert fails, for example when the Objective is not recorded.
pub fn insert_capture(conn: &Connection, row: &CaptureRow) -> Sql<()> {
    conn.execute(
        "INSERT INTO captures (id, objective_id, tree, target_commit, files, added, removed,
            retention_ref, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            row.id.to_string(),
            row.objective_id.to_string(),
            row.tree.hex(),
            row.target_commit.hex(),
            row.counts.files,
            u(row.counts.added),
            u(row.counts.removed),
            row.retention_ref,
            row.created_at
        ],
    )?;
    Ok(())
}

fn capture_from_row(row: &Row<'_>) -> Sql<CaptureRow> {
    Ok(CaptureRow {
        id: CaptureId::new(uuid(&row.get::<_, String>(0)?)?),
        objective_id: ObjectiveId::new(uuid(&row.get::<_, String>(1)?)?),
        tree: git(&row.get::<_, String>(2)?)?,
        target_commit: git(&row.get::<_, String>(3)?)?,
        counts: ChangeCounts {
            files: row.get(4)?,
            added: count(row.get(5)?, "added")?,
            removed: count(row.get(6)?, "removed")?,
        },
        retention_ref: row.get(7)?,
        created_at: row.get(8)?,
    })
}

const CAPTURE_COLUMNS: &str =
    "id, objective_id, tree, target_commit, files, added, removed, retention_ref, created_at";

/// The capture with `id`, if recorded.
///
/// # Errors
///
/// Returns the SQLite error when the query fails, or `InvalidColumnType` when a stored id or count
/// no longer parses.
pub fn load_capture(conn: &Connection, id: CaptureId) -> Sql<Option<CaptureRow>> {
    conn.query_row(
        &format!("SELECT {CAPTURE_COLUMNS} FROM captures WHERE id = ?1"),
        params![id.to_string()],
        capture_from_row,
    )
    .optional()
}

/// The retention refs of every capture of `objective`, in recording order.
///
/// # Errors
///
/// Returns the SQLite error when the query fails.
pub fn capture_retention_refs(conn: &Connection, objective: ObjectiveId) -> Sql<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT retention_ref FROM captures WHERE objective_id = ?1 ORDER BY rowid")?;
    let rows = stmt.query_map(params![objective.to_string()], |row| row.get(0))?;
    rows.collect()
}

/// The most recent capture of each Objective.
///
/// # Errors
///
/// Returns the SQLite error when the query fails, or `InvalidColumnType` when a stored id or count
/// no longer parses.
pub fn latest_captures(conn: &Connection) -> Sql<Vec<CaptureRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {CAPTURE_COLUMNS} FROM captures c WHERE rowid = (
            SELECT rowid FROM captures WHERE objective_id = c.objective_id
            ORDER BY created_at DESC, rowid DESC LIMIT 1)"
    ))?;
    let rows = stmt.query_map([], capture_from_row)?;
    rows.collect()
}
