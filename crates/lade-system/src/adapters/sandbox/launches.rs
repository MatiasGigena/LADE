//! The sandboxes LADE runs: an agent worker, a verification run and a developer terminal.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use super::spec::{ENV_PROGRAMS, SANDBOX_PATH};
use super::{FIXTURE_SWITCH, Limits, SandboxError, SandboxSpec};

#[derive(Clone)]
pub struct WorkerLaunch {
    pub node: PathBuf,
    pub worker_dir: PathBuf,
    pub entry: PathBuf,
    pub node_args: Vec<OsString>,
    pub args: Vec<OsString>,
    pub worktree: PathBuf,
    pub repo_common_dir: PathBuf,
    pub session_dir: PathBuf,
    pub gateway_socket: PathBuf,
    pub pi_env: Vec<(String, OsString)>,
    pub credentials: Vec<(String, OsString)>,
    pub extra_ro: Vec<PathBuf>,
    pub limits: Limits,
    pub test_fixture_provider: bool,
}

impl fmt::Debug for WorkerLaunch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let credentials: Vec<&str> = self.credentials.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("WorkerLaunch")
            .field("node", &self.node)
            .field("worker_dir", &self.worker_dir)
            .field("entry", &self.entry)
            .field("node_args", &self.node_args)
            .field("args", &self.args)
            .field("worktree", &self.worktree)
            .field("repo_common_dir", &self.repo_common_dir)
            .field("session_dir", &self.session_dir)
            .field("gateway_socket", &self.gateway_socket)
            .field("pi_env", &self.pi_env)
            .field("credentials", &credentials)
            .field("extra_ro", &self.extra_ro)
            .field("limits", &self.limits)
            .field("test_fixture_provider", &self.test_fixture_provider)
            .finish()
    }
}

/// The sandbox for a pi worker: Node and the worker read-only, the worktree, session
/// directory and gateway socket writable, credentials as secret environment.
///
/// # Errors
///
/// Returns [`SandboxError::InvalidSpec`] when Node cannot be resolved, a bind source is missing, or
/// a pi variable does not start with `PI_`, and the errors of [`SandboxSpec::new`].
pub fn worker_spec(launch: &WorkerLaunch) -> Result<SandboxSpec, SandboxError> {
    let node = launch
        .node
        .canonicalize()
        .map_err(|_| SandboxError::InvalidSpec { what: "node" })?;
    let prefix = node
        .parent()
        .and_then(Path::parent)
        .ok_or(SandboxError::InvalidSpec { what: "node" })?;
    let mut argv = vec![node.clone().into_os_string()];
    argv.extend(launch.node_args.iter().cloned());
    argv.push(launch.entry.clone().into());
    argv.extend(launch.args.iter().cloned());
    let mut spec = SandboxSpec::new(argv, &launch.worktree, launch.limits)?;
    spec.ro_bind(prefix)?
        .ro_bind(&launch.worker_dir)?
        .ro_bind(&launch.repo_common_dir)?;
    for path in &launch.extra_ro {
        spec.ro_bind(path)?;
    }
    spec.rw_bind(&launch.worktree)?
        .rw_bind(&launch.session_dir)?
        .rw_bind(&launch.gateway_socket)?;
    let mut path = prefix.join("bin").into_os_string();
    path.push(":");
    path.push(SANDBOX_PATH);
    spec.env("HOME", &launch.session_dir)?
        .env("PATH", path)?
        .env("LANG", "C.UTF-8")?
        .env("TERM", "dumb")?
        .env("LADE_GATEWAY_SOCKET", &launch.gateway_socket)?;
    for (name, value) in &launch.pi_env {
        if !name.starts_with("PI_") {
            return Err(SandboxError::InvalidSpec { what: "pi env" });
        }
        spec.env(name, value)?;
    }
    if launch.test_fixture_provider {
        spec.env(FIXTURE_SWITCH, "1")?;
    }
    for (name, value) in &launch.credentials {
        spec.secret_env(name, value)?;
    }
    Ok(spec)
}

#[derive(Clone, Debug)]
pub struct VerificationLaunch {
    pub work_dir: PathBuf,
    pub argv: Vec<OsString>,
    pub extra_ro: Vec<PathBuf>,
    pub env: Vec<(String, OsString)>,
    pub limits: Limits,
}

/// The sandbox for a verification command, with only its work directory writable.
///
/// # Errors
///
/// Returns the errors of [`SandboxSpec::new`], [`SandboxSpec::ro_bind`], [`SandboxSpec::rw_bind`]
/// and [`SandboxSpec::env`] for an invalid command, path or variable.
pub fn verification_spec(launch: &VerificationLaunch) -> Result<SandboxSpec, SandboxError> {
    let mut spec = SandboxSpec::new(launch.argv.clone(), &launch.work_dir, launch.limits)?;
    for path in &launch.extra_ro {
        spec.ro_bind(path)?;
    }
    spec.rw_bind(&launch.work_dir)?
        .env("HOME", "/tmp")?
        .env("PATH", SANDBOX_PATH)?
        .env("LANG", "C.UTF-8")?
        .env("TERM", "dumb")?;
    for (name, value) in &launch.env {
        spec.env(name, value)?;
    }
    Ok(spec)
}

#[derive(Clone, Debug)]
pub struct TerminalLaunch {
    pub shell: PathBuf,
    pub setsid: Option<PathBuf>,
    pub worktree: PathBuf,
    pub repo_common_dir: PathBuf,
    pub writable: bool,
    pub limits: Limits,
}

/// The sandbox for an interactive terminal on an Objective's worktree, writable only when
/// `launch.writable` is set.
///
/// # Errors
///
/// Returns [`SandboxError::ToolMissing`] when `env` is not installed, and the errors of
/// [`SandboxSpec::new`] and the binds for an invalid shell or path.
pub fn terminal_spec(launch: &TerminalLaunch) -> Result<SandboxSpec, SandboxError> {
    let (mode, prompt) = if launch.writable {
        ("read_write", "[lade] \\W\\$ ")
    } else {
        ("read_only", "[lade read-only] \\W\\$ ")
    };
    let env_program = ENV_PROGRAMS
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .ok_or(SandboxError::ToolMissing { tool: "env" })?;
    let mut argv = Vec::new();
    if let Some(setsid) = &launch.setsid {
        argv.extend([
            setsid.clone().into_os_string(),
            "--ctty".into(),
            "--wait".into(),
        ]);
    }
    argv.extend([
        env_program.into_os_string(),
        format!("PS1={prompt}").into(),
        launch.shell.clone().into_os_string(),
        "-i".into(),
    ]);
    let mut spec = SandboxSpec::new(argv, &launch.worktree, launch.limits)?;
    spec.ro_bind(&launch.repo_common_dir)?;
    if launch.writable {
        spec.rw_bind(&launch.worktree)?;
    } else {
        spec.ro_bind(&launch.worktree)?;
    }
    spec.env("HOME", "/tmp")?
        .env("PATH", SANDBOX_PATH)?
        .env("LANG", "C.UTF-8")?
        .env("TERM", "vt100")?
        .env("LADE_TERMINAL", mode)?;
    Ok(spec)
}
