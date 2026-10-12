//! `laded`: the authoritative LADE service.
//!
//! Test harnesses start `serve` with `LADE_TEST_FIXTURE_PROVIDER=1` to enable the offline
//! `lade-fixture` model (a warning is logged); a normal install never offers it.
//!
//! ```text
//! laded --version
//! laded serve [--state-dir DIR] [--runtime-dir DIR] [--max-workers N] [--max-terminals N]
//!             [--storage-warn-mib N] [--storage-reserve-mib N]
//! laded credentials set <provider> [--variable NAME]
//! laded credentials clear <provider> [--variable NAME]
//! laded store info [--state-dir DIR]
//! laded store restore --schema N [--state-dir DIR]
//! laded diagnostics export OUT [--state-dir DIR]
//! ```
//!
//! Exit statuses: 0 success, 64 usage error, 78 refused to start (unsafe directory, unopenable or
//! corrupt store, another instance), 70 internal failure; `credentials` 65 an empty or multi-line
//! key and 69 the Secret Service is unavailable (the reason and the credentials-file alternative
//! are printed; nothing is written anywhere else); `store restore` 75 while a service holds the
//! state directory, 1 when refused (no intact backup, nothing to restore); `diagnostics export`
//! 73 when the output exists, 74 on I/O errors. `serve` diagnostics go to stderr and, redacted
//! and bounded, to `$STATE/logs` (see `lade_system::diagnostics`); `credentials` never starts
//! that capture, and the key it reads is never printed.

use std::ffi::OsString;
use std::io::{self, BufRead, Read};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use rustix::termios::{LocalModes, OptionalActions, Termios, isatty, tcgetattr, tcsetattr};

use lade_system::adapters::sandbox::{OBJECTIVE_CGROUP_LIMITS, SandboxCgroups, cgroup};
use lade_system::diagnostics;
use lade_system::service::{self, ResourceLimits, ServiceConfig, StartError};
use lade_system::storage::{self, backup};
use lade_system::supervision::credentials::{
    FIXTURE_PROVIDER, FIXTURE_SWITCH, custom_var, fixture_switch_on, provider_env_vars,
};
use lade_system::supervision::secret_service::{
    MAX_SECRET_BYTES, SecretService, valid_provider_id,
};
use lade_system::supervision::{
    GATEWAY_SOCKET_DIR, LaunchError, LaunchRequest, LauncherConfig, SupervisorEvent, WorkerControl,
    WorkerLauncher, default_launcher,
};

const USAGE: &str = "usage: laded serve [--state-dir DIR] [--runtime-dir DIR] [--max-workers N]
                   [--max-terminals N] [--storage-warn-mib N] [--storage-reserve-mib N]
       laded credentials set|clear <provider> [--variable NAME]
       laded store info [--state-dir DIR]
       laded store restore --schema N [--state-dir DIR]
       laded diagnostics export OUT [--state-dir DIR]
       laded --version";

/// What `laded credentials` prints when the Secret Service cannot be used.
const FILE_FALLBACK: &str = "put the key in ~/.config/lade/credentials.env instead: one line \
     NAME=value, file mode 0600 (chmod 600 ~/.config/lade/credentials.env)";

enum CredentialsAction {
    Set,
    Clear,
}

/// `credentials set|clear <provider> [--variable NAME]`, validated: the variable is the provider's
/// own (the first one by default), or a `LADE_PROVIDER_*` name for a custom provider.
fn parse_credentials(
    args: &[OsString],
) -> Result<(CredentialsAction, String, Option<String>), String> {
    let args: Vec<&str> = args
        .iter()
        .map(|a| a.to_str().ok_or("arguments must be UTF-8"))
        .collect::<Result<_, _>>()?;
    let (action, provider, variable) = match args.as_slice() {
        [action, provider] => (*action, *provider, None),
        [action, provider, "--variable", variable] => (*action, *provider, Some(*variable)),
        _ => return Err(USAGE.into()),
    };
    let action = match action {
        "set" => CredentialsAction::Set,
        "clear" => CredentialsAction::Clear,
        _ => return Err(USAGE.into()),
    };
    if !valid_provider_id(provider) {
        return Err(format!("invalid provider id {provider:?}"));
    }
    if provider == FIXTURE_PROVIDER {
        return Err(format!("{FIXTURE_PROVIDER} needs no credentials"));
    }
    let variable = match (provider_env_vars(provider), variable) {
        (Some(vars), Some(variable)) if !vars.contains(&variable) => {
            return Err(format!(
                "provider {provider} reads {}, not {variable}",
                vars.join(", ")
            ));
        }
        (Some(_), Some(variable)) => Some(variable.to_owned()),
        // Setting defaults to the provider's first variable; clearing removes all of them.
        (Some(vars), None) => match action {
            CredentialsAction::Set => vars.first().map(|v| (*v).to_owned()),
            CredentialsAction::Clear => None,
        },
        (None, Some(variable)) if custom_var(variable) => Some(variable.to_owned()),
        (None, _) => {
            return Err(format!(
                "{provider} is not a built-in provider: name the custom provider's variable with \
                 --variable LADE_PROVIDER_…"
            ));
        }
    };
    Ok((action, provider.to_owned(), variable))
}

/// Terminal echo is off while this lives.
struct EchoOff(Termios);

impl EchoOff {
    fn new() -> io::Result<Self> {
        let saved = tcgetattr(io::stdin())?;
        let mut quiet = saved.clone();
        quiet.local_modes.remove(LocalModes::ECHO);
        tcsetattr(io::stdin(), OptionalActions::Flush, &quiet)?;
        Ok(Self(saved))
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        let _ = tcsetattr(io::stdin(), OptionalActions::Now, &self.0);
    }
}

/// The key from stdin: one line typed without echo on a terminal, else everything piped in. One
/// trailing newline is dropped; the rest must be one non-empty line of at most 64 KiB.
fn read_key(variable: &str, provider: &str) -> Result<Vec<u8>, ExitCode> {
    let mut key = Vec::new();
    let read = if isatty(io::stdin()) {
        let Ok(echo) = EchoOff::new() else {
            eprintln!("laded: could not turn off terminal echo; pipe the key on stdin instead");
            return Err(ExitCode::from(65));
        };
        eprint!("{variable} for {provider} (input hidden): ");
        let read = io::stdin()
            .lock()
            .take(MAX_SECRET_BYTES as u64 + 2)
            .read_until(b'\n', &mut key);
        drop(echo);
        eprintln!();
        read
    } else {
        io::stdin()
            .lock()
            .take(MAX_SECRET_BYTES as u64 + 2)
            .read_to_end(&mut key)
    };
    if read.is_err() {
        eprintln!("laded: could not read the key from stdin");
        return Err(ExitCode::from(65));
    }
    if key.ends_with(b"\n") {
        key.pop();
        if key.ends_with(b"\r") {
            key.pop();
        }
    }
    if key.is_empty()
        || key.len() > MAX_SECRET_BYTES
        || key.iter().any(|b| matches!(b, 0 | b'\n' | b'\r'))
    {
        eprintln!("laded: the key must be one non-empty line of at most 64 KiB");
        return Err(ExitCode::from(65));
    }
    Ok(key)
}

/// `laded credentials …`: store or remove a key in the user's Secret Service. The key is read
/// from stdin and handed to `secret-tool` on its stdin; it is never an argument and never written
/// to a file. Without a usable Secret Service the reason and the credentials-file alternative are
/// printed.
fn credentials_command(args: &[OsString]) -> ExitCode {
    let (action, provider, variable) = match parse_credentials(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("laded: {message}");
            return ExitCode::from(64);
        }
    };
    let service = SecretService::from_env();
    let unavailable = |reason: &str| {
        eprintln!("laded: Secret Service unavailable: {reason}");
        eprintln!("laded: {FILE_FALLBACK}");
        ExitCode::from(69)
    };
    if let Some(reason) = service.unavailable_reason() {
        return unavailable(reason);
    }
    match action {
        CredentialsAction::Set => {
            let variable = variable.unwrap_or_default();
            let key = match read_key(&variable, &provider) {
                Ok(key) => key,
                Err(code) => return code,
            };
            match service.store(&provider, &variable, &key) {
                Ok(()) => {
                    println!(
                        "stored {variable} for {provider} in the Secret Service; it is read when \
                         execution starts (the laded environment still takes precedence)"
                    );
                    ExitCode::SUCCESS
                }
                Err(reason) => unavailable(&reason),
            }
        }
        CredentialsAction::Clear => match service.clear(&provider, variable.as_deref()) {
            Ok(true) => {
                println!("removed LADE's Secret Service item(s) for {provider}");
                ExitCode::SUCCESS
            }
            Ok(false) => {
                println!("no LADE Secret Service item for {provider}");
                ExitCode::SUCCESS
            }
            Err(reason) => unavailable(&reason),
        },
    }
}

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
        .and_then(|launcher| {
            default_launcher(LauncherConfig {
                cgroups: config.cgroups.clone(),
                test_fixture_provider: config.test_fixture_provider,
                ..launcher
            })
        });
    match built {
        Ok(launcher) => Arc::from(launcher),
        Err(e) => {
            eprintln!("laded: worker launcher unavailable ({e}); execution is blocked");
            Arc::new(Unavailable(e.to_string()))
        }
    }
}

/// Per-Objective cgroup limits when systemd delegated a subtree to this service; otherwise an
/// explicit diagnostic and the `prlimit` limits only (never a refusal to start).
fn delegated_cgroups() -> Option<Arc<SandboxCgroups>> {
    match cgroup::detect(OBJECTIVE_CGROUP_LIMITS) {
        Ok(cgroups) => {
            let swept = cgroups.sweep();
            eprintln!(
                "laded: cgroup v2 delegated; sandboxes run in {} with per-Objective limits{}",
                cgroups.root().display(),
                if swept > 0 {
                    format!(" ({swept} stale sandbox cgroup(s) removed)")
                } else {
                    String::new()
                }
            );
            Some(Arc::new(cgroups))
        }
        Err(reason) => {
            eprintln!(
                "laded: cgroup limits unavailable ({reason}); sandboxes use the prlimit limits only"
            );
            None
        }
    }
}

struct Args {
    state: Option<PathBuf>,
    runtime: Option<PathBuf>,
    schema: Option<i64>,
    out: Option<PathBuf>,
    /// Aggregate accounting across Objectives (`serve` only); defaults printed at startup.
    limits: ResourceLimits,
}

/// Parses `--flag VALUE` pairs (each at most once, only the `allowed` ones) and, when
/// `positional` is set, one leading positional path.
fn parse_args(args: &[OsString], allowed: &[&str], positional: bool) -> Option<Args> {
    let mut parsed = Args {
        state: None,
        runtime: None,
        schema: None,
        out: None,
        limits: ResourceLimits::default(),
    };
    let mut seen = Vec::new();
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
        if seen.contains(&flag) {
            return None;
        }
        seen.push(flag);
        let value = iter.next()?;
        let number = || value.to_str()?.parse::<u64>().ok();
        let count = || usize::try_from(number()?).ok().filter(|n| *n > 0);
        let mib = || number()?.checked_mul(1 << 20);
        match flag {
            "--max-workers" => parsed.limits.max_workers = count()?,
            "--max-terminals" => parsed.limits.max_terminals = count()?,
            "--storage-warn-mib" => parsed.limits.storage_warn_bytes = mib()?,
            "--storage-reserve-mib" => parsed.limits.storage_reserve_bytes = mib()?,
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
    let Some(mut config) =
        ServiceConfig::from_environment(parsed.state.clone(), parsed.runtime.clone())
    else {
        eprintln!("laded: no state directory (set XDG_STATE_HOME or HOME, or pass --state-dir)");
        return Err(ExitCode::from(64));
    };
    if !config.state_dir.is_absolute() || !config.runtime_dir.is_absolute() {
        eprintln!("laded: --state-dir and --runtime-dir must be absolute paths");
        return Err(ExitCode::from(64));
    }
    config.limits = parsed.limits.clone();
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
    let config = ServiceConfig {
        cgroups: delegated_cgroups(),
        test_fixture_provider: fixture_switch_on(),
        ..config
    };
    if config.test_fixture_provider {
        eprintln!(
            "laded: warning: offline test model {FIXTURE_PROVIDER} enabled ({FIXTURE_SWITCH}=1); \
             test harnesses only, not for real use"
        );
    }
    if let Some(dir) = lade_system::fault::directory() {
        eprintln!(
            "laded: warning: test fault injection enabled ({}={}); not for real use",
            lade_system::fault::FAULTS_ENV,
            dir.display()
        );
    }
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
    // Before any diagnostics capture: `credentials` reads a key and must never reach $STATE/logs.
    if args.first().is_some_and(|a| a == "credentials") {
        return credentials_command(&args[1..]);
    }
    let words: Vec<&str> = args
        .iter()
        .take(2)
        .map(|a| a.to_str().unwrap_or(""))
        .collect();
    let parsed = match words.as_slice() {
        ["serve", ..] => parse_args(
            &args[1..],
            &[
                "--state-dir",
                "--runtime-dir",
                "--max-workers",
                "--max-terminals",
                "--storage-warn-mib",
                "--storage-reserve-mib",
            ],
            false,
        )
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
