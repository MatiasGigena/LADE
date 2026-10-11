//! `laded`: the authoritative LADE service.
//!
//! ```text
//! laded --version
//! laded serve [--state-dir DIR] [--runtime-dir DIR]
//! laded credentials set <provider> [--variable NAME]
//! laded credentials clear <provider> [--variable NAME]
//! ```
//!
//! Exit statuses: 0 version, 64 usage error, 78 refused to start (unsafe directory, unopenable or
//! corrupt store, another instance), 70 internal failure; for `credentials`, 65 an empty or
//! multi-line key and 69 the Secret Service is unavailable (the reason and the credentials-file
//! alternative are printed; nothing is written anywhere else). Diagnostics go to stderr only.

use std::ffi::OsString;
use std::io::{self, BufRead, Read};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use rustix::termios::{LocalModes, OptionalActions, Termios, isatty, tcgetattr, tcsetattr};

use lade_system::adapters::sandbox::{OBJECTIVE_CGROUP_LIMITS, SandboxCgroups, cgroup};
use lade_system::service::{self, ServiceConfig, StartError};
use lade_system::supervision::credentials::{FIXTURE_PROVIDER, custom_var, provider_env_vars};
use lade_system::supervision::secret_service::{
    MAX_SECRET_BYTES, SecretService, valid_provider_id,
};
use lade_system::supervision::{
    GATEWAY_SOCKET_DIR, LaunchError, LaunchRequest, LauncherConfig, SupervisorEvent, WorkerControl,
    WorkerLauncher, default_launcher,
};

const USAGE: &str = "usage: laded serve [--state-dir DIR] [--runtime-dir DIR]
       laded credentials set|clear <provider> [--variable NAME]
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
}

fn parse_serve(args: &[OsString]) -> Option<Args> {
    let mut parsed = Args {
        state: None,
        runtime: None,
    };
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        let value = iter.next().map(PathBuf::from)?;
        match flag.to_str()? {
            "--state-dir" if parsed.state.is_none() => parsed.state = Some(value),
            "--runtime-dir" if parsed.runtime.is_none() => parsed.runtime = Some(value),
            _ => return None,
        }
    }
    Some(parsed)
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
    let Some((command, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(64);
    };
    if command == "credentials" {
        return credentials_command(rest);
    }
    if command != "serve" {
        eprintln!("{USAGE}");
        return ExitCode::from(64);
    }
    let Some(parsed) = parse_serve(rest) else {
        eprintln!("{USAGE}");
        return ExitCode::from(64);
    };
    let Some(config) = ServiceConfig::from_environment(parsed.state, parsed.runtime) else {
        eprintln!("laded: no state directory (set XDG_STATE_HOME or HOME, or pass --state-dir)");
        return ExitCode::from(64);
    };
    // Every file the service creates is private to its user.
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    if !config.state_dir.is_absolute() || !config.runtime_dir.is_absolute() {
        eprintln!("laded: --state-dir and --runtime-dir must be absolute paths");
        return ExitCode::from(64);
    }
    let config = ServiceConfig {
        cgroups: delegated_cgroups(),
        ..config
    };
    let launcher = make_launcher(&config);
    match service::start(config, launcher) {
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
    }
}
