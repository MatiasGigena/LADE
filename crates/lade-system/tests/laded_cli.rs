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

fn serve(state: &Path, run_dir: &Path, home: &Path) -> Child {
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
    let records: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).expect("json record"))
        .collect();
    assert!(
        records.iter().any(|r| r["source"] == "laded"
            && r["line"]
                .as_str()
                .is_some_and(|l| l.starts_with("laded: serving epoch="))),
        "{text}"
    );

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

const BEFORE: &str = "11111111-1111-4111-8111-111111111111";
const AFTER: &str = "22222222-2222-4222-8222-222222222222";

fn add_workspace(db: &Path, id: &str) {
    let conn = rusqlite::Connection::open(db).expect("open");
    conn.execute(
        "INSERT INTO workspaces (id, path, name, opened_at) VALUES (?1, CAST(?1 AS BLOB), 'w', 0)",
        [id],
    )
    .expect("insert");
}

fn restore(state: &Path, schema: &str) -> Output {
    run(&[
        "store",
        "restore",
        "--schema",
        schema,
        "--state-dir",
        state.to_str().expect("utf-8"),
    ])
}

#[test]
fn rollback_restore_after_the_real_migrations_lets_a_schema_1_store_start() {
    let root = TempDir::new("laded-restore");
    let state = root.path().join("state");
    let run_dir = root.path().join("run");
    private_dir(&state);
    let db = state.join(backup::DATABASE);

    drop(Store::open_as_schema(&db, 1).expect("schema-1 store"));
    add_workspace(&db, BEFORE);
    assert!(info(&state).contains("database_schema=1\n"));

    let child = serve(&state, &run_dir, root.path());
    stop(child);
    add_workspace(&db, AFTER);
    let text = info(&state);
    let latest = lade_system::storage::SCHEMA_VERSION;
    const { assert!(lade_system::storage::SCHEMA_VERSION >= 3) };
    assert!(
        text.contains(&format!("supported_schema={latest}\n")),
        "{text}"
    );
    assert!(
        text.contains(&format!("database_schema={latest}\n")),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l.starts_with(&format!("backup schema=1 to={latest} taken="))),
        "{text}"
    );
    let journal =
        fs::read_to_string(state.join(backup::BACKUP_DIR).join(backup::JOURNAL)).expect("journal");
    assert!(
        journal
            .lines()
            .any(|l| l.ends_with(&format!("\tapplied\t1\t{latest}")))
    );
    for entry in fs::read_dir(state.join(backup::BACKUP_DIR)).expect("dir") {
        assert_eq!(mode(&entry.expect("entry").path()), 0o600);
    }
    assert!(matches!(
        Store::open_as_schema(&db, 1),
        Err(lade_system::storage::StoreError::NewerSchema(v)) if v == latest
    ));

    let blocker = fs::OpenOptions::new()
        .write(true)
        .open(state.join("lade.lock"))
        .expect("lock file");
    rustix::fs::flock(
        &blocker,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .expect("flock");
    let busy = restore(&state, "1");
    assert_eq!(busy.status.code(), Some(75), "{busy:?}");
    drop(blocker);

    let restored = restore(&state, "1");
    assert!(restored.status.success(), "{restored:?}");
    let stdout = String::from_utf8_lossy(&restored.stdout);
    assert!(stdout.contains("restored schema 1"), "{stdout}");
    assert_eq!(mode(&db), 0o600);
    assert!(info(&state).contains("database_schema=1\n"));
    let prefix = format!("lade.db.newer-schema-{latest}.");
    let kept: Vec<PathBuf> = fs::read_dir(state.join(backup::BACKUP_DIR))
        .expect("dir")
        .map(|e| e.expect("entry").path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(&prefix))
        })
        .collect();
    assert_eq!(kept.len(), 1, "{kept:?}");
    let newer = rusqlite::Connection::open(&kept[0]).expect("kept opens");
    let after: i64 = newer
        .query_row(
            "SELECT COUNT(*) FROM workspaces WHERE id = ?1",
            [AFTER],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(after, 1, "post-migration data is kept");

    let older = Store::open_as_schema(&db, 1).expect("schema-1 store starts after the restore");
    assert_eq!(older.schema_version().expect("version"), 1);
    let ids: i64 = older
        .conn()
        .query_row("SELECT COUNT(*) FROM workspaces", [], |row| row.get(0))
        .expect("count");
    assert_eq!(ids, 1);
    drop(older);

    assert_eq!(restore(&state, "1").status.code(), Some(1));
    let child = serve(&state, &run_dir, root.path());
    stop(child);
    assert!(info(&state).contains(&format!("database_schema={latest}\n")));
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
