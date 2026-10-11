//! Pre-migration backups, the migration journal and the rollback restore (IMP-014).
//!
//! Before any schema migration of an existing database, [`before_migration`] writes a consistent
//! copy of it (`VACUUM INTO`) to `$STATE/backups/lade.db.schema-<from>.<unix ms>.bak` (0600, the
//! directory 0700) and records it in the append-only journal `$STATE/backups/migrations.journal`.
//! A migration never runs without its backup. At most [`KEEP_BACKUPS`] backups are kept; older
//! ones are deleted and the deletion is journaled.
//!
//! [`restore`] is the rollback path used by `scripts/install --rollback` (through
//! `laded store restore`): it puts back the newest intact backup of the requested schema under the
//! service's state lock. The newer database is never deleted: it is moved next to the backups as
//! `lade.db.newer-schema-<n>.<unix ms>` (with its `-wal`), which is never pruned.
//!
//! Journal lines are tab-separated: `<unix ms> backup <from> <to> <file> <sha256> <bundle>`,
//! `<unix ms> applied <from> <to>`, `<unix ms> pruned <file>`,
//! `<unix ms> restored <schema> <backup file> <kept newer file>`. File names never contain tabs.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use sha2::{Digest as _, Sha256};

use crate::service::convert::{hex, now_ms};

/// Directory (relative to the state directory) holding backups and the journal.
pub const BACKUP_DIR: &str = "backups";
/// Journal file name inside [`BACKUP_DIR`].
pub const JOURNAL: &str = "migrations.journal";
/// Pre-migration backups kept (newest first); older ones are deleted.
pub const KEEP_BACKUPS: usize = 3;
/// Database file name inside the state directory.
pub const DATABASE: &str = "lade.db";

/// One pre-migration backup recorded in the journal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Backup {
    pub taken_ms: i64,
    pub from: i64,
    pub to: i64,
    pub file: String,
    pub sha256: String,
}

/// Why a backup or restore failed. `Display` names no content.
#[derive(Debug)]
pub enum BackupError {
    Io(&'static str, io::ErrorKind),
    Sql(String),
    /// The service (or another restore) holds the state lock.
    Busy,
    /// No intact backup of the requested schema exists.
    NoBackup(i64),
    /// The database is not newer than the requested schema: nothing to restore.
    NotNeeded {
        database: i64,
        requested: i64,
    },
    /// The backup file exists but is not the recorded content or not the recorded schema.
    Invalid(String),
}

impl std::fmt::Display for BackupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(what, kind) => write!(f, "{what}: {kind}"),
            Self::Sql(e) => write!(f, "database error: {e}"),
            Self::Busy => f.write_str(
                "a LADE service is using the state directory; stop it first \
                 (systemctl --user stop lade.service)",
            ),
            Self::NoBackup(schema) => {
                write!(f, "no intact pre-migration backup of schema {schema}")
            }
            Self::NotNeeded {
                database,
                requested,
            } => write!(
                f,
                "the database is at schema {database}, not newer than {requested}; nothing to restore"
            ),
            Self::Invalid(e) => write!(f, "backup rejected: {e}"),
        }
    }
}

impl std::error::Error for BackupError {}

fn io_err(what: &'static str) -> impl Fn(io::Error) -> BackupError {
    move |e| BackupError::Io(what, e.kind())
}

fn sql_err(e: rusqlite::Error) -> BackupError {
    BackupError::Sql(e.to_string())
}

fn backup_dir(state: &Path) -> PathBuf {
    state.join(BACKUP_DIR)
}

fn private_backup_dir(state: &Path) -> Result<PathBuf, BackupError> {
    let dir = backup_dir(state);
    crate::transport::prepare_private_dir(&dir)
        .map_err(|e| BackupError::Invalid(format!("backup directory: {e}")))?;
    Ok(dir)
}

fn sync_dir(dir: &Path) -> Result<(), BackupError> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(io_err("sync backup directory"))
}

fn append_journal(dir: &Path, fields: &[&str]) -> Result<(), BackupError> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(dir.join(JOURNAL))
        .map_err(io_err("open migration journal"))?;
    let line = format!("{}\t{}\n", now_ms(), fields.join("\t"));
    file.write_all(line.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(io_err("write migration journal"))
}

fn sha256_file(path: &Path) -> Result<String, BackupError> {
    let mut file = File::open(path).map_err(io_err("read backup"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(io_err("read backup"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

/// Backups recorded in the journal of `state` whose file was not pruned, oldest first. A missing
/// journal is an empty list; unparseable lines are skipped.
pub fn backups(state: &Path) -> Result<Vec<Backup>, BackupError> {
    let journal = match fs::read_to_string(backup_dir(state).join(JOURNAL)) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(BackupError::Io("read migration journal", e.kind())),
    };
    let mut list: Vec<Backup> = Vec::new();
    for line in journal.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            [ts, "backup", from, to, file, sha, ..] => {
                let (Ok(taken_ms), Ok(from), Ok(to)) = (ts.parse(), from.parse(), to.parse())
                else {
                    continue;
                };
                list.push(Backup {
                    taken_ms,
                    from,
                    to,
                    file: (*file).to_owned(),
                    sha256: (*sha).to_owned(),
                });
            }
            [_, "pruned", file] => list.retain(|b| b.file != *file),
            _ => {}
        }
    }
    Ok(list)
}

/// Writes the pre-migration backup of the open database `conn` (at schema `from`, about to be
/// migrated to `to`) into `$STATE/backups`, journals it and prunes old backups. Fails closed: the
/// caller must not migrate when this fails.
pub(crate) fn before_migration(
    conn: &Connection,
    state: &Path,
    from: i64,
    to: i64,
) -> Result<PathBuf, BackupError> {
    let dir = private_backup_dir(state)?;
    let name = format!("{DATABASE}.schema-{from}.{}.bak", now_ms());
    let path = dir.join(&name);
    // VACUUM INTO needs a missing or empty target; creating it first fixes its mode at 0600.
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(io_err("create backup"))?;
    let written = conn
        .execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()])
        .map_err(sql_err)
        .and_then(|_| {
            File::open(&path)
                .and_then(|f| f.sync_all())
                .map_err(io_err("sync backup"))
        });
    if let Err(e) = written {
        let _ = fs::remove_file(&path);
        return Err(e);
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .map_err(io_err("chmod backup"))?;
    sync_dir(&dir)?;
    let sha = sha256_file(&path)?;
    append_journal(
        &dir,
        &[
            "backup",
            &from.to_string(),
            &to.to_string(),
            &name,
            &sha,
            lade_protocol::BUNDLE,
        ],
    )?;
    prune(state, &dir)?;
    Ok(path)
}

/// Records that the migration `from` → `to` committed.
pub(crate) fn record_applied(state: &Path, from: i64, to: i64) -> Result<(), BackupError> {
    let dir = private_backup_dir(state)?;
    append_journal(&dir, &["applied", &from.to_string(), &to.to_string()])
}

fn prune(state: &Path, dir: &Path) -> Result<(), BackupError> {
    let mut list = backups(state)?;
    list.sort_by_key(|b| b.taken_ms);
    let excess = list.len().saturating_sub(KEEP_BACKUPS);
    for old in &list[..excess] {
        match fs::remove_file(dir.join(&old.file)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(BackupError::Io("remove old backup", e.kind())),
        }
        append_journal(dir, &["pruned", &old.file])?;
    }
    Ok(())
}

/// Applied schema version of the database at `path` without migrating it (`None`: no database).
/// The database is opened read-write without creating anything, so it works on a WAL database
/// whose shared-memory file is absent; nothing is written.
pub fn database_schema(path: &Path) -> Result<Option<i64>, BackupError> {
    if !path.exists() {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sql_err)?;
    schema_of(&conn).map(Some)
}

fn schema_of(conn: &Connection) -> Result<i64, BackupError> {
    let has_table: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'",
            [],
            |row| row.get(0),
        )
        .map_err(sql_err)?;
    if has_table == 0 {
        return Ok(0);
    }
    conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )
    .map_err(sql_err)
}

/// What [`restore`] did.
#[derive(Debug)]
pub struct Restored {
    pub backup: Backup,
    pub database_schema: i64,
    /// Where the newer database was kept.
    pub kept: PathBuf,
}

/// Restores the newest intact backup of `schema` as `$STATE/lade.db`, keeping the newer database
/// under `$STATE/backups`. Holds the service's state lock for the whole operation, so it refuses
/// while a service runs.
pub fn restore(state: &Path, schema: i64) -> Result<Restored, BackupError> {
    let _lock = crate::service::lock_state(state).map_err(|_| BackupError::Busy)?;
    let db = state.join(DATABASE);
    let database_schema = database_schema(&db)?.ok_or(BackupError::Invalid(
        "no database in the state directory".into(),
    ))?;
    if database_schema <= schema {
        return Err(BackupError::NotNeeded {
            database: database_schema,
            requested: schema,
        });
    }
    let dir = private_backup_dir(state)?;
    let mut candidates: Vec<Backup> = backups(state)?
        .into_iter()
        .filter(|b| b.from == schema)
        .collect();
    candidates.sort_by_key(|b| std::cmp::Reverse(b.taken_ms));
    let backup = candidates
        .into_iter()
        .find(|b| sha256_file(&dir.join(&b.file)).is_ok_and(|sha| sha == b.sha256))
        .ok_or(BackupError::NoBackup(schema))?;
    let source = dir.join(&backup.file);
    {
        let conn = Connection::open_with_flags(&source, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(sql_err)?;
        let check: String = conn
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .map_err(sql_err)?;
        if check != "ok" {
            return Err(BackupError::Invalid("integrity check failed".into()));
        }
        let found = schema_of(&conn)?;
        if found != schema {
            return Err(BackupError::Invalid(format!("it holds schema {found}")));
        }
    }

    // Fold the WAL into the newer database so it is kept as one consistent file.
    {
        let conn =
            Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(sql_err)?;
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .map_err(sql_err)?;
    }
    // Stage the restored copy before touching the live database.
    let staged = state.join(format!("{DATABASE}.restore.tmp"));
    let _ = fs::remove_file(&staged);
    {
        let mut from = File::open(&source).map_err(io_err("read backup"))?;
        let mut to = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staged)
            .map_err(io_err("stage restored database"))?;
        io::copy(&mut from, &mut to)
            .and_then(|_| to.sync_all())
            .map_err(io_err("stage restored database"))?;
    }
    let kept = dir.join(format!(
        "{DATABASE}.newer-schema-{database_schema}.{}",
        now_ms()
    ));
    fs::rename(&db, &kept).map_err(io_err("keep the newer database"))?;
    for suffix in ["-wal", "-shm"] {
        let side = state.join(format!("{DATABASE}{suffix}"));
        let kept_side = PathBuf::from(format!("{}{suffix}", kept.display()));
        match fs::rename(&side, &kept_side) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(BackupError::Io("keep the newer database", e.kind())),
        }
    }
    sync_dir(&dir)?;
    fs::rename(&staged, &db).map_err(io_err("activate restored database"))?;
    sync_dir(state)?;
    let kept_name = kept
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    append_journal(
        &dir,
        &["restored", &schema.to_string(), &backup.file, &kept_name],
    )?;
    Ok(Restored {
        backup,
        database_schema,
        kept,
    })
}
