mod adapters_support;

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use adapters_support::TempDir;
use lade_system::storage::{Store, backup};

#[test]
fn migration_and_restore_preserve_non_utf8_state_paths() {
    let tmp = TempDir::new("storage-path-bytes");
    let state = tmp.path().join(OsString::from_vec(b"state-\xff".to_vec()));
    fs::create_dir(&state).unwrap();
    let db = state.join(backup::DATABASE);
    drop(Store::open_as_schema(&db, 1).unwrap());

    drop(Store::open(&db).expect("migration backs up using the exact path bytes"));
    let restored = backup::restore(&state, 1).expect("restore uses the exact path bytes");

    assert!(restored.kept.exists());
    assert_eq!(backup::database_schema(&db).unwrap(), Some(1));
    assert_eq!(
        backup::database_schema(&restored.kept).unwrap(),
        Some(lade_system::storage::SCHEMA_VERSION)
    );
}

#[test]
fn restore_keeps_wal_sidecars_under_non_utf8_state_paths() {
    let tmp = TempDir::new("storage-sidecar-bytes");
    let state = tmp.path().join(OsString::from_vec(b"state-\xff".to_vec()));
    fs::create_dir(&state).unwrap();
    let db = state.join(backup::DATABASE);
    drop(Store::open_as_schema(&db, 1).unwrap());
    let current = Store::open(&db).unwrap();
    assert!(state.join("lade.db-wal").exists());
    assert!(state.join("lade.db-shm").exists());

    let restored = backup::restore(&state, 1).unwrap();

    for suffix in ["-wal", "-shm"] {
        let mut kept_side = restored.kept.as_os_str().to_os_string();
        kept_side.push(suffix);
        assert!(
            PathBuf::from(kept_side).exists(),
            "newer database sidecar {suffix} was not kept"
        );
        assert!(!state.join(format!("lade.db{suffix}")).exists());
    }
    drop(current);
    assert_eq!(backup::database_schema(&db).unwrap(), Some(1));
}
