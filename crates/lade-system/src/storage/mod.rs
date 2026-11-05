pub mod backup;
mod preferences;
mod reference;
mod rows;
mod schema;

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::thread;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, Transaction};

use schema::{MIGRATIONS, latest};

use crate::support::now_ms;
pub use preferences::{ApprovalRow, load_approvals, load_thinking, save_approval, save_thinking};
pub use rows::{
    ActivityRow, CaptureRow, JournalRow, JournalState, ObjectiveRow, OperationKey, OperationRow,
    OperationState, PendingOperation, WorkspaceRow, activity_page, capture_retention_refs,
    delete_draft, insert_capture, insert_input, insert_journal, insert_workspace, latest_captures,
    load_activity_item, load_capture, load_drafts, load_inputs, load_objectives, load_operation,
    load_review, load_settings, load_verification_tails, load_workspaces, max_activity_seq,
    open_journal, pending_operations, pi_session_file, record_operation, record_pi_session,
    save_delivery, save_draft, save_objective, save_review, save_session, save_settings,
    set_verification_tail, update_journal, upsert_activity, workspace_by_path,
};
pub use schema::SCHEMA_VERSION;

pub const LOCK_FILE: &str = "lade.lock";
/// How many times `lock_state` tries the state lock, [`LOCK_RETRY`] apart. `flock` has no
/// timed form, so a bounded wait for a predecessor's exit retries the non-blocking lock.
const LOCK_ATTEMPTS: u32 = 40;
const LOCK_RETRY: Duration = Duration::from_millis(50);

/// Takes the exclusive lock that marks the state directory as owned by one service. Retries
/// for about two seconds so a restarting service can outlast its predecessor's exit.
///
/// # Errors
///
/// Returns the error from creating or opening the lock file, or `ErrorKind::WouldBlock` when
/// another process still holds the lock after the last attempt.
pub fn lock_state(state: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(state.join(LOCK_FILE))?;
    for _ in 0..LOCK_ATTEMPTS {
        if rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive).is_ok() {
            return Ok(file);
        }
        thread::sleep(LOCK_RETRY);
    }
    Err(io::ErrorKind::WouldBlock.into())
}

#[derive(Debug)]
pub enum StoreError {
    Open(String),
    Corrupt(String),
    NewerSchema(i64),
    Sql(String),
    Backup(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open(e) => write!(f, "database cannot be opened: {e}"),
            Self::Corrupt(e) => write!(f, "database failed its integrity check: {e}"),
            Self::NewerSchema(v) => write!(f, "database schema {v} is newer than this service"),
            Self::Sql(e) => write!(f, "database error: {e}"),
            Self::Backup(e) => write!(
                f,
                "pre-migration backup failed ({e}); the database was not migrated"
            ),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error.to_string())
    }
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (creating it with mode 0600 if missing) and migrates the store at `path`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Open`] when the file cannot be created or opened or WAL mode and foreign
    /// keys are unavailable, [`StoreError::Corrupt`] when it fails its integrity check,
    /// [`StoreError::NewerSchema`] when a newer service migrated it, [`StoreError::Backup`] when the
    /// pre-migration backup fails, and [`StoreError::Sql`] when a migration fails.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::open_with(path, MIGRATIONS)
    }

    /// Opens the store migrated only up to schema `version`, as an older build would.
    ///
    /// # Errors
    ///
    /// Fails as [`Store::open`] does.
    #[doc(hidden)]
    pub fn open_as_schema(path: &Path, version: i64) -> Result<Self, StoreError> {
        let known = MIGRATIONS.iter().take_while(|(v, _)| *v <= version).count();
        Self::open_with(path, &MIGRATIONS[..known])
    }

    fn open_with(path: &Path, migrations: &[(i64, &str)]) -> Result<Self, StoreError> {
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
        let state = path.parent().unwrap_or(Path::new("."));
        store.migrate(state, migrations)?;
        Ok(store)
    }

    fn migrate(&mut self, state: &Path, migrations: &[(i64, &str)]) -> Result<(), StoreError> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL)",
        )?;
        let current: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        let latest = latest(migrations);
        if current > latest {
            return Err(StoreError::NewerSchema(current));
        }
        if current == latest {
            return Ok(());
        }
        if current > 0 {
            backup::before_migration(&self.conn, state, current, latest)
                .map_err(|e| StoreError::Backup(e.to_string()))?;
        }
        for (version, sql) in migrations.iter().filter(|(v, _)| *v > current) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                rusqlite::params![version, now_ms()],
            )?;
            tx.commit()?;
        }
        if current > 0
            && let Err(e) = backup::record_applied(state, current, latest)
        {
            eprintln!("laded: could not journal the applied migration: {e}");
        }
        Ok(())
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Starts a transaction on the store's connection.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error when the transaction cannot begin.
    pub fn begin(&mut self) -> rusqlite::Result<Transaction<'_>> {
        self.conn.transaction()
    }

    /// The highest schema migration applied to the store.
    ///
    /// # Errors
    ///
    /// Returns the SQLite error when the query fails.
    pub fn schema_version(&self) -> rusqlite::Result<i64> {
        self.conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
    }
}

/// A fully migrated in-memory connection with foreign keys on, for row-level unit tests.
#[cfg(test)]
fn memory() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory store");
    conn.pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    for (_, sql) in MIGRATIONS {
        conn.execute_batch(sql).expect("migration");
    }
    conn
}

#[cfg(test)]
mod tests;
