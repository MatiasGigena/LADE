//! Settings and the open Workspaces.

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use lade_core::domain::ids::WorkspaceId;
use lade_protocol::client as wire;
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{Sql, corrupt, uuid};

/// The saved Settings, or `None` before any were saved.
///
/// # Errors
///
/// Returns the SQLite error when the query fails, or `InvalidColumnType` when the stored JSON no
/// longer deserializes into Settings.
pub fn load_settings(conn: &Connection) -> Sql<Option<wire::Settings>> {
    let json: Option<String> = conn
        .query_row("SELECT json FROM settings WHERE id = 1", [], |row| {
            row.get(0)
        })
        .optional()?;
    json.map(|text| serde_json::from_str(&text).map_err(|_| corrupt("settings")))
        .transpose()
}

/// Replaces the saved Settings.
///
/// # Errors
///
/// Returns the SQLite error when the write fails.
pub fn save_settings(conn: &Connection, settings: &wire::Settings) -> Sql<()> {
    let text = serde_json::to_string(settings).map_err(|_| corrupt("settings"))?;
    conn.execute(
        "INSERT INTO settings (id, json) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET json = excluded.json",
        params![text],
    )?;
    Ok(())
}

#[derive(Clone, Debug)]
pub struct WorkspaceRow {
    pub id: WorkspaceId,
    pub path: PathBuf,
    pub name: String,
    pub default_branch: Option<String>,
    pub opened_at: i64,
}

/// Records a newly opened Workspace.
///
/// # Errors
///
/// Returns the SQLite error when the insert fails, including the uniqueness violation for an id
/// or path that is already recorded.
pub fn insert_workspace(conn: &Connection, row: &WorkspaceRow) -> Sql<()> {
    conn.execute(
        "INSERT INTO workspaces (id, path, name, default_branch, opened_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            row.id.to_string(),
            row.path.as_os_str().as_bytes(),
            row.name,
            row.default_branch,
            row.opened_at
        ],
    )?;
    Ok(())
}

fn workspace_from_row(row: &Row<'_>) -> Sql<WorkspaceRow> {
    let path: Vec<u8> = row.get(1)?;
    Ok(WorkspaceRow {
        id: WorkspaceId::new(uuid(&row.get::<_, String>(0)?)?),
        path: PathBuf::from(OsString::from_vec(path)),
        name: row.get(2)?,
        default_branch: row.get(3)?,
        opened_at: row.get(4)?,
    })
}

/// Every recorded Workspace, oldest first.
///
/// # Errors
///
/// Returns the SQLite error when the query fails, or `InvalidColumnType` when a stored id is not a
/// UUID.
pub fn load_workspaces(conn: &Connection) -> Sql<Vec<WorkspaceRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, name, default_branch, opened_at FROM workspaces ORDER BY opened_at, id",
    )?;
    let rows = stmt.query_map([], workspace_from_row)?;
    rows.collect()
}

/// The Workspace recorded for `path`, if any.
///
/// # Errors
///
/// Returns the SQLite error when the query fails, or `InvalidColumnType` when the stored id is not
/// a UUID.
pub fn workspace_by_path(conn: &Connection, path: &Path) -> Sql<Option<WorkspaceRow>> {
    conn.query_row(
        "SELECT id, path, name, default_branch, opened_at FROM workspaces WHERE path = ?1",
        params![path.as_os_str().as_bytes()],
        workspace_from_row,
    )
    .optional()
}
