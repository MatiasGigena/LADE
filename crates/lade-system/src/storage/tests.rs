use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::PathBuf;

use super::schema::SCHEMA_V1;
use super::*;
use crate::support::now_ms;

const V2: &str = "CREATE TABLE added_in_v2 (id INTEGER PRIMARY KEY);";
const OLD: &[(i64, &str)] = &[(1, SCHEMA_V1)];
const NEW: &[(i64, &str)] = &[(1, SCHEMA_V1), (2, V2)];

fn temp_state(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lade-store-{name}-{}-{}",
        std::process::id(),
        now_ms()
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
    let journal =
        fs::read_to_string(state.join(backup::BACKUP_DIR).join(backup::JOURNAL)).expect("journal");
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
