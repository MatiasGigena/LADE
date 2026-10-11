//! `laded`: the authoritative LADE service.
//!
//! ```text
//! laded --version
//! laded serve [--state-dir DIR] [--runtime-dir DIR]
//! laded store info [--state-dir DIR]
//! laded store restore --schema N [--state-dir DIR]
//! laded diagnostics export OUT [--state-dir DIR]
//! ```
//!
//! Exit statuses: 0 success, 64 usage error, 78 refused to start (unsafe directory, unopenable or
//! corrupt store, another instance), 70 internal failure; `store restore` 75 while a service
//! holds the state directory, 1 when refused (no intact backup, nothing to restore);
//! `diagnostics export` 73 when the output exists, 74 on I/O errors. `serve` diagnostics go to
//! stderr and, redacted and bounded, to `$STATE/logs` (see `lade_system::diagnostics`).

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use lade_system::diagnostics;
use lade_system::service::{self, ServiceConfig, StartError};
use lade_system::storage::{self, backup};
use lade_system::supervision::{
    GATEWAY_SOCKET_DIR, LaunchError, LaunchRequest, LauncherConfig, SupervisorEvent, WorkerControl,
    WorkerLauncher, default_launcher,
};

const USAGE: &str = "usage: laded serve [--state-dir DIR] [--runtime-dir DIR]
       laded store info [--state-dir DIR]
       laded store restore --schema N [--state-dir DIR]
       laded diagnostics export OUT [--state-dir DIR]
       laded --version";

/// Launcher used when the sandboxed launcher cannot be constructed: every launch is refused, so
/// execution fails closed and never runs unsandboxed.
struct Unavailable(String);

impl WorkerLauncher for Unavailable {
    fn launch(
        &self,
        _request: LaunchRequest,
        _events: std::sync::mpsc::SyncSender<SupervisorEvent>,
    ) -> Result<Box<dyn WorkerControl>, LaunchError> {
        Err(LaunchError::SandboxUnavailable(self.0.clone()))
    }

    fn preflight(&self, _provider: &str) -> Result<(), LaunchError> {
        Err(LaunchError::SandboxUnavailable(self.0.clone()))
    }
}

fn make_launcher(config: &ServiceConfig) -> Arc<dyn WorkerLauncher> {
    let run_dir = config.state_dir.join("run");
    let socket_dir = config.runtime_dir.join(GATEWAY_SOCKET_DIR);
    let built = std::fs::DirBuilder::new()
        .recursive(true)
        .create(&run_dir)
        .map_err(LaunchError::Io)
        .and_then(|()| LauncherConfig::new(run_dir, socket_dir))
        .and_then(default_launcher);
    match built {
        Ok(launcher) => Arc::from(launcher),
        Err(e) => {
            eprintln!("laded: worker launcher unavailable ({e}); execution is blocked");
            Arc::new(Unavailable(e.to_string()))
        }
    }
}

struct Args {
    state: Option<PathBuf>,
    runtime: Option<PathBuf>,
    schema: Option<i64>,
    out: Option<PathBuf>,
}

/// Parses `--flag VALUE` pairs (each at most once, only the `allowed` ones) and, when
/// `positional` is set, one leading positional path.
fn parse_args(args: &[OsString], allowed: &[&str], positional: bool) -> Option<Args> {
    let mut parsed = Args {
        state: None,
        runtime: None,
        schema: None,
        out: None,
    };
    let mut iter = args.iter().peekable();
    if positional {
        let first = iter.next()?;
        if first.to_str().is_some_and(|s| s.starts_with("--")) {
            return None;
        }
        parsed.out = Some(PathBuf::from(first));
    }
    while let Some(flag) = iter.next() {
        let flag = flag.to_str()?;
        if !allowed.contains(&flag) {
            return None;
        }
        let value = iter.next()?;
        match flag {
            "--state-dir" if parsed.state.is_none() => parsed.state = Some(PathBuf::from(value)),
            "--runtime-dir" if parsed.runtime.is_none() => {
                parsed.runtime = Some(PathBuf::from(value));
            }
            "--schema" if parsed.schema.is_none() => {
                parsed.schema = Some(value.to_str()?.parse().ok().filter(|v| *v >= 0)?);
            }
            _ => return None,
        }
    }
    Some(parsed)
}

fn config(parsed: &Args) -> Result<ServiceConfig, ExitCode> {
    let Some(config) =
        ServiceConfig::from_environment(parsed.state.clone(), parsed.runtime.clone())
    else {
        eprintln!("laded: no state directory (set XDG_STATE_HOME or HOME, or pass --state-dir)");
        return Err(ExitCode::from(64));
    };
    if !config.state_dir.is_absolute() || !config.runtime_dir.is_absolute() {
        eprintln!("laded: --state-dir and --runtime-dir must be absolute paths");
        return Err(ExitCode::from(64));
    }
    Ok(config)
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(64)
}

fn serve(parsed: &Args) -> ExitCode {
    let config = match config(parsed) {
        Ok(config) => config,
        Err(code) => return code,
    };
    // Every file the service creates is private to its user.
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    // From here on, stderr is redacted and also kept in the bounded $STATE/logs.
    let capture = match diagnostics::capture_stderr(config.state_dir.clone()) {
        Ok(capture) => Some(capture),
        Err(e) => {
            eprintln!("laded: persistent diagnostics unavailable: {}", e.kind());
            None
        }
    };
    let launcher = make_launcher(&config);
    let code = match service::start(config, launcher) {
        Ok(running) => {
            eprintln!("laded: serving epoch={}", running.service_epoch());
            running.wait();
            ExitCode::SUCCESS
        }
        Err(e @ StartError::InvalidPath(_)) => {
            eprintln!("laded: {e}");
            ExitCode::from(64)
        }
        Err(e) => {
            eprintln!("laded: refused to start: {e}");
            ExitCode::from(78)
        }
    };
    if let Some(capture) = capture {
        capture.finish();
    }
    code
}

/// `laded store info`: what the installer needs to decide a rollback.
fn store_info(parsed: &Args) -> ExitCode {
    let config = match config(parsed) {
        Ok(config) => config,
        Err(code) => return code,
    };
    let state = &config.state_dir;
    let db = state.join(backup::DATABASE);
    println!("supported_schema={}", storage::SCHEMA_VERSION);
    println!("database={}", db.display());
    match backup::database_schema(&db) {
        Ok(Some(version)) => println!("database_schema={version}"),
        Ok(None) => println!("database_schema=absent"),
        Err(e) => {
            eprintln!("laded: cannot read the database schema: {e}");
            return ExitCode::from(74);
        }
    }
    match backup::backups(state) {
        Ok(list) => {
            for b in list {
                println!(
                    "backup schema={} to={} taken={} file={}",
                    b.from,
                    b.to,
                    service::convert::wire_ts(b.taken_ms),
                    state.join(backup::BACKUP_DIR).join(&b.file).display()
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("laded: cannot read the migration journal: {e}");
            ExitCode::from(74)
        }
    }
}

/// `laded store restore --schema N`: the rollback restore (exit 75 while a service runs).
fn store_restore(parsed: &Args) -> ExitCode {
    let Some(schema) = parsed.schema else {
        return usage();
    };
    let config = match config(parsed) {
        Ok(config) => config,
        Err(code) => return code,
    };
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    match backup::restore(&config.state_dir, schema) {
        Ok(restored) => {
            println!(
                "restored schema {} backup taken {} (database was schema {})",
                schema,
                service::convert::wire_ts(restored.backup.taken_ms),
                restored.database_schema
            );
            println!("kept the newer database as {}", restored.kept.display());
            ExitCode::SUCCESS
        }
        Err(e @ backup::BackupError::Busy) => {
            eprintln!("laded: {e}");
            ExitCode::from(75)
        }
        Err(e) => {
            eprintln!("laded: restore refused: {e}");
            ExitCode::from(1)
        }
    }
}

/// `laded diagnostics export OUT`: a new 0600 redacted text bundle.
fn diagnostics_export(parsed: &Args) -> ExitCode {
    let Some(out) = parsed.out.as_deref() else {
        return usage();
    };
    let config = match config(parsed) {
        Ok(config) => config,
        Err(code) => return code,
    };
    match diagnostics::export(&config.state_dir, out) {
        Ok(summary) => {
            println!(
                "wrote {} ({} bytes, mode 0600)",
                out.display(),
                summary.bytes
            );
            for (section, lines) in summary.sections {
                println!("  {section}: {lines} lines");
            }
            println!("review the file before sharing it; redaction is best effort");
            ExitCode::SUCCESS
        }
        Err(e @ diagnostics::ExportError::Exists) => {
            eprintln!("laded: {e}");
            ExitCode::from(73)
        }
        Err(e) => {
            eprintln!("laded: export failed: {e}");
            ExitCode::from(74)
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args == ["--version"] {
        println!(
            "laded {} protocol {}",
            lade_protocol::BUNDLE,
            lade_protocol::VERSION
        );
        return ExitCode::SUCCESS;
    }
    let words: Vec<&str> = args
        .iter()
        .take(2)
        .map(|a| a.to_str().unwrap_or(""))
        .collect();
    let parsed = match words.as_slice() {
        ["serve", ..] => parse_args(&args[1..], &["--state-dir", "--runtime-dir"], false)
            .map(|p| (serve as fn(&Args) -> ExitCode, p)),
        ["store", "info"] => parse_args(&args[2..], &["--state-dir"], false)
            .map(|p| (store_info as fn(&Args) -> ExitCode, p)),
        ["store", "restore"] => parse_args(&args[2..], &["--state-dir", "--schema"], false)
            .map(|p| (store_restore as fn(&Args) -> ExitCode, p)),
        ["diagnostics", "export"] => parse_args(&args[2..], &["--state-dir"], true)
            .map(|p| (diagnostics_export as fn(&Args) -> ExitCode, p)),
        _ => None,
    };
    match parsed {
        Some((run, args)) => run(&args),
        None => usage(),
    }
}
