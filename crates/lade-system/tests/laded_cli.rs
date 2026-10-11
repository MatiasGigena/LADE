//! The `laded` executable's maintenance commands: persistent redacted diagnostics and export, and
//! the rollback restore used by `scripts/install --rollback` (forward migration → restore → the
//! older schema starts again).

#[path = "adapters_support/mod.rs"]
mod adapters_support;

use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use adapters_support::TempDir;
use lade_system::storage::{Store, backup};
use sha2::{Digest as _, Sha256};

fn laded() -> Command {
    Command::new(env!("CARGO_BIN_EXE_laded"))
}

fn run(args: &[&str]) -> Output {
    laded().args(args).output().expect("run laded")
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

fn private_dir(path: &Path) {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .expect("mkdir");
}

/// Starts `laded serve` and waits for its control socket.
fn serve(state: &Path, run_dir: &Path, home: &Path) -> Child {
    // A killed instance leaves its socket behind; wait for the new one.
    let _ = fs::remove_file(run_dir.join("control.sock"));
    let mut child = laded()
        .args(["serve", "--state-dir"])
        .arg(state)
        .arg("--runtime-dir")
        .arg(run_dir)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn laded");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !run_dir.join("control.sock").exists() {
        if Instant::now() > deadline || child.try_wait().is_ok_and(|s| s.is_some()) {
            panic!("laded did not listen: {}", stop(child));
        }
        thread::sleep(Duration::from_millis(50));
    }
    child
}

fn stop(mut child: Child) -> String {
    let _ = child.kill();
    let output = child.wait_with_output().expect("wait");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn serve_keeps_private_redacted_logs_and_export_is_private() {
    let root = TempDir::new("laded-diag");
    let home = root.path().join("home");
    let state = root.path().join("state");
    let run_dir = root.path().join("run");
    fs::create_dir_all(&home).expect("home");
    let child = serve(&state, &run_dir, &home);
    let stderr = stop(child);
    assert!(stderr.contains("laded: serving epoch="), "{stderr}");
    let logs = state.join("logs");
    assert_eq!(mode(&logs), 0o700);
    let log = logs.join("laded.log");
    assert_eq!(mode(&log), 0o600);
    let text = fs::read_to_string(&log).expect("log");
    let record: serde_json::Value =
        serde_json::from_str(text.lines().next().expect("a record")).expect("json record");
    assert_eq!(record["source"], "laded");
    assert!(
        record["line"]
            .as_str()
            .is_some_and(|l| l.starts_with("laded: serving epoch="))
    );

    // A second instance refuses; its refusal is logged with the home directory redacted.
    let child = serve(&state, &run_dir, &home);
    let refused = laded()
        .args(["serve", "--state-dir"])
        .arg(&state)
        .arg("--runtime-dir")
        .arg(root.path().join("run2"))
        .env("HOME", root.path())
        .output()
        .expect("second laded");
    stop(child);
    assert_eq!(refused.status.code(), Some(78));
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("another service owns"), "{stderr}");
    let text = fs::read_to_string(&log).expect("log");
    assert!(
        text.contains("refused to start: another service owns"),
        "{text}"
    );

    // Paths under the home directory reach the journal and the log as `~`.
    let long_runtime = root.path().join("r".repeat(100));
    let refused = laded()
        .args(["serve", "--state-dir"])
        .arg(&state)
        .arg("--runtime-dir")
        .arg(&long_runtime)
        .env("HOME", root.path())
        .output()
        .expect("long runtime");
    assert_eq!(refused.status.code(), Some(78));
    let stderr = String::from_utf8_lossy(&refused.stderr);
    let root_text = root.path().to_str().expect("utf-8 path");
    assert!(stderr.contains("socket path ~/rrrr"), "{stderr}");
    assert!(!stderr.contains(root_text), "{stderr}");
    let text = fs::read_to_string(&log).expect("log");
    assert!(text.contains("socket path ~/rrrr"), "{text}");
    assert!(!text.contains(root_text), "{text}");

    let out = root.path().join("diagnostics.txt");
    let export = laded()
        .args(["diagnostics", "export"])
        .arg(&out)
        .arg("--state-dir")
        .arg(&state)
        .output()
        .expect("export");
    assert!(export.status.success(), "{export:?}");
    assert_eq!(mode(&out), 0o600);
    let bundle = fs::read_to_string(&out).expect("bundle");
    assert!(bundle.contains("=== logs/laded.log"), "{bundle}");
    assert!(bundle.contains("serving epoch="));
    assert!(!bundle.contains(root_text), "{bundle}");
    let again = laded()
        .args(["diagnostics", "export"])
        .arg(&out)
        .arg("--state-dir")
        .arg(&state)
        .output()
        .expect("export again");
    assert_eq!(
        again.status.code(),
        Some(73),
        "existing output is never overwritten"
    );
}

fn info(state: &Path) -> String {
    let output = run(&[
        "store",
        "info",
        "--state-dir",
        state.to_str().expect("utf-8"),
    ]);
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).expect("utf-8")
}

/// A schema-1 store as written by this build, its journaled backup, then the database moved to
/// a schema-2 layout as a newer build would after taking that backup.
fn migrated_store(state: &Path) -> PathBuf {
    private_dir(state);
    let db = state.join(backup::DATABASE);
    let store = Store::open(&db).expect("store");
    store
        .conn()
        .execute(
            "INSERT INTO workspaces (id, path, name, opened_at) VALUES ('11111111-1111-4111-8111-111111111111', X'62', 'w', 0)",
            [],
        )
        .expect("insert");
    drop(store);
    let backups = state.join(backup::BACKUP_DIR);
    private_dir(&backups);
    let name = "lade.db.schema-1.1700000000000.bak";
    fs::copy(&db, backups.join(name)).expect("backup copy");
    fs::set_permissions(backups.join(name), fs::Permissions::from_mode(0o600)).expect("chmod");
    let sha: String = Sha256::digest(fs::read(backups.join(name)).expect("read"))
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    fs::write(
        backups.join(backup::JOURNAL),
        format!(
            "1700000000000\tbackup\t1\t2\t{name}\t{sha}\t0.3.0\n1700000000001\tapplied\t1\t2\n"
        ),
    )
    .expect("journal");
    let conn = rusqlite::Connection::open(&db).expect("open");
    conn.execute_batch(
        "CREATE TABLE added_in_v2 (id INTEGER PRIMARY KEY);
         INSERT INTO schema_migrations (version, applied_at) VALUES (2, 1700000000001);
         INSERT INTO workspaces (id, path, name, opened_at) VALUES ('22222222-2222-4222-8222-222222222222', X'61', 'w', 0);",
    )
    .expect("newer schema");
    db
}

#[test]
fn rollback_restore_lets_the_older_schema_start_and_keeps_the_newer_database() {
    let root = TempDir::new("laded-restore");
    let state = root.path().join("state");
    let run_dir = root.path().join("run");
    let db = migrated_store(&state);

    let text = info(&state);
    assert!(text.contains("supported_schema=1\n"), "{text}");
    assert!(text.contains("database_schema=2\n"), "{text}");
    assert!(
        text.lines()
            .any(|l| l.starts_with("backup schema=1 to=2 taken=2023-11-14T22:13:20.000Z file=")),
        "{text}"
    );

    // This (older, schema-1) build refuses the migrated database and leaves it untouched.
    let refused = laded()
        .args(["serve", "--state-dir"])
        .arg(&state)
        .arg("--runtime-dir")
        .arg(&run_dir)
        .output()
        .expect("serve");
    assert_eq!(refused.status.code(), Some(78));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("schema 2 is newer"));

    // A running service blocks the restore.
    let blocker = fs::OpenOptions::new()
        .write(true)
        .open(state.join("lade.lock"))
        .expect("lock file");
    rustix::fs::flock(
        &blocker,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .expect("flock");
    let busy = run(&[
        "store",
        "restore",
        "--schema",
        "1",
        "--state-dir",
        state.to_str().expect("utf-8"),
    ]);
    assert_eq!(busy.status.code(), Some(75), "{busy:?}");
    drop(blocker);

    let restored = run(&[
        "store",
        "restore",
        "--schema",
        "1",
        "--state-dir",
        state.to_str().expect("utf-8"),
    ]);
    assert!(restored.status.success(), "{restored:?}");
    let stdout = String::from_utf8_lossy(&restored.stdout);
    assert!(stdout.contains("restored schema 1"), "{stdout}");
    assert_eq!(mode(&db), 0o600);
    assert!(info(&state).contains("database_schema=1\n"));
    let kept: Vec<PathBuf> = fs::read_dir(state.join(backup::BACKUP_DIR))
        .expect("dir")
        .map(|e| e.expect("entry").path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("lade.db.newer-schema-2."))
        })
        .collect();
    assert_eq!(kept.len(), 1, "{kept:?}");
    let newer = rusqlite::Connection::open(&kept[0]).expect("kept opens");
    let after: i64 = newer
        .query_row(
            "SELECT COUNT(*) FROM workspaces WHERE id = '22222222-2222-4222-8222-222222222222'",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(after, 1, "post-migration data is kept");

    // The older schema starts again with its own data.
    let child = serve(&state, &run_dir, root.path());
    stop(child);
    let store = Store::open(&db).expect("reopen");
    let ids: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM workspaces", [], |row| row.get(0))
        .expect("count");
    assert_eq!(ids, 1);

    // Nothing left to restore.
    let again = run(&[
        "store",
        "restore",
        "--schema",
        "1",
        "--state-dir",
        state.to_str().expect("utf-8"),
    ]);
    assert_eq!(again.status.code(), Some(1));
}

#[test]
fn maintenance_commands_reject_bad_usage() {
    for args in [
        &["store"][..],
        &["store", "restore"],
        &["store", "restore", "--schema", "x"],
        &["store", "info", "--bogus", "1"],
        &["diagnostics", "export"],
        &["diagnostics", "export", "--state-dir", "/tmp"],
    ] {
        assert_eq!(run(args).status.code(), Some(64), "{args:?}");
    }
}
