pub mod backup;
mod preferences;
mod reference;
mod rows;

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::thread;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, Transaction};

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

pub const LOCK_FILE: &str = "lade.lock";

/// Takes the exclusive lock that marks the state directory as owned by one service. Retries
/// for about two seconds so a restarting service can outlast its predecessor's exit; a lock
/// still held after that fails with `ErrorKind::WouldBlock`.
pub fn lock_state(state: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(state.join(LOCK_FILE))?;
    for _ in 0..40 {
        if rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive).is_ok() {
            return Ok(file);
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(io::ErrorKind::WouldBlock.into())
}

const MIGRATIONS: &[(i64, &str)] = &[(1, SCHEMA_V1), (2, preferences::SCHEMA_V2), (3, SCHEMA_V3)];

const SCHEMA_V3: &str = r"
ALTER TABLE objectives ADD COLUMN history_deleted INTEGER NOT NULL DEFAULT 0;
";

pub const SCHEMA_VERSION: i64 = latest(MIGRATIONS);

const fn latest(migrations: &[(i64, &str)]) -> i64 {
    match migrations.last() {
        Some((version, _)) => *version,
        None => 0,
    }
}

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
                rusqlite::params![version, crate::support::now_ms()],
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::path::PathBuf;

    use super::*;

    const V2: &str = "CREATE TABLE added_in_v2 (id INTEGER PRIMARY KEY);";
    const OLD: &[(i64, &str)] = &[(1, SCHEMA_V1)];
    const NEW: &[(i64, &str)] = &[(1, SCHEMA_V1), (2, V2)];

    fn temp_state(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lade-store-{name}-{}-{}",
            std::process::id(),
            crate::support::now_ms()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&dir)
            .expect("temp state");
        dir
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).expect("metadata").permissions().mode() & 0o777
    }

    fn workspaces(store: &Store) -> i64 {
        store
            .conn()
            .query_row("SELECT COUNT(*) FROM workspaces", [], |row| row.get(0))
            .expect("count")
    }

    fn add_workspace(store: &Store, id: &str) {
        store
            .conn()
            .execute(
                "INSERT INTO workspaces (id, path, name, opened_at) VALUES (?1, ?1, 'w', 0)",
                [id],
            )
            .expect("insert");
    }

    #[test]
    fn fresh_database_takes_no_backup() {
        let state = temp_state("fresh");
        Store::open_with(&state.join(backup::DATABASE), NEW).expect("open");
        assert!(backup::backups(&state).expect("journal").is_empty());
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn forward_migration_backs_up_and_rollback_restores_the_old_schema() {
        let state = temp_state("rollback");
        let db = state.join(backup::DATABASE);
        let old = Store::open_with(&db, OLD).expect("old version creates the store");
        add_workspace(&old, "before-upgrade");
        drop(old);

        let new = Store::open_with(&db, NEW).expect("new version migrates");
        assert_eq!(new.schema_version().expect("version"), 2);
        add_workspace(&new, "after-upgrade");
        drop(new);
        let list = backup::backups(&state).expect("journal");
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].from, list[0].to), (1, 2));
        let file = state.join(backup::BACKUP_DIR).join(&list[0].file);
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(&state.join(backup::BACKUP_DIR)), 0o700);
        assert_eq!(
            mode(&state.join(backup::BACKUP_DIR).join(backup::JOURNAL)),
            0o600
        );
        let journal = fs::read_to_string(state.join(backup::BACKUP_DIR).join(backup::JOURNAL))
            .expect("journal");
        assert!(journal.lines().any(|l| l.ends_with("\tapplied\t1\t2")));
        assert!(matches!(
            Store::open_with(&db, OLD),
            Err(StoreError::NewerSchema(2))
        ));

        let restored = backup::restore(&state, 1).expect("restore");
        assert_eq!(restored.database_schema, 2);
        assert!(restored.kept.exists(), "the newer database is kept");
        assert_eq!(mode(&db), 0o600);
        let old = Store::open_with(&db, OLD).expect("old version starts after restore");
        assert_eq!(old.schema_version().expect("version"), 1);
        assert_eq!(workspaces(&old), 1);
        drop(old);
        let kept = Connection::open(&restored.kept).expect("kept database opens");
        let kept_count: i64 = kept
            .query_row("SELECT COUNT(*) FROM workspaces", [], |row| row.get(0))
            .expect("count");
        assert_eq!(kept_count, 2, "post-upgrade data survives in the kept copy");
        assert!(matches!(
            backup::restore(&state, 1),
            Err(backup::BackupError::NotNeeded { .. })
        ));
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn real_migrations_back_up_from_v1_and_restore_lets_v1_start() {
        let state = temp_state("real");
        let db = state.join(backup::DATABASE);
        let v1 = Store::open_as_schema(&db, 1).expect("v1 store");
        assert_eq!(v1.schema_version().expect("version"), 1);
        add_workspace(&v1, "before-upgrade");
        drop(v1);
        let current = Store::open(&db).expect("current build migrates");
        assert_eq!(current.schema_version().expect("version"), SCHEMA_VERSION);
        const { assert!(SCHEMA_VERSION >= 3) };
        drop(current);
        let list = backup::backups(&state).expect("journal");
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].from, list[0].to), (1, SCHEMA_VERSION));
        assert!(matches!(
            Store::open_as_schema(&db, 1),
            Err(StoreError::NewerSchema(v)) if v == SCHEMA_VERSION
        ));
        backup::restore(&state, 1).expect("restore");
        let v1 = Store::open_as_schema(&db, 1).expect("v1 starts after restore");
        assert_eq!(v1.schema_version().expect("version"), 1);
        assert_eq!(workspaces(&v1), 1);
        drop(v1);
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn restore_refuses_a_missing_or_tampered_backup_and_a_running_service() {
        let state = temp_state("refuse");
        let db = state.join(backup::DATABASE);
        drop(Store::open_with(&db, OLD).expect("old"));
        drop(Store::open_with(&db, NEW).expect("new"));
        assert!(matches!(
            backup::restore(&state, 0),
            Err(backup::BackupError::NoBackup(0))
        ));
        let lock = lock_state(&state).expect("lock");
        assert!(matches!(
            backup::restore(&state, 1),
            Err(backup::BackupError::Busy)
        ));
        drop(lock);
        let file = &backup::backups(&state).expect("journal")[0].file;
        let mut bytes = fs::read(state.join(backup::BACKUP_DIR).join(file)).expect("read");
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        fs::write(state.join(backup::BACKUP_DIR).join(file), bytes).expect("tamper");
        assert!(matches!(
            backup::restore(&state, 1),
            Err(backup::BackupError::NoBackup(1))
        ));
        assert_eq!(backup::database_schema(&db).expect("schema"), Some(2));
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn backups_are_bounded() {
        let state = temp_state("bounded");
        let db = state.join(backup::DATABASE);
        drop(Store::open_with(&db, OLD).expect("old"));
        let mut migrations: Vec<(i64, String)> = vec![(1, SCHEMA_V1.to_owned())];
        for version in 2..=6 {
            migrations.push((version, format!("CREATE TABLE t{version} (id INTEGER);")));
            let list: Vec<(i64, &str)> = migrations.iter().map(|(v, s)| (*v, s.as_str())).collect();
            drop(Store::open_with(&db, &list).expect("migrate"));
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let list = backup::backups(&state).expect("journal");
        assert_eq!(list.len(), backup::KEEP_BACKUPS);
        assert_eq!(list.iter().map(|b| b.from).collect::<Vec<_>>(), [3, 4, 5]);
        let files = fs::read_dir(state.join(backup::BACKUP_DIR))
            .expect("dir")
            .filter(|e| {
                e.as_ref()
                    .is_ok_and(|e| e.file_name().to_string_lossy().ends_with(".bak"))
            })
            .count();
        assert_eq!(files, backup::KEEP_BACKUPS);
        let _ = fs::remove_dir_all(state);
    }
}
