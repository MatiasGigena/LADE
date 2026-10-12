//! bubblewrap sandbox construction for the pi worker, contained terminals and verification runs.
//!
//! The command line is `prlimit <limits> -- bwrap <isolation> -- bash -c <gate> lade-gate rm env -u
//! PWD -- <argv>`: `prlimit` sets the resource limits on itself and `exec`s bwrap, so the limits are inherited by
//! everything inside. Isolation: every namespace unshared (network always), nested user
//! namespaces disabled, all capabilities dropped, a new session (no controlling-terminal
//! injection), `--die-with-parent` plus the PID namespace so every descendant dies with the
//! sandbox or with the supervisor, a read-only root built only from explicit binds, a private
//! size-limited `/tmp` ([`SANDBOX_TMP_BYTES`]), private `/proc` and `/dev`, the [`seccomp`] filter,
//! and a cleared environment with an explicit allowlist (`env -u PWD` removes the `PWD` bwrap
//! exports for `--chdir`). There is no unsandboxed fallback: when [`sandbox_available`] fails,
//! execution is blocked.
//!
//! Launch gate: bwrap copies a pipe into `/tmp/.lade-gate` (`--file`) while it sets up the
//! sandbox, so it blocks until the supervisor opens the [`Gate`] (writes `go`) or loses it (the
//! pipe closes empty); meanwhile only the outer bwrap and its fork, the future PID-namespace init,
//! exist. A small bash gate inside the sandbox then starts the program only after `go`; an empty
//! gate ends the sandbox with status 125 before anything runs. Supervisors record the sandbox
//! (pidfile, cgroup placement) before opening the gate, so the program never runs unrecorded or
//! unplaced, and a supervisor that dies first leaves nothing running. Every command line also
//! carries a marker argument ([`SandboxSpec::marker`]) by which the startup sweep finds sandboxes
//! whose supervisor died after opening the gate but before bwrap armed its parent-death signals.
//!
//! File descriptors: children receive only stdin/stdout/stderr. The standard library opens every
//! descriptor with `O_CLOEXEC`; the two pipe ends bwrap reads (`--seccomp`, `--file`) are made
//! inheritable only while one sandbox is spawned (sandbox spawns are serialized) and bwrap closes
//! both before the sandboxed program starts. bwrap forwards any other inheritable descriptor, so
//! the gate closes every descriptor above stderr right before `exec`: the program gets exactly
//! stdin, stdout and stderr even if `laded` inherited a stray descriptor. Nothing else the service
//! holds (control socket, database, capability file) is inherited.

pub mod cgroup;
pub mod limits;
mod run;
pub mod seccomp;

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::{self, PipeReader, PipeWriter, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

pub use cgroup::{CgroupLeaf, SandboxCgroups};
pub use limits::{
    CgroupLimits, Limits, OBJECTIVE_CGROUP_LIMITS, SANDBOX_TMP_BYTES, STORAGE_CHECK_INTERVAL,
    TERMINAL_LIMITS, VERIFICATION_OUTPUT_LIMIT, VERIFICATION_TIMEOUT, WORKER_LIMITS,
    WRITABLE_STORAGE_BYTES,
};
pub use run::{RunOutcome, run_bounded, run_bounded_with};

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

/// Test-only switch (`LADE_TEST_FIXTURE_PROVIDER=1`) that makes the worker and its model catalogue
/// register the offline `lade-fixture` model; set in a sandbox only when the service runs with it.
pub const FIXTURE_SWITCH: &str = "LADE_TEST_FIXTURE_PROVIDER";

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

/// Programs that remove `PWD` (exported by bwrap for `--chdir`) before the sandboxed program.
const ENV_PROGRAMS: &[&str] = &["/usr/bin/env", "/bin/env"];
/// Shells that run the launch gate inside the sandbox. bash (Essential on Debian, Ubuntu and
/// Mint) is required: closing descriptors above 9 needs multi-digit redirections, which POSIX
/// `sh` (dash) does not support.
const SHELL_PROGRAMS: &[&str] = &["/usr/bin/bash", "/bin/bash"];
const RM_PROGRAMS: &[&str] = &["/usr/bin/rm", "/bin/rm"];
/// Where bwrap writes the launch gate's content inside the sandbox (private tmpfs).
const GATE_FILE: &str = "/tmp/.lade-gate";
/// The launch gate: the sandboxed program starts only if the supervisor wrote `go`. A gate the
/// supervisor never opened (it died, or gave up on the launch) is empty, and the sandbox ends
/// with status 125 before running anything. The file is removed first, then every descriptor
/// above stderr is closed, so the program gets exactly stdin/stdout/stderr whatever bwrap
/// passed through (bwrap forwards every inheritable descriptor it received, including ones a
/// concurrent spawn or `laded`'s own parent left inheritable). `$1` is `rm`.
const GATE_SCRIPT: &str = r#"IFS= read -r gate < /tmp/.lade-gate; "$1" -f /tmp/.lade-gate; shift; [ "$gate" = go ] || exit 125; for fd in /proc/$$/fd/*; do fd=${fd##*/}; [ "$fd" -gt 2 ] 2>/dev/null && eval "exec $fd>&-"; done; exec "$@""#;

/// Serializes sandbox spawns: the pipe ends bwrap reads are inheritable only inside this lock, so
/// no sandbox can inherit another sandbox's descriptors.
static SPAWN: Mutex<()> = Mutex::new(());

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
    env_program: PathBuf,
    shell_program: PathBuf,
    rm_program: PathBuf,
    limits: Limits,
    tmp_bytes: u64,
    marker: Option<OsString>,
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
        let first = |candidates: &[&str], tool| {
            candidates
                .iter()
                .map(PathBuf::from)
                .find(|path| path.is_file())
                .ok_or(SandboxError::ToolMissing { tool })
        };
        let env_program = first(ENV_PROGRAMS, "env")?;
        let shell_program = first(SHELL_PROGRAMS, "bash")?;
        let rm_program = first(RM_PROGRAMS, "rm")?;
        Ok(Self {
            ro_binds: Vec::new(),
            rw_binds: Vec::new(),
            chdir: chdir.to_path_buf(),
            env: Vec::new(),
            secret_env: Vec::new(),
            argv,
            env_program,
            shell_program,
            rm_program,
            limits,
            tmp_bytes: SANDBOX_TMP_BYTES,
            marker: None,
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

    /// Size of the private `/tmp` tmpfs (default [`SANDBOX_TMP_BYTES`]).
    pub fn tmp_size(&mut self, bytes: u64) -> &mut Self {
        self.tmp_bytes = bytes;
        self
    }

    /// Tags the outer `prlimit`/`bwrap` command lines with `marker` (a `LADE_` variable name,
    /// passed as the no-op `--unsetenv <marker>`) so a sweep can find this sandbox without a
    /// pidfile. The sandboxed program can neither see nor change the outer processes.
    pub fn marker(&mut self, marker: &str) -> Result<&mut Self, SandboxError> {
        let valid = marker.starts_with("LADE_")
            && marker.len() <= 64
            && marker
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if !valid {
            return Err(SandboxError::InvalidSpec { what: "marker" });
        }
        self.marker = Some(marker.into());
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

    /// The bwrap arguments (without the program name); `seccomp` and `gate` are the descriptor
    /// numbers bwrap reads the filter and the launch gate from.
    fn bwrap_args(&self, seccomp: i32, gate: i32) -> Vec<OsString> {
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
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        if let Some(marker) = &self.marker {
            args.extend(["--unsetenv".into(), marker.clone()]);
        }
        args.extend(["--seccomp".into(), seccomp.to_string().into()]);
        args.extend(["--ro-bind", "/usr", "/usr"].map(OsString::from));
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
        let tmp_bytes = OsString::from(self.tmp_bytes.to_string());
        let gate = OsString::from(gate.to_string());
        push(&[
            "--proc".as_ref(),
            "/proc".as_ref(),
            "--dev".as_ref(),
            "/dev".as_ref(),
            "--size".as_ref(),
            &tmp_bytes,
            "--tmpfs".as_ref(),
            "/tmp".as_ref(),
            // bwrap copies the gate pipe into this file during setup, blocking until the
            // supervisor opens (writes `go`) or loses (closes) it.
            "--perms".as_ref(),
            "0600".as_ref(),
            "--file".as_ref(),
            &gate,
            GATE_FILE.as_ref(),
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
        push(&[
            "--".as_ref(),
            self.shell_program.as_os_str(),
            "-c".as_ref(),
            GATE_SCRIPT.as_ref(),
            "lade-gate".as_ref(),
            self.rm_program.as_os_str(),
            self.env_program.as_os_str(),
            "-u".as_ref(),
            "PWD".as_ref(),
            // bash, which runs the gate, exports SHLVL.
            "-u".as_ref(),
            "SHLVL".as_ref(),
            "--".as_ref(),
        ]);
        args.extend(self.argv.iter().cloned());
        args
    }

    /// `prlimit … -- bwrap … -- bash -c <gate> lade-gate rm env -u PWD -u SHLVL -- argv`, with the outer
    /// environment cleared (except the secret variables), stdin closed, the seccomp program on a
    /// pipe (`--seccomp`) and the launch gate pipe (`--file`). Fails when this architecture has no
    /// seccomp program.
    pub fn command(&self, tools: &SandboxTools) -> Result<SandboxCommand, SandboxError> {
        let program = seccomp::program().ok_or(SandboxError::InvalidSpec { what: "seccomp" })?;
        let io = |e: io::Error| SandboxError::Io { kind: e.kind() };
        let (filter, mut filler) = io::pipe().map_err(io)?;
        // The program (a few hundred bytes) fits in the pipe buffer; closing the write end
        // gives bwrap its end of file.
        filler.write_all(&program).map_err(io)?;
        drop(filler);
        let (gate_reader, gate) = io::pipe().map_err(io)?;
        let args = self.bwrap_args(filter.as_raw_fd(), gate_reader.as_raw_fd());
        let mut command = Command::new(&tools.prlimit);
        command
            .env_clear()
            .envs(self.secret_env.iter().map(|(k, v)| (k, &v.0)))
            .current_dir("/")
            .stdin(std::process::Stdio::null())
            .args(self.limits.prlimit_args())
            .arg("--")
            .arg(&tools.bwrap)
            .args(args);
        Ok(SandboxCommand {
            command,
            inherit: vec![filter, gate_reader],
            gate: Some(gate),
        })
    }
}

/// A sandbox command ready to spawn (see [`SandboxSpec::command`]). Configure stdio through
/// [`Self::command_mut`]; spawn only through [`Self::spawn`], which hands bwrap its descriptors.
#[derive(Debug)]
pub struct SandboxCommand {
    command: Command,
    inherit: Vec<PipeReader>,
    gate: Option<PipeWriter>,
}

impl From<Command> for SandboxCommand {
    /// A plain command without descriptors or gate (for [`run_bounded`] outside a sandbox).
    fn from(command: Command) -> Self {
        Self {
            command,
            inherit: Vec::new(),
            gate: None,
        }
    }
}

impl SandboxCommand {
    pub fn command_mut(&mut self) -> &mut Command {
        &mut self.command
    }

    pub fn get_command(&self) -> &Command {
        &self.command
    }

    /// Spawns the sandbox. bwrap then waits on the returned [`Gate`].
    pub fn spawn(self) -> io::Result<(Child, Gate)> {
        let Self {
            mut command,
            inherit,
            gate,
        } = self;
        spawn_inheriting(&inherit, || command.spawn()).map(|child| (child, Gate(gate)))
    }

    /// Spawns `supervisor` instead: a process that itself executes this sandbox's command line
    /// (for example a shell in a supervisor-loss test), with bwrap's descriptors inheritable.
    pub fn spawn_via(self, mut supervisor: Command) -> io::Result<(Child, Gate)> {
        let Self { inherit, gate, .. } = self;
        spawn_inheriting(&inherit, || supervisor.spawn()).map(|child| (child, Gate(gate)))
    }
}

fn spawn_inheriting(
    inherit: &[PipeReader],
    spawn: impl FnOnce() -> io::Result<Child>,
) -> io::Result<Child> {
    let _serial = SPAWN.lock().unwrap_or_else(PoisonError::into_inner);
    for fd in inherit {
        rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::empty())?;
    }
    let spawned = spawn();
    // Restore close-on-exec before releasing the lock, so no later spawn can inherit them.
    for fd in inherit {
        let _ = rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::CLOEXEC);
    }
    spawned
}

/// The launch gate of a spawned sandbox: the sandboxed program starts only after [`Gate::open`].
/// Dropping an unopened gate (or the supervisor dying) makes the sandbox end with status 125
/// without running the program.
#[derive(Debug)]
pub struct Gate(Option<PipeWriter>);

impl Gate {
    /// Lets the sandboxed program start.
    pub fn open(&mut self) -> io::Result<()> {
        match self.0.take() {
            Some(mut writer) => writer.write_all(b"go\n"),
            None => Ok(()),
        }
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
    /// Test-only: export [`FIXTURE_SWITCH`] so the worker registers the offline fixture model.
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
    if launch.test_fixture_provider {
        spec.env(FIXTURE_SWITCH, "1")?;
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

/// Inputs for one contained terminal (IMP-012): an interactive shell over a service-owned PTY in
/// the same isolation as the worker, without network, credentials or the session directory.
#[derive(Clone, Debug)]
pub struct TerminalLaunch {
    /// Shell executable (absolute, under `/usr` or `/bin`, which are bound read-only).
    pub shell: PathBuf,
    /// `setsid` from util-linux: makes the PTY the shell's controlling terminal inside the
    /// sandbox (job control, `^C`). Without it the shell runs without a controlling terminal.
    pub setsid: Option<PathBuf>,
    /// Objective worktree: working directory; read-write only when `writable`.
    pub worktree: PathBuf,
    /// Repository common directory (`.git`), read-only: the terminal cannot commit or move refs.
    pub repo_common_dir: PathBuf,
    /// `false` while review writer exclusion holds (frozen/approved candidate): the worktree is
    /// bound read-only, so no process of the terminal can change the reviewed content.
    pub writable: bool,
    pub limits: Limits,
}

/// Build a terminal sandbox: `HOME=/tmp` (private tmpfs), `TERM=vt100`, worker limits, no
/// network, no credentials, the worktree read-write or read-only.
pub fn terminal_spec(launch: &TerminalLaunch) -> Result<SandboxSpec, SandboxError> {
    let mut argv = Vec::new();
    if let Some(setsid) = &launch.setsid {
        argv.extend([
            setsid.clone().into_os_string(),
            "--ctty".into(),
            "--wait".into(),
        ]);
    }
    argv.extend([launch.shell.clone().into_os_string(), "-i".into()]);
    let mut spec = SandboxSpec::new(argv, &launch.worktree, launch.limits)?;
    spec.ro_bind(&launch.repo_common_dir)?;
    if launch.writable {
        spec.rw_bind(&launch.worktree)?;
    } else {
        spec.ro_bind(&launch.worktree)?;
    }
    let (mode, prompt) = if launch.writable {
        ("read_write", "[lade] \\W\\$ ")
    } else {
        ("read_only", "[lade read-only] \\W\\$ ")
    };
    spec.env("HOME", "/tmp")?
        .env("PATH", SANDBOX_PATH)?
        .env("LANG", "C.UTF-8")?
        .env("TERM", "vt100")?
        .env("PS1", prompt)?
        .env("LADE_TERMINAL", mode)?;
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
    let outcome = run_bounded(spec.command(tools)?, Duration::from_secs(10), 2048)
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
