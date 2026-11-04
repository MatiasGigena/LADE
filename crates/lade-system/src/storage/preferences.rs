//! Per-Objective session preferences (the chosen thinking level) and the developer's
//! approve/reject decisions for executable profile extensions.

use lade_core::domain::ids::{ObjectiveId, Uuid};
use lade_protocol::types::ThinkingLevel;
use rusqlite::{Connection, params};

type Sql<T> = rusqlite::Result<T>;

fn corrupt(what: &'static str) -> rusqlite::Error {
    rusqlite::Error::InvalidColumnType(0, what.to_string(), rusqlite::types::Type::Text)
}

pub(super) const SCHEMA_V2: &str = r"
CREATE TABLE session_preferences (
    objective_id TEXT PRIMARY KEY REFERENCES objectives(id),
    thinking TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE resource_approvals (
    name TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    decision TEXT NOT NULL CHECK (decision IN ('approved', 'rejected')),
    decided_at INTEGER NOT NULL,
    PRIMARY KEY (name, sha256)
);
";

/// Records the thinking level chosen for `objective`.
///
/// # Errors
///
/// Returns the SQLite error when the write fails, for example when the Objective is not recorded.
pub fn save_thinking(
    conn: &Connection,
    objective: ObjectiveId,
    level: ThinkingLevel,
    now: i64,
) -> Sql<()> {
    conn.execute(
        "INSERT INTO session_preferences (objective_id, thinking, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(objective_id) DO UPDATE SET thinking = excluded.thinking,
            updated_at = excluded.updated_at",
        params![objective.to_string(), level.as_str(), now],
    )?;
    Ok(())
}

/// The thinking level recorded for each Objective.
///
/// # Errors
///
/// Returns the SQLite error when the query fails, or `InvalidColumnType` when a stored id or level
/// no longer parses.
pub fn load_thinking(conn: &Connection) -> Sql<Vec<(ObjectiveId, ThinkingLevel)>> {
    let mut stmt = conn.prepare("SELECT objective_id, thinking FROM session_preferences")?;
    let rows = stmt.query_map([], |row| {
        let id = Uuid::parse(&row.get::<_, String>(0)?).map_err(|_| corrupt("uuid"))?;
        let level = ThinkingLevel::parse(&row.get::<_, String>(1)?)
            .ok_or_else(|| corrupt("thinking level"))?;
        Ok((ObjectiveId::new(id), level))
    })?;
    rows.collect()
}

/// One extension decision, keyed by the extension's name and the SHA-256 of its content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalRow {
    pub name: String,
    pub sha256: String,
    pub approved: bool,
}

/// Records the developer's decision for one extension version.
///
/// # Errors
///
/// Returns the SQLite error when the write fails.
pub fn save_approval(conn: &Connection, row: &ApprovalRow, now: i64) -> Sql<()> {
    conn.execute(
        "INSERT INTO resource_approvals (name, sha256, decision, decided_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(name, sha256) DO UPDATE SET decision = excluded.decision,
            decided_at = excluded.decided_at",
        params![
            row.name,
            row.sha256,
            if row.approved { "approved" } else { "rejected" },
            now
        ],
    )?;
    Ok(())
}

/// Every recorded extension decision.
///
/// # Errors
///
/// Returns the SQLite error when the query fails, or `InvalidColumnType` when a stored decision is
/// neither `approved` nor `rejected`.
pub fn load_approvals(conn: &Connection) -> Sql<Vec<ApprovalRow>> {
    let mut stmt = conn.prepare("SELECT name, sha256, decision FROM resource_approvals")?;
    let rows = stmt.query_map([], |row| {
        let approved = match row.get::<_, String>(2)?.as_str() {
            "approved" => true,
            "rejected" => false,
            _ => return Err(corrupt("approval decision")),
        };
        Ok(ApprovalRow {
            name: row.get(0)?,
            sha256: row.get(1)?,
            approved,
        })
    })?;
    rows.collect()
}
