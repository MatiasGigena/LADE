use lade_core::domain::ids::{ObjectiveId, SessionId, Uuid};
use rusqlite::{Connection, OptionalExtension, params};

type Sql<T> = rusqlite::Result<T>;

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

pub fn save_thinking(conn: &Connection, objective: ObjectiveId, level: &str, now: i64) -> Sql<()> {
    conn.execute(
        "INSERT INTO session_preferences (objective_id, thinking, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(objective_id) DO UPDATE SET thinking = excluded.thinking,
            updated_at = excluded.updated_at",
        params![objective.to_string(), level, now],
    )?;
    Ok(())
}

pub fn load_thinking(conn: &Connection) -> Sql<Vec<(ObjectiveId, String)>> {
    let mut stmt = conn.prepare("SELECT objective_id, thinking FROM session_preferences")?;
    let rows = stmt.query_map([], |row| {
        let id: String = row.get(0)?;
        let level: String = row.get(1)?;
        Ok((id, level))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, level) = row?;
        let uuid = Uuid::parse(&id).map_err(|_| {
            rusqlite::Error::InvalidColumnType(0, "uuid".into(), rusqlite::types::Type::Text)
        })?;
        out.push((ObjectiveId::new(uuid), level));
    }
    Ok(out)
}

pub fn save_approval(
    conn: &Connection,
    name: &str,
    sha256: &str,
    approve: bool,
    now: i64,
) -> Sql<()> {
    conn.execute(
        "INSERT INTO resource_approvals (name, sha256, decision, decided_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(name, sha256) DO UPDATE SET decision = excluded.decision,
            decided_at = excluded.decided_at",
        params![
            name,
            sha256,
            if approve { "approved" } else { "rejected" },
            now
        ],
    )?;
    Ok(())
}

pub fn load_approvals(conn: &Connection) -> Sql<Vec<(String, String, bool)>> {
    let mut stmt = conn.prepare("SELECT name, sha256, decision FROM resource_approvals")?;
    let rows = stmt.query_map([], |row| {
        let decision: String = row.get(2)?;
        Ok((row.get(0)?, row.get(1)?, decision == "approved"))
    })?;
    rows.collect()
}

pub fn pi_session_file(conn: &Connection, session: SessionId) -> Sql<Option<String>> {
    conn.query_row(
        "SELECT session_file FROM sessions WHERE session_id = ?1",
        params![session.to_string()],
        |row| row.get::<_, Option<String>>(0),
    )
    .optional()
    .map(Option::flatten)
}
