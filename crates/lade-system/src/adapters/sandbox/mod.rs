//! bubblewrap sandbox construction for the pi worker and verification runs.
//!
//! The command line is `prlimit <limits> -- bwrap <isolation> -- <argv>`: `prlimit` sets the
//! resource limits on itself and `exec`s bwrap, so the limits are inherited by everything inside.
//! Isolation: every namespace unshared (network always), nested user namespaces disabled, all
//! capabilities dropped, a new session (no controlling-terminal injection), `--die-with-parent`
//! plus the PID namespace so every descendant dies with the sandbox or with the supervisor, a
//! read-only root built only from explicit binds, private `/tmp`, `/proc` and `/dev`, and a
//! cleared environment with an explicit allowlist (bwrap additionally exports `PWD`). There is no
//! unsandboxed fallback: when [`sandbox_available`] fails, execution is blocked.
//!
//! Known gap: if the supervisor dies within the first milliseconds of a launch, before bwrap has
//! armed its parent-death signals, the sandbox can outlive it (observed with bubblewrap 0.9). The
//! service must therefore sweep stale sandboxes on start rather than rely on `--die-with-parent`
//! alone.
//!
//! File descriptors: children receive only stdin/stdout/stderr. The standard library opens every
//! descriptor with `O_CLOEXEC`, and [`run_bounded`] passes explicit pipes; nothing else the
//! service holds (control socket, database, capability file) is inherited.

mod run;

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub use run::{RunOutcome, run_bounded};

/// Default wall-clock timeout for verification runs (§7).
pub const VERIFICATION_TIMEOUT: Duration = Duration::from_secs(900);
/// Default captured output for verification runs (tail retained, §7).
pub const VERIFICATION_OUTPUT_LIMIT: usize = 4 << 20;

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;

/// Host system paths bound read-only when present.
const SYSTEM_DIRS: &[&str] = &["/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32"];
const SYSTEM_FILES: &[&str] = &[
    "/etc/ssl",
    "/etc/ca-certificates",
    "/etc/resolv.conf",
    "/etc/hosts",
    "/etc/passwd",
    "/etc/group",
    "/etc/alternatives",
    "/etc/nsswitch.conf",
    "/etc/ld.so.cache",
];
const SANDBOX_PATH: &str = "/usr/bin:/bin";

/// Resource limits applied through `prlimit` (soft = hard).
///
/// `processes` is `RLIMIT_NPROC`, which the kernel counts per real UID (per user namespace on
/// Linux ≥ 5.14) and does not enforce for host root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub address_space_bytes: u64,
    pub processes: u64,
    pub open_files: u64,
    pub file_size_bytes: u64,
    pub cpu_seconds: Option<u64>,
}

impl Limits {
    /// Worker defaults: 4 GiB address space, 512 processes, 1024 files, 512 MiB file size.
    pub const WORKER: Self = Self {
        address_space_bytes: 4 * GIB,
        processes: 512,
        open_files: 1024,
        file_size_bytes: 512 * MIB,
        cpu_seconds: None,
    };

    /// Verification defaults: worker limits plus 600 s CPU time.
    pub const VERIFICATION: Self = Self {
        cpu_seconds: Some(600),
        ..Self::WORKER
    };

    fn prlimit_args(&self) -> Vec<OsString> {
        let mut args = vec![
            format!("--as={}", self.address_space_bytes),
            format!("--nproc={}", self.processes),
            format!("--nofile={}", self.open_files),
            format!("--fsize={}", self.file_size_bytes),
        ];
        if let Some(cpu) = self.cpu_seconds {
            args.push(format!("--cpu={cpu}"));
        }
        args.into_iter().map(OsString::from).collect()
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::WORKER
    }
}

#[derive(Debug)]
pub enum SandboxError {
    ToolMissing {
        tool: &'static str,
    },
    InvalidSpec {
        what: &'static str,
    },
    /// The probe sandbox did not run successfully. `output` is a bounded diagnostic tail.
    Unavailable {
        exit_code: Option<i32>,
        output: Vec<u8>,
    },
    Io {
        kind: std::io::ErrorKind,
    },
}

impl SandboxError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ToolMissing { .. } => "sandbox_tool_missing",
            Self::InvalidSpec { .. } => "sandbox_invalid_spec",
            Self::Unavailable { .. } => "sandbox_unavailable",
            Self::Io { .. } => "sandbox_io",
        }
    }
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ToolMissing { tool } => write!(f, "{} ({tool})", self.code()),
            Self::InvalidSpec { what } => write!(f, "{} ({what})", self.code()),
            _ => f.write_str(self.code()),
        }
    }
}

impl std::error::Error for SandboxError {}

/// Absolute paths of the external helpers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxTools {
    pub bwrap: PathBuf,
    pub prlimit: PathBuf,
}

impl SandboxTools {
    /// Find `bwrap` and `prlimit` on `PATH` (then `/usr/bin`, `/bin`).
    pub fn locate() -> Result<Self, SandboxError> {
        Ok(Self {
            bwrap: find_program("bwrap").ok_or(SandboxError::ToolMissing { tool: "bwrap" })?,
            prlimit: find_program("prlimit")
                .ok_or(SandboxError::ToolMissing { tool: "prlimit" })?,
        })
    }
}

pub(crate) fn find_program(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain([PathBuf::from("/usr/bin"), PathBuf::from("/bin")])
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| {
            fs::metadata(candidate)
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// A complete sandbox description. Paths are bound at the same location inside the sandbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxSpec {
    ro_binds: Vec<PathBuf>,
    rw_binds: Vec<PathBuf>,
    chdir: PathBuf,
    env: Vec<(OsString, OsString)>,
    /// Variables passed through the outer process environment instead of the bwrap command
    /// line, so their values never appear in a world-readable `/proc/<pid>/cmdline`.
    secret_env: Vec<(OsString, Redacted)>,
    argv: Vec<OsString>,
    limits: Limits,
}

impl SandboxSpec {
    /// `argv[0]` must be an absolute path visible inside the sandbox; `chdir` must be absolute.
    pub fn new(argv: Vec<OsString>, chdir: &Path, limits: Limits) -> Result<Self, SandboxError> {
        if !argv.first().is_some_and(|a| Path::new(a).is_absolute()) {
            return Err(SandboxError::InvalidSpec { what: "argv" });
        }
        if !chdir.is_absolute() {
            return Err(SandboxError::InvalidSpec { what: "chdir" });
        }
        Ok(Self {
            ro_binds: Vec::new(),
            rw_binds: Vec::new(),
            chdir: chdir.to_path_buf(),
            env: Vec::new(),
            secret_env: Vec::new(),
            argv,
            limits,
        })
    }

    /// Bind an existing absolute path read-only.
    pub fn ro_bind(&mut self, path: &Path) -> Result<&mut Self, SandboxError> {
        self.ro_binds.push(checked_bind(path)?);
        Ok(self)
    }

    /// Bind an existing absolute path read-write. Read-write binds are applied after read-only
    /// ones, so a writable worktree may live below a read-only directory.
    pub fn rw_bind(&mut self, path: &Path) -> Result<&mut Self, SandboxError> {
        self.rw_binds.push(checked_bind(path)?);
        Ok(self)
    }

    /// Add one allowlisted environment variable (`[A-Za-z_][A-Za-z0-9_]*`, set once).
    pub fn env(&mut self, name: &str, value: impl AsRef<OsStr>) -> Result<&mut Self, SandboxError> {
        self.check_env_name(name)?;
        self.env.push((name.into(), value.as_ref().to_os_string()));
        Ok(self)
    }

    /// Add one allowlisted secret variable (for example a provider API key). It reaches the
    /// sandbox through the environment of the `prlimit`/`bwrap` processes, which only the same
    /// user can read, never through their argument vectors.
    pub fn secret_env(
        &mut self,
        name: &str,
        value: impl AsRef<OsStr>,
    ) -> Result<&mut Self, SandboxError> {
        self.check_env_name(name)?;
        self.secret_env
            .push((name.into(), Redacted(value.as_ref().to_os_string())));
        Ok(self)
    }

    fn check_env_name(&self, name: &str) -> Result<(), SandboxError> {
        let valid = name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
        let duplicate = self
            .env
            .iter()
            .map(|(k, _)| k)
            .chain(self.secret_env.iter().map(|(k, _)| k))
            .any(|k| k == name);
        if !valid || duplicate {
            return Err(SandboxError::InvalidSpec { what: "env" });
        }
        Ok(())
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// The bwrap arguments (without the program name).
    pub fn bwrap_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = [
            "--unshare-all",
            "--unshare-user",
            "--disable-userns",
            "--unshare-net",
            "--die-with-parent",
            "--new-session",
            "--cap-drop",
            "ALL",
            "--hostname",
            "lade-sandbox",
            "--ro-bind",
            "/usr",
            "/usr",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        let mut push = |items: &[&OsStr]| args.extend(items.iter().map(|s| s.to_os_string()));
        for dir in SYSTEM_DIRS {
            let path = Path::new(dir);
            match fs::symlink_metadata(path) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    if let Ok(target) = fs::read_link(path) {
                        push(&["--symlink".as_ref(), target.as_os_str(), path.as_os_str()]);
                    }
                }
                Ok(meta) if meta.is_dir() => {
                    push(&["--ro-bind".as_ref(), path.as_os_str(), path.as_os_str()]);
                }
                _ => {}
            }
        }
        for file in SYSTEM_FILES {
            push(&["--ro-bind-try".as_ref(), file.as_ref(), file.as_ref()]);
        }
        push(&[
            "--proc".as_ref(),
            "/proc".as_ref(),
            "--dev".as_ref(),
            "/dev".as_ref(),
            "--tmpfs".as_ref(),
            "/tmp".as_ref(),
        ]);
        for path in &self.ro_binds {
            push(&["--ro-bind".as_ref(), path.as_os_str(), path.as_os_str()]);
        }
        for path in &self.rw_binds {
            push(&["--bind".as_ref(), path.as_os_str(), path.as_os_str()]);
        }
        push(&[
            "--remount-ro".as_ref(),
            "/".as_ref(),
            "--chdir".as_ref(),
            self.chdir.as_os_str(),
        ]);
        // With secret variables the outer environment set by `command()` holds exactly those
        // (it is cleared first), so bwrap must keep it instead of clearing it.
        if self.secret_env.is_empty() {
            push(&["--clearenv".as_ref()]);
        }
        for (key, value) in &self.env {
            push(&["--setenv".as_ref(), key.as_os_str(), value.as_os_str()]);
        }
        args.push("--".into());
        args.extend(self.argv.iter().cloned());
        args
    }

    /// `prlimit … -- bwrap … -- argv`, with the outer environment cleared (except the secret
    /// variables) and stdin closed.
    pub fn command(&self, tools: &SandboxTools) -> Command {
        let mut cmd = Command::new(&tools.prlimit);
        cmd.env_clear()
            .envs(self.secret_env.iter().map(|(k, v)| (k, &v.0)))
            .current_dir("/")
            .stdin(std::process::Stdio::null())
            .args(self.limits.prlimit_args())
            .arg("--")
            .arg(&tools.bwrap)
            .args(self.bwrap_args());
        cmd
    }
}

/// A secret value whose `Debug` output is redacted.
#[derive(Clone, PartialEq, Eq)]
struct Redacted(OsString);

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

fn checked_bind(path: &Path) -> Result<PathBuf, SandboxError> {
    if !path.is_absolute() {
        return Err(SandboxError::InvalidSpec { what: "bind path" });
    }
    if fs::symlink_metadata(path).is_err() {
        return Err(SandboxError::InvalidSpec {
            what: "bind source",
        });
    }
    Ok(path.to_path_buf())
}

/// Inputs for one pi worker sandbox. `Debug` redacts credential values.
#[derive(Clone)]
pub struct WorkerLaunch {
    /// Node executable; its installation prefix (`<prefix>/bin/node`) is bound read-only.
    pub node: PathBuf,
    /// The LADE worker package, bound read-only.
    pub worker_dir: PathBuf,
    /// Worker entry script (inside `worker_dir`).
    pub entry: PathBuf,
    /// Node runtime options placed before the entry script (for example
    /// `--disable-wasm-trap-handler`).
    pub node_args: Vec<OsString>,
    /// Script arguments after the entry.
    pub args: Vec<OsString>,
    /// Objective worktree: read-write, working directory.
    pub worktree: PathBuf,
    /// Main repository Git common directory: read-only (the worker cannot commit).
    pub repo_common_dir: PathBuf,
    /// Objective session directory: read-write, `HOME`.
    pub session_dir: PathBuf,
    /// Gateway Unix socket: the only network path, exported as `LADE_GATEWAY_SOCKET`.
    pub gateway_socket: PathBuf,
    /// `PI_*` agent-directory variables.
    pub pi_env: Vec<(String, OsString)>,
    /// Only the selected provider's credential variables (passed as secret environment, never
    /// on a command line).
    pub credentials: Vec<(String, OsString)>,
    /// Further read-only paths (for example vendored packages the worker resolves).
    pub extra_ro: Vec<PathBuf>,
    pub limits: Limits,
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
            .finish()
    }
}

/// Build the worker sandbox (`HOME` = session dir, `TERM=dumb`, explicit allowlist only).
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
    for (name, value) in &launch.credentials {
        spec.secret_env(name, value)?;
    }
    Ok(spec)
}

/// Inputs for one verification run over a disposable candidate copy.
#[derive(Clone, Debug)]
pub struct VerificationLaunch {
    /// The disposable verification copy: read-write, working directory.
    pub work_dir: PathBuf,
    /// Absolute program path and arguments.
    pub argv: Vec<OsString>,
    pub extra_ro: Vec<PathBuf>,
    pub env: Vec<(String, OsString)>,
    pub limits: Limits,
}

/// Build a verification sandbox (`HOME=/tmp`, no network, verification limits).
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

/// Run a trivial command in a fully configured sandbox (worker limits). Any failure means
/// execution must be blocked.
pub fn sandbox_available(tools: &SandboxTools) -> Result<(), SandboxError> {
    let program = find_program("true").ok_or(SandboxError::ToolMissing { tool: "true" })?;
    let spec = SandboxSpec::new(
        vec![program.into_os_string()],
        Path::new("/"),
        Limits::WORKER,
    )?;
    let outcome = run_bounded(spec.command(tools), Duration::from_secs(10), 2048)
        .map_err(|e| SandboxError::Io { kind: e.kind() })?;
    if outcome.exit_code == Some(0) && !outcome.timed_out {
        Ok(())
    } else {
        Err(SandboxError::Unavailable {
            exit_code: outcome.exit_code,
            output: outcome.output_tail,
        })
    }
}
