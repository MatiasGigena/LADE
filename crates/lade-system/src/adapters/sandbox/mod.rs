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

pub use cgroup::{CgroupError, CgroupLeaf, SandboxCgroups};
pub use limits::{
    CgroupLimits, Limits, OBJECTIVE_CGROUP_LIMITS, SANDBOX_TMP_BYTES, STORAGE_CHECK_INTERVAL,
    TERMINAL_LIMITS, VERIFICATION_OUTPUT_LIMIT, VERIFICATION_TIMEOUT, WORKER_LIMITS,
    WRITABLE_STORAGE_BYTES,
};
pub use run::{RunOutcome, run_bounded, run_bounded_with};

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

pub const FIXTURE_SWITCH: &str = "LADE_TEST_FIXTURE_PROVIDER";

#[derive(Debug)]
pub enum SandboxError {
    ToolMissing {
        tool: &'static str,
    },
    InvalidSpec {
        what: &'static str,
    },
    Unavailable {
        exit_code: Option<i32>,
        output: Vec<u8>,
    },
    Io {
        kind: std::io::ErrorKind,
    },
}

impl SandboxError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::ToolMissing { .. } => "sandbox_tool_missing",
            Self::InvalidSpec { .. } => "sandbox_invalid_spec",
            Self::Unavailable { .. } => "sandbox_unavailable",
            Self::Io { .. } => "sandbox_io",
        }
    }
}

const PROBE_DIAGNOSTIC_LINES: usize = 4;

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ToolMissing { tool } => {
                write!(f, "{}: {tool} was not found on PATH", self.code())
            }
            Self::InvalidSpec { what } => {
                write!(f, "{}: the sandbox {what} is invalid", self.code())
            }
            Self::Unavailable { exit_code, output } => {
                write!(f, "{}: the probe sandbox ", self.code())?;
                match exit_code {
                    Some(code) => write!(f, "exited with status {code}")?,
                    None => f.write_str("was killed or timed out")?,
                }
                let text = String::from_utf8_lossy(output);
                let lines: Vec<&str> = text
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .collect();
                let tail = &lines[lines.len().saturating_sub(PROBE_DIAGNOSTIC_LINES)..];
                if tail.is_empty() {
                    f.write_str(" without output")
                } else {
                    write!(f, ": {}", tail.join(" | "))
                }
            }
            Self::Io { kind } => write!(f, "{}: {kind}", self.code()),
        }
    }
}

impl std::error::Error for SandboxError {}

impl From<SandboxError> for io::Error {
    fn from(error: SandboxError) -> Self {
        let kind = match &error {
            SandboxError::ToolMissing { .. } => io::ErrorKind::NotFound,
            SandboxError::InvalidSpec { .. } => io::ErrorKind::InvalidInput,
            SandboxError::Unavailable { .. } => io::ErrorKind::Other,
            SandboxError::Io { kind } => *kind,
        };
        io::Error::new(kind, error)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxTools {
    pub bwrap: PathBuf,
    pub prlimit: PathBuf,
}

impl SandboxTools {
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

const ENV_PROGRAMS: &[&str] = &["/usr/bin/env", "/bin/env"];
const SHELL_PROGRAMS: &[&str] = &["/usr/bin/bash", "/bin/bash"];
const RM_PROGRAMS: &[&str] = &["/usr/bin/rm", "/bin/rm"];
const GATE_FILE: &str = "/tmp/.lade-gate";
const GATE_SCRIPT: &str = r#"IFS= read -r gate < /tmp/.lade-gate; "$2" -f /tmp/.lade-gate; [ "$gate" = go ] || exit 125; ulimit -S -H -u "$1" || exit 125; shift 2; for fd in /proc/$$/fd/*; do fd=${fd##*/}; [ "$fd" -gt 2 ] 2>/dev/null && eval "exec $fd>&-"; done; exec "$@""#;

static SPAWN: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxSpec {
    ro_binds: Vec<PathBuf>,
    rw_binds: Vec<PathBuf>,
    chdir: PathBuf,
    env: Vec<(OsString, OsString)>,
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

    pub fn ro_bind(&mut self, path: &Path) -> Result<&mut Self, SandboxError> {
        self.ro_binds.push(checked_bind(path)?);
        Ok(self)
    }

    pub fn rw_bind(&mut self, path: &Path) -> Result<&mut Self, SandboxError> {
        self.rw_binds.push(checked_bind(path)?);
        Ok(self)
    }

    pub fn env(&mut self, name: &str, value: impl AsRef<OsStr>) -> Result<&mut Self, SandboxError> {
        self.check_env_name(name)?;
        self.env.push((name.into(), value.as_ref().to_os_string()));
        Ok(self)
    }

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

    pub fn tmp_size(&mut self, bytes: u64) -> &mut Self {
        self.tmp_bytes = bytes;
        self
    }

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

    #[must_use]
    pub fn limits(&self) -> Limits {
        self.limits
    }

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
        let processes = OsString::from(self.limits.processes.to_string());
        push(&[
            "--proc".as_ref(),
            "/proc".as_ref(),
            "--dev".as_ref(),
            "/dev".as_ref(),
            "--size".as_ref(),
            &tmp_bytes,
            "--tmpfs".as_ref(),
            "/tmp".as_ref(),
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
            &processes,
            self.rm_program.as_os_str(),
            self.env_program.as_os_str(),
            "-u".as_ref(),
            "PWD".as_ref(),
            "-u".as_ref(),
            "SHLVL".as_ref(),
            "--".as_ref(),
        ]);
        args.extend(self.argv.iter().cloned());
        args
    }

    pub fn command(&self, tools: &SandboxTools) -> Result<SandboxCommand, SandboxError> {
        let program = seccomp::program().ok_or(SandboxError::InvalidSpec { what: "seccomp" })?;
        let io = |e: io::Error| SandboxError::Io { kind: e.kind() };
        let (filter, mut filler) = io::pipe().map_err(io)?;
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

#[derive(Debug)]
pub struct SandboxCommand {
    command: Command,
    inherit: Vec<PipeReader>,
    gate: Option<PipeWriter>,
}

impl From<Command> for SandboxCommand {
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

    #[must_use]
    pub fn get_command(&self) -> &Command {
        &self.command
    }

    pub fn spawn(self) -> io::Result<(Child, Gate)> {
        let Self {
            mut command,
            inherit,
            gate,
        } = self;
        spawn_inheriting(&inherit, || command.spawn()).map(|child| (child, Gate(gate)))
    }

    /// Spawns a supervisor that runs this sandbox itself, instead of the sandbox directly:
    /// `supervisor` builds that process from the sandbox command, and the sandbox's inherited
    /// descriptors and gate carry over to it.
    pub fn spawn_via(
        self,
        supervisor: impl FnOnce(&Command) -> Command,
    ) -> io::Result<(Child, Gate)> {
        let Self {
            command,
            inherit,
            gate,
        } = self;
        let mut supervisor = supervisor(&command);
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
    for fd in inherit {
        let _ = rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::CLOEXEC);
    }
    spawned
}

#[derive(Debug)]
pub struct Gate(Option<PipeWriter>);

impl Gate {
    pub fn open(&mut self) -> io::Result<()> {
        match self.0.take() {
            Some(mut writer) => writer.write_all(b"go\n"),
            None => Ok(()),
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::SandboxError;

    #[test]
    fn a_failed_probe_names_its_step_and_output() {
        let error = SandboxError::Unavailable {
            exit_code: Some(1),
            output: b"bwrap: Creating new namespace failed: Resource temporarily unavailable\n"
                .to_vec(),
        };
        assert_eq!(
            error.to_string(),
            "sandbox_unavailable: the probe sandbox exited with status 1: bwrap: Creating new namespace failed: Resource temporarily unavailable"
        );
        let silent = SandboxError::Unavailable {
            exit_code: None,
            output: Vec::new(),
        };
        assert_eq!(
            silent.to_string(),
            "sandbox_unavailable: the probe sandbox was killed or timed out without output"
        );
        assert_eq!(
            SandboxError::ToolMissing { tool: "bwrap" }.to_string(),
            "sandbox_tool_missing: bwrap was not found on PATH"
        );
    }
}
