//! The bubblewrap sandbox a command runs in: its binds, environment, limits and the gate
//! that holds the program until the sandbox is placed.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{Limits, SANDBOX_TMP_BYTES, SandboxCommand, SandboxError, SandboxTools, seccomp};

pub(super) const SYSTEM_DIRS: &[&str] = &["/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32"];
pub(super) const SYSTEM_FILES: &[&str] = &[
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
pub(super) const SANDBOX_PATH: &str = "/usr/bin:/bin";

pub(super) const ENV_PROGRAMS: &[&str] = &["/usr/bin/env", "/bin/env"];
pub(super) const SHELL_PROGRAMS: &[&str] = &["/usr/bin/bash", "/bin/bash"];
pub(super) const RM_PROGRAMS: &[&str] = &["/usr/bin/rm", "/bin/rm"];
pub(super) const GATE_FILE: &str = "/tmp/.lade-gate";
pub(super) const GATE_SCRIPT: &str = r#"IFS= read -r gate < /tmp/.lade-gate; "$2" -f /tmp/.lade-gate; [ "$gate" = go ] || exit 125; ulimit -S -H -u "$1" || exit 125; shift 2; for fd in /proc/$$/fd/*; do fd=${fd##*/}; [ "$fd" -gt 2 ] 2>/dev/null && eval "exec $fd>&-"; done; exec "$@""#;

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
    /// A sandbox that runs `argv` in `chdir` under `limits`, with nothing bound or set yet.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::InvalidSpec`] when the program or `chdir` is not an absolute path, or
    /// [`SandboxError::ToolMissing`] when `env`, `bash` or `rm` is not installed.
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

    /// Makes `path` visible read-only inside the sandbox.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::InvalidSpec`] when `path` is relative or does not exist.
    pub fn ro_bind(&mut self, path: &Path) -> Result<&mut Self, SandboxError> {
        self.ro_binds.push(checked_bind(path)?);
        Ok(self)
    }

    /// Makes `path` visible and writable inside the sandbox.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::InvalidSpec`] when `path` is relative or does not exist.
    pub fn rw_bind(&mut self, path: &Path) -> Result<&mut Self, SandboxError> {
        self.rw_binds.push(checked_bind(path)?);
        Ok(self)
    }

    /// Sets an environment variable that is passed on the `bwrap` command line.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::InvalidSpec`] when `name` is not a valid variable name or is already
    /// set.
    pub fn env(&mut self, name: &str, value: impl AsRef<OsStr>) -> Result<&mut Self, SandboxError> {
        self.check_env_name(name)?;
        self.env.push((name.into(), value.as_ref().to_os_string()));
        Ok(self)
    }

    /// Sets an environment variable through the inherited environment instead of the command
    /// line, so it never shows in a process listing.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::InvalidSpec`] when `name` is not a valid variable name or is already
    /// set.
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

    /// Tags the sandbox with an environment marker that the startup sweep finds it by; the
    /// marker is unset inside the sandbox.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::InvalidSpec`] when `marker` does not start with `LADE_`, is longer than
    /// 64 bytes, or holds anything but ASCII letters, digits and `_`.
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

    /// The `prlimit` + `bwrap` command for this sandbox, with the seccomp filter and the gate
    /// pipe ready to be inherited.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::InvalidSpec`] when no seccomp filter exists for this architecture, or
    /// [`SandboxError::Io`] when a pipe cannot be created.
    pub fn command(&self, tools: &SandboxTools) -> Result<SandboxCommand, SandboxError> {
        let program = seccomp::program().ok_or(SandboxError::InvalidSpec { what: "seccomp" })?;
        let io = |e: io::Error| SandboxError::Io { kind: e.kind() };
        let (filter_reader, mut filter_writer) = io::pipe().map_err(io)?;
        filter_writer.write_all(&program).map_err(io)?;
        drop(filter_writer);
        let (gate_reader, gate) = io::pipe().map_err(io)?;
        let args = self.bwrap_args(filter_reader.as_raw_fd(), gate_reader.as_raw_fd());
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
            inherit: vec![filter_reader, gate_reader],
            gate: Some(gate),
        })
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
