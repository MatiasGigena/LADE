//! Product store: one SQLite database (`$STATE/lade.db`) owned by the service thread.
//!
//! WAL journal, `synchronous=FULL`, foreign keys on, versioned migrations recorded in
//! `schema_migrations`. There is exactly one connection; every write happens on the service thread
//! in a short transaction (never across Git, worker or provider work). An unopenable, corrupt or
//! newer-than-known database refuses to start; it is never reset. No secret (capability,
//! credential) is ever stored.

mod rows;

use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, Transaction};

pub use rows::*;

/// Ordered schema migrations. Never edit an applied entry; append a new one.
const MIGRATIONS: &[(i64, &str)] = &[(1, SCHEMA_V1)];

const SCHEMA_V1: &str = r"
CREATE TABLE settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    json TEXT NOT NULL
);
CREATE TABLE workspaces (
    id TEXT PRIMARY KEY,
    path BLOB NOT NULL UNIQUE,
    name TEXT NOT NULL,
    default_branch TEXT,
    opened_at INTEGER NOT NULL
);
CREATE TABLE objectives (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    title TEXT NOT NULL,
    outcome TEXT NOT NULL,
    constraints_text TEXT NOT NULL,
    acceptance TEXT NOT NULL,
    intent_revision INTEGER NOT NULL,
    integration_target TEXT NOT NULL,
    start_algorithm TEXT NOT NULL,
    start_commit TEXT NOT NULL,
    branch TEXT NOT NULL,
    lifecycle TEXT NOT NULL,
    exec_state TEXT NOT NULL,
    exec_interrupted INTEGER NOT NULL,
    exec_reason TEXT,
    exec_generation INTEGER NOT NULL,
    session_id TEXT,
    association_revision INTEGER,
    model_provider TEXT,
    model_id TEXT,
    cleaned_up INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    revision INTEGER NOT NULL,
    git_branch TEXT,
    worktree_path BLOB,
    integration_conflict INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE sessions (
    session_id TEXT PRIMARY KEY,
    objective_id TEXT NOT NULL REFERENCES objectives(id),
    association_revision INTEGER NOT NULL,
    model_provider TEXT NOT NULL,
    model_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    pi_session_id TEXT,
    session_file TEXT,
    last_generation INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE captures (
    id TEXT PRIMARY KEY,
    objective_id TEXT NOT NULL REFERENCES objectives(id),
    tree TEXT NOT NULL,
    target_commit TEXT NOT NULL,
    files INTEGER NOT NULL,
    added INTEGER NOT NULL,
    removed INTEGER NOT NULL,
    retention_ref TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX captures_by_objective ON captures(objective_id, created_at);
CREATE TABLE drafts (
    id TEXT PRIMARY KEY,
    objective_id TEXT NOT NULL REFERENCES objectives(id),
    destination_session_id TEXT NOT NULL,
    reference TEXT NOT NULL,
    body TEXT NOT NULL,
    revision INTEGER NOT NULL,
    state TEXT NOT NULL,
    input_id TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    deleted INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE draft_revisions (
    draft_id TEXT NOT NULL REFERENCES drafts(id),
    revision INTEGER NOT NULL,
    reference TEXT NOT NULL,
    body TEXT NOT NULL,
    saved_at INTEGER NOT NULL,
    PRIMARY KEY (draft_id, revision)
);
CREATE TABLE accepted_inputs (
    id TEXT PRIMARY KEY,
    operation_id TEXT NOT NULL UNIQUE,
    objective_id TEXT NOT NULL REFERENCES objectives(id),
    session_id TEXT NOT NULL,
    association_revision INTEGER NOT NULL,
    mode TEXT NOT NULL,
    items TEXT NOT NULL,
    note TEXT,
    rendered_text TEXT NOT NULL,
    accepted_at INTEGER NOT NULL,
    seq INTEGER NOT NULL
);
CREATE TRIGGER accepted_inputs_immutable BEFORE UPDATE ON accepted_inputs
BEGIN SELECT RAISE(ABORT, 'accepted input is immutable'); END;
CREATE TRIGGER accepted_inputs_undeletable BEFORE DELETE ON accepted_inputs
BEGIN SELECT RAISE(ABORT, 'accepted input is immutable'); END;
CREATE TABLE input_delivery (
    input_id TEXT PRIMARY KEY REFERENCES accepted_inputs(id),
    state TEXT NOT NULL,
    generation INTEGER,
    blocked_reason TEXT,
    updated_at INTEGER NOT NULL
);
CREATE TABLE operations (
    operation_id TEXT PRIMARY KEY,
    digest TEXT NOT NULL,
    name TEXT NOT NULL,
    state TEXT NOT NULL,
    outcome TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE candidates (
    id TEXT PRIMARY KEY,
    objective_id TEXT NOT NULL REFERENCES objectives(id),
    ordinal INTEGER NOT NULL,
    commit_id TEXT NOT NULL,
    tree TEXT NOT NULL,
    base_target_commit TEXT NOT NULL,
    manifest_digest TEXT NOT NULL,
    files INTEGER NOT NULL,
    added INTEGER NOT NULL,
    removed INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    status TEXT NOT NULL,
    UNIQUE (objective_id, ordinal)
);
CREATE TABLE verifications (
    id TEXT PRIMARY KEY,
    objective_id TEXT NOT NULL REFERENCES objectives(id),
    candidate_id TEXT NOT NULL REFERENCES candidates(id),
    argv TEXT NOT NULL,
    cwd_label TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    ended_at INTEGER,
    exit_code INTEGER,
    outcome TEXT NOT NULL,
    output_artifact TEXT,
    output_tail TEXT NOT NULL DEFAULT '',
    truncated INTEGER NOT NULL,
    seq INTEGER NOT NULL
);
CREATE TABLE approvals (
    id TEXT PRIMARY KEY,
    objective_id TEXT NOT NULL REFERENCES objectives(id),
    candidate_id TEXT NOT NULL REFERENCES candidates(id),
    target_commit TEXT NOT NULL,
    override_reason TEXT,
    state TEXT NOT NULL,
    invalidation_reason TEXT,
    approved_at INTEGER NOT NULL,
    seq INTEGER NOT NULL
);
CREATE TABLE activity (
    item_id TEXT PRIMARY KEY,
    objective_id TEXT NOT NULL REFERENCES objectives(id),
    seq INTEGER NOT NULL,
    at INTEGER NOT NULL,
    role TEXT NOT NULL,
    title TEXT NOT NULL,
    text TEXT NOT NULL,
    tool_name TEXT,
    is_error INTEGER NOT NULL,
    truncated INTEGER NOT NULL,
    UNIQUE (objective_id, seq)
);
CREATE TABLE integration_journal (
    id TEXT PRIMARY KEY,
    objective_id TEXT NOT NULL REFERENCES objectives(id),
    approval_id TEXT NOT NULL,
    candidate_id TEXT NOT NULL,
    target_branch TEXT NOT NULL,
    expected_target TEXT NOT NULL,
    candidate_tree TEXT NOT NULL,
    message TEXT NOT NULL,
    commit_time INTEGER NOT NULL,
    state TEXT NOT NULL,
    result_commit TEXT,
    detail TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
";

/// Why the store could not be opened. Display is a short diagnostic without content.
#[derive(Debug)]
pub enum StoreError {
    Open(String),
    Corrupt(String),
    NewerSchema(i64),
    Sql(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open(e) => write!(f, "database cannot be opened: {e}"),
            Self::Corrupt(e) => write!(f, "database failed its integrity check: {e}"),
            Self::NewerSchema(v) => write!(f, "database schema {v} is newer than this service"),
            Self::Sql(e) => write!(f, "database error: {e}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error.to_string())
    }
}

/// The single product-store connection.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (creating 0600 if absent) and migrates the database. Never resets existing data.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if !path.exists() {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
                .map_err(|e| StoreError::Open(e.kind().to_string()))?;
        }
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| StoreError::Open(e.to_string()))?;
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|e| StoreError::Open(e.to_string()))?;
        let check: String = conn
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .map_err(|e| StoreError::Corrupt(e.to_string()))?;
        if check != "ok" {
            return Err(StoreError::Corrupt("quick_check failed".into()));
        }
        let mode: String = conn
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .map_err(|e| StoreError::Open(e.to_string()))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(StoreError::Open(format!("journal mode {mode} unavailable")));
        }
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let foreign: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
        if foreign != 1 {
            return Err(StoreError::Open("foreign keys unavailable".into()));
        }
        let mut store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<(), StoreError> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL)",
        )?;
        let current: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        let latest = MIGRATIONS.last().map_or(0, |(v, _)| *v);
        if current > latest {
            return Err(StoreError::NewerSchema(current));
        }
        for (version, sql) in MIGRATIONS.iter().filter(|(v, _)| *v > current) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                rusqlite::params![version, crate::service::convert::now_ms()],
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// A short write transaction; commit explicitly, drop rolls back.
    pub fn begin(&mut self) -> rusqlite::Result<Transaction<'_>> {
        self.conn.transaction()
    }

    /// Applied schema version (diagnostics and tests).
    pub fn schema_version(&self) -> rusqlite::Result<i64> {
        self.conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
    }
}
