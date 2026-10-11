//! `laded`: the authoritative LADE service.
//!
//! ```text
//! laded --version
//! laded serve [--state-dir DIR] [--runtime-dir DIR]
//! ```
//!
//! Exit statuses: 0 version, 64 usage error, 78 refused to start (unsafe directory, unopenable or
//! corrupt store, another instance), 70 internal failure. Diagnostics go to stderr only.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use lade_system::adapters::sandbox::{OBJECTIVE_CGROUP_LIMITS, SandboxCgroups, cgroup};
use lade_system::service::{self, ServiceConfig, StartError};
use lade_system::supervision::{
    GATEWAY_SOCKET_DIR, LaunchError, LaunchRequest, LauncherConfig, SupervisorEvent, WorkerControl,
    WorkerLauncher, default_launcher,
};

const USAGE: &str =
    "usage: laded serve [--state-dir DIR] [--runtime-dir DIR]\n       laded --version";

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
