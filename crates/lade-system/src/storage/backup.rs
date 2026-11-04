use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use sha2::{Digest as _, Sha256};

use crate::support::fs::ensure_private_dir;
use crate::support::{hex, now_ms};

pub const BACKUP_DIR: &str = "backups";
pub const JOURNAL: &str = "migrations.journal";
pub const KEEP_BACKUPS: usize = 3;
pub const DATABASE: &str = "lade.db";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Backup {
    pub taken_ms: i64,
    pub from: i64,
    pub to: i64,
    pub file: String,
    pub sha256: String,
}

#[derive(Debug)]
pub enum BackupError {
    Io(&'static str, io::ErrorKind),
    Sql(String),
    Busy,
    NoBackup(i64),
    NotNeeded { database: i64, requested: i64 },
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

impl From<rusqlite::Error> for BackupError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error.to_string())
    }
}

fn io_err(what: &'static str) -> impl Fn(io::Error) -> BackupError {
    move |e| BackupError::Io(what, e.kind())
}

fn backup_dir(state: &Path) -> PathBuf {
    state.join(BACKUP_DIR)
}

fn private_backup_dir(state: &Path) -> Result<PathBuf, BackupError> {
    let dir = backup_dir(state);
    ensure_private_dir(&dir).map_err(|e| BackupError::Invalid(format!("backup directory: {e}")))?;
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

/// The pre-migration backups the migration journal lists and has not pruned.
///
/// # Errors
///
/// Returns [`BackupError::Io`] when the journal exists but cannot be read.
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

pub(crate) fn before_migration(
    conn: &Connection,
    state: &Path,
    from: i64,
    to: i64,
) -> Result<PathBuf, BackupError> {
    let dir = private_backup_dir(state)?;
    let name = format!("{DATABASE}.schema-{from}.{}.bak", now_ms());
    let path = dir.join(&name);
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(io_err("create backup"))?;
    let written = conn
        .execute(
            "VACUUM INTO CAST(?1 AS TEXT)",
            [path.as_os_str().as_encoded_bytes()],
        )
        .map_err(BackupError::from)
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

/// The schema version of the database at `path`, or `None` when there is no database.
///
/// # Errors
///
/// Returns [`BackupError::Sql`] when the database cannot be opened or queried.
pub fn database_schema(path: &Path) -> Result<Option<i64>, BackupError> {
    if !path.exists() {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    schema_of(&conn).map(Some)
}

fn schema_of(conn: &Connection) -> Result<i64, BackupError> {
    let has_table: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'",
        [],
        |row| row.get(0),
    )?;
    if has_table == 0 {
        return Ok(0);
    }
    conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )
    .map_err(BackupError::from)
}

#[derive(Debug)]
pub struct Restored {
    pub backup: Backup,
    pub database_schema: i64,
    pub kept: PathBuf,
}

/// Restores the newest intact backup taken before schema `schema` was migrated, keeping
/// the newer database beside the backups.
///
/// # Errors
///
/// Returns [`BackupError::Busy`] when a service holds the state lock, [`BackupError::NotNeeded`]
/// when the database is not newer than `schema`, [`BackupError::NoBackup`] when no backup of
/// `schema` matches its recorded digest, [`BackupError::Invalid`] when there is no database or the
/// backup fails its integrity or schema check, and [`BackupError::Io`] or [`BackupError::Sql`] when
/// a step of the swap fails.
pub fn restore(state: &Path, schema: i64) -> Result<Restored, BackupError> {
    let _lock = super::lock_state(state).map_err(|e| match e.kind() {
        io::ErrorKind::WouldBlock => BackupError::Busy,
        kind => BackupError::Io("lock state directory", kind),
    })?;
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
        let conn = Connection::open_with_flags(&source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let check: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        if check != "ok" {
            return Err(BackupError::Invalid("integrity check failed".into()));
        }
        let found = schema_of(&conn)?;
        if found != schema {
            return Err(BackupError::Invalid(format!("it holds schema {found}")));
        }
    }

    {
        let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    }
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
        let mut kept_side = kept.as_os_str().to_os_string();
        kept_side.push(suffix);
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
