use std::ffi::OsString;
use std::io::{self, BufRead, Read};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use rustix::termios::{LocalModes, OptionalActions, Termios, isatty, tcgetattr, tcsetattr};

use lade_system::adapters::providers::FIXTURE_PROVIDER;
use lade_system::adapters::sandbox::{
    FIXTURE_SWITCH, OBJECTIVE_CGROUP_LIMITS, SandboxCgroups, cgroup,
};
use lade_system::diagnostics;
use lade_system::service::{self, ResourceLimits, ServiceConfig, StartError};
use lade_system::storage::{self, backup};
use lade_system::supervision::credentials::{custom_var, fixture_switch_on, provider_env_vars};
use lade_system::supervision::secret_service::{
    MAX_SECRET_BYTES, SecretService, valid_provider_id,
};
use lade_system::supervision::{
    GATEWAY_SOCKET_DIR, LaunchError, LaunchRequest, LauncherConfig, SupervisorEvent, WorkerControl,
    WorkerLauncher, default_launcher,
};
use lade_system::support::wire_ts;

const USAGE: &str = "usage: laded serve [--state-dir DIR] [--runtime-dir DIR] [--max-workers N]
                   [--max-terminals N] [--storage-warn-mib N] [--storage-reserve-mib N]
       laded credentials set|clear <provider> [--variable NAME]
       laded store info [--state-dir DIR]
       laded store restore --schema N [--state-dir DIR]
       laded diagnostics export OUT [--state-dir DIR]
       laded --version";

const FILE_FALLBACK: &str = "put the key in ~/.config/lade/credentials.env instead: one line \
     NAME=value, file mode 0600 (chmod 600 ~/.config/lade/credentials.env)";

enum CredentialsAction {
    Set,
    Clear,
}

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
    one_line_key(key).ok_or_else(|| {
        eprintln!("laded: the key must be one non-empty line of at most 64 KiB");
        ExitCode::from(65)
    })
}

/// The key in `input` without its line ending, if it is one non-empty line of at most
/// [`MAX_SECRET_BYTES`] with no NUL byte.
fn one_line_key(mut input: Vec<u8>) -> Option<Vec<u8>> {
    if input.ends_with(b"\n") {
        input.pop();
        if input.ends_with(b"\r") {
            input.pop();
        }
    }
    let valid = !input.is_empty()
        && input.len() <= MAX_SECRET_BYTES
        && !input.iter().any(|b| matches!(b, 0 | b'\n' | b'\r'));
    valid.then_some(input)
}

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

fn delegated_cgroups() -> Option<Arc<SandboxCgroups>> {
    match cgroup::detect(OBJECTIVE_CGROUP_LIMITS) {
        Ok(cgroups) => {
            let swept = cgroups.sweep();
            eprintln!(
                "laded: cgroup v2 delegated ({}); sandboxes run in {} with per-Objective limits{}",
                cgroups.delegation(),
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
    limits: ResourceLimits,
}

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
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
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
    let code = match service::start(&config, launcher) {
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
                    wire_ts(b.taken_ms),
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
                wire_ts(restored.backup.taken_ms),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Serve,
    StoreInfo,
    StoreRestore,
    DiagnosticsExport,
}

/// The subcommand `args` names and its parsed options, or `None` for a usage error. The
/// `credentials` and `--version` forms are handled before routing.
fn route(args: &[OsString]) -> Option<(Command, Args)> {
    let words: Vec<&str> = args
        .iter()
        .take(2)
        .map(|a| a.to_str().unwrap_or(""))
        .collect();
    match words.as_slice() {
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
        .map(|p| (Command::Serve, p)),
        ["store", "info"] => {
            parse_args(&args[2..], &["--state-dir"], false).map(|p| (Command::StoreInfo, p))
        }
        ["store", "restore"] => parse_args(&args[2..], &["--state-dir", "--schema"], false)
            .map(|p| (Command::StoreRestore, p)),
        ["diagnostics", "export"] => {
            parse_args(&args[2..], &["--state-dir"], true).map(|p| (Command::DiagnosticsExport, p))
        }
        _ => None,
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
    if args.first().is_some_and(|a| a == "credentials") {
        return credentials_command(&args[1..]);
    }
    match route(&args) {
        Some((Command::Serve, parsed)) => serve(&parsed),
        Some((Command::StoreInfo, parsed)) => store_info(&parsed),
        Some((Command::StoreRestore, parsed)) => store_restore(&parsed),
        Some((Command::DiagnosticsExport, parsed)) => diagnostics_export(&parsed),
        None => usage(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    fn parsed(words: &[&str]) -> Option<(Command, Args)> {
        route(&args(words))
    }

    #[test]
    fn serve_takes_directories_and_positive_limits() {
        let (command, serve) = parsed(&[
            "serve",
            "--state-dir",
            "/s",
            "--runtime-dir",
            "/r",
            "--max-workers",
            "3",
            "--max-terminals",
            "2",
            "--storage-warn-mib",
            "10",
            "--storage-reserve-mib",
            "1",
        ])
        .expect("valid");
        assert_eq!(command, Command::Serve);
        assert_eq!(serve.state, Some(PathBuf::from("/s")));
        assert_eq!(serve.runtime, Some(PathBuf::from("/r")));
        assert_eq!(serve.limits.max_workers, 3);
        assert_eq!(serve.limits.max_terminals, 2);
        assert_eq!(serve.limits.storage_warn_bytes, 10 << 20);
        assert_eq!(serve.limits.storage_reserve_bytes, 1 << 20);
        let (_, defaults) = parsed(&["serve"]).expect("no options");
        assert_eq!(defaults.limits, ResourceLimits::default());
    }

    #[test]
    fn options_are_refused_when_unknown_repeated_valueless_or_out_of_range() {
        for words in [
            &["serve", "--schema", "1"][..],
            &["serve", "--state-dir", "/a", "--state-dir", "/b"],
            &["serve", "--state-dir"],
            &["serve", "--max-workers", "0"],
            &["serve", "--max-workers", "-1"],
            &["serve", "--max-terminals", "many"],
            &["serve", "--storage-warn-mib", &u64::MAX.to_string()],
            &["store", "restore", "--schema", "-1"],
            &["store", "info", "extra"],
            &["store"],
            &["diagnostics", "export"],
            &["diagnostics", "export", "--state-dir", "/s"],
            &["bogus"],
            &[],
        ] {
            assert!(parsed(words).is_none(), "{words:?}");
        }
    }

    #[test]
    fn store_and_diagnostics_subcommands_route_with_their_options() {
        let (command, restore) =
            parsed(&["store", "restore", "--schema", "2", "--state-dir", "/s"]).expect("restore");
        assert_eq!(command, Command::StoreRestore);
        assert_eq!(restore.schema, Some(2));
        assert_eq!(restore.state, Some(PathBuf::from("/s")));
        let (command, info) = parsed(&["store", "info"]).expect("info");
        assert_eq!(command, Command::StoreInfo);
        assert_eq!(info.schema, None);
        let (command, export) =
            parsed(&["diagnostics", "export", "/tmp/out.txt", "--state-dir", "/s"])
                .expect("export");
        assert_eq!(command, Command::DiagnosticsExport);
        assert_eq!(export.out, Some(PathBuf::from("/tmp/out.txt")));
    }

    #[test]
    fn non_utf8_options_are_refused_but_paths_may_be_any_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let raw = OsString::from_vec(b"/state/\xff".to_vec());
        let routed = route(&[OsString::from("serve"), "--state-dir".into(), raw.clone()]);
        assert_eq!(
            routed.expect("valid").1.state,
            Some(PathBuf::from(raw.clone()))
        );
        assert!(route(&[OsString::from("serve"), raw]).is_none());
    }

    #[test]
    fn credentials_name_a_built_in_or_custom_provider_variable() {
        let parse = |words: &[&str]| parse_credentials(&args(words));
        let (action, provider, variable) = parse(&["set", "anthropic"]).expect("set");
        assert!(matches!(action, CredentialsAction::Set));
        assert_eq!(provider, "anthropic");
        assert_eq!(
            variable.as_deref(),
            provider_env_vars("anthropic").and_then(|vars| vars.first().copied())
        );
        let (action, _, variable) = parse(&["clear", "anthropic"]).expect("clear");
        assert!(matches!(action, CredentialsAction::Clear));
        assert_eq!(
            variable, None,
            "clearing a built-in provider clears every variable"
        );
        assert!(parse(&["set", "anthropic", "--variable", "NOT_ITS_VARIABLE"]).is_err());
        assert!(
            parse(&["set", "my-llm"]).is_err(),
            "a custom provider names its variable"
        );
        let custom =
            parse(&["set", "my-llm", "--variable", "LADE_PROVIDER_MY_LLM"]).expect("custom");
        assert_eq!(custom.2.as_deref(), Some("LADE_PROVIDER_MY_LLM"));
        assert!(parse(&["set", "my-llm", "--variable", "HOME"]).is_err());
        assert!(parse(&["set", FIXTURE_PROVIDER]).is_err());
        assert!(parse(&["set", "bad id!"]).is_err());
        assert!(parse(&["rotate", "anthropic"]).is_err());
        assert!(parse(&["set"]).is_err());
    }

    #[test]
    fn a_key_is_one_non_empty_line_without_nul_bytes() {
        assert_eq!(one_line_key(b"secret\n".to_vec()), Some(b"secret".to_vec()));
        assert_eq!(
            one_line_key(b"secret\r\n".to_vec()),
            Some(b"secret".to_vec())
        );
        assert_eq!(one_line_key(b"secret".to_vec()), Some(b"secret".to_vec()));
        for bad in [&b""[..], b"\n", b"two\nlines", b"nul\0byte", b"cr\rinside"] {
            assert_eq!(one_line_key(bad.to_vec()), None, "{bad:?}");
        }
        assert!(one_line_key(vec![b'k'; MAX_SECRET_BYTES]).is_some());
        assert_eq!(one_line_key(vec![b'k'; MAX_SECRET_BYTES + 1]), None);
    }
}
