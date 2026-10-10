//! The single hardened Git invocation path.
//!
//! Every Git process LADE starts is built here. The environment is cleared (no inherited `GIT_*`,
//! no user/system configuration, no credential or editor helpers) and a fixed set of overrides is
//! injected through `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n`. Those carry
//! command-line precedence, so they win over the untrusted repository configuration, they accept
//! arbitrary bytes in subsection names, and Git propagates them to any Git child process.
//!
//! Children receive only stdin/stdout/stderr: the standard library opens every descriptor with
//! `O_CLOEXEC`, so nothing else the service holds is inherited.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::thread::{self, JoinHandle};

use super::GitError;

/// Maximum stderr bytes retained for diagnostics.
pub const STDERR_LIMIT: usize = 2048;
const DEFAULT_STDOUT_LIMIT: usize = 64 << 20;
const GIT_PROGRAM: &str = "git";
const FALLBACK_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// LADE's fixed identity for every commit it creates.
pub const LADE_IDENTITY_NAME: &str = "LADE";
/// LADE's fixed identity e-mail for every commit it creates.
pub const LADE_IDENTITY_EMAIL: &str = "lade@localhost";

/// Overrides applied to every invocation. Repository configuration, hooks, filters, helpers and
/// external programs are untrusted and must never run with the service's authority.
const HARDENED_CONFIG: &[(&str, &str)] = &[
    ("core.hooksPath", "/dev/null"),
    ("core.fsmonitor", "false"),
    ("core.untrackedCache", "false"),
    ("core.pager", "cat"),
    ("core.editor", "false"),
    ("core.askPass", ""),
    ("core.sshCommand", "false"),
    ("core.attributesFile", "/dev/null"),
    ("core.alternateRefsCommand", ""),
    ("protocol.allow", "never"),
    ("credential.helper", ""),
    ("diff.external", ""),
    ("diff.suppressBlankEmpty", "false"),
    ("commit.gpgSign", "false"),
    ("tag.gpgSign", "false"),
    ("gpg.program", "false"),
    ("log.showSignature", "false"),
    ("gc.auto", "0"),
    ("maintenance.auto", "false"),
    ("submodule.recurse", "false"),
    ("merge.renormalize", "false"),
    ("color.ui", "false"),
    ("uploadpack.packObjectsHook", ""),
    ("sequence.editor", "false"),
];

/// Bounded stderr retained for diagnostics; never part of an error's `Display`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Diagnostics {
    bytes: Vec<u8>,
    truncated: bool,
}

impl Diagnostics {
    /// The retained bytes (at most [`STDERR_LIMIT`]).
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Whether more stderr was produced than was retained.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    pub(crate) fn contains(&self, needle: &str) -> bool {
        self.bytes
            .windows(needle.len())
            .any(|w| w == needle.as_bytes())
    }
}

impl fmt::Debug for Diagnostics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", String::from_utf8_lossy(&self.bytes))?;
        if self.truncated {
            f.write_str("…")?;
        }
        Ok(())
    }
}

/// Completed process output.
pub(crate) struct GitOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Diagnostics,
}

/// Builder for one hardened Git invocation.
pub(crate) struct GitCommand {
    op: &'static str,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    config: Vec<(OsString, OsString)>,
    git_dir: Option<PathBuf>,
    work_tree: Option<PathBuf>,
    cwd: Option<PathBuf>,
    read_only: bool,
    stdin: Option<Vec<u8>>,
    stdout_limit: usize,
}

impl GitCommand {
    /// `op` is a short, static label used in errors (never user content).
    pub(crate) fn new(op: &'static str) -> Self {
        Self {
            op,
            args: Vec::new(),
            env: Vec::new(),
            config: Vec::new(),
            git_dir: None,
            work_tree: None,
            cwd: None,
            read_only: false,
            stdin: None,
            stdout_limit: DEFAULT_STDOUT_LIMIT,
        }
    }

    /// Explicit `--git-dir`; repository discovery through a (possibly worker-written) `.git` file
    /// is never used for operations on known repositories.
    pub(crate) fn git_dir(mut self, path: &Path) -> Self {
        self.git_dir = Some(path.to_path_buf());
        self
    }

    /// Explicit `--work-tree` (overrides an untrusted `core.worktree`); also the working directory.
    pub(crate) fn work_tree(mut self, path: &Path) -> Self {
        self.work_tree = Some(path.to_path_buf());
        self
    }

    pub(crate) fn cwd(mut self, path: &Path) -> Self {
        self.cwd = Some(path.to_path_buf());
        self
    }

    pub(crate) fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    pub(crate) fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|a| a.as_ref().to_os_string()));
        self
    }

    /// An explicit `GIT_*` (or other) variable; the inherited environment is always cleared.
    pub(crate) fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.env
            .push((key.as_ref().to_os_string(), value.as_ref().to_os_string()));
        self
    }

    /// An additional command-scope configuration override.
    pub(crate) fn config(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.config
            .push((key.as_ref().to_os_string(), value.as_ref().to_os_string()));
        self
    }

    pub(crate) fn configs(mut self, pairs: &[(OsString, OsString)]) -> Self {
        self.config.extend_from_slice(pairs);
        self
    }

    /// Read-only invocation: `GIT_OPTIONAL_LOCKS=0` so no opportunistic index refresh is written.
    pub(crate) fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    pub(crate) fn stdin(mut self, bytes: Vec<u8>) -> Self {
        self.stdin = Some(bytes);
        self
    }

    pub(crate) fn stdout_limit(mut self, limit: usize) -> Self {
        self.stdout_limit = limit;
        self
    }

    fn build(&self) -> Command {
        let mut cmd = Command::new(GIT_PROGRAM);
        cmd.env_clear();
        cmd.env(
            "PATH",
            std::env::var_os("PATH").unwrap_or_else(|| FALLBACK_PATH.into()),
        );
        cmd.env("HOME", "/nonexistent");
        cmd.env("LC_ALL", "C");
        cmd.env("LANG", "C");
        cmd.env("GIT_CONFIG_NOSYSTEM", "1");
        cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
        cmd.env("GIT_ATTR_NOSYSTEM", "1");
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        cmd.env("GIT_NO_REPLACE_OBJECTS", "1");
        cmd.env("GIT_LITERAL_PATHSPECS", "1");
        cmd.env("GIT_PROTOCOL_FROM_USER", "0");
        cmd.env("GIT_NO_LAZY_FETCH", "1");
        cmd.env("GIT_PAGER", "cat");
        cmd.env("GIT_EDITOR", "false");
        cmd.env("GIT_ADVICE", "0");
        if self.read_only {
            cmd.env("GIT_OPTIONAL_LOCKS", "0");
        }
        let total = HARDENED_CONFIG.len() + self.config.len();
        cmd.env("GIT_CONFIG_COUNT", total.to_string());
        let pairs = HARDENED_CONFIG
            .iter()
            .map(|(k, v)| (OsStr::new(k), OsStr::new(v)))
            .chain(
                self.config
                    .iter()
                    .map(|(k, v)| (k.as_os_str(), v.as_os_str())),
            );
        for (i, (key, value)) in pairs.enumerate() {
            cmd.env(format!("GIT_CONFIG_KEY_{i}"), key);
            cmd.env(format!("GIT_CONFIG_VALUE_{i}"), value);
        }
        for (key, value) in &self.env {
            cmd.env(key, value);
        }
        cmd.arg("--no-pager");
        if let Some(dir) = &self.git_dir {
            cmd.arg("--git-dir").arg(dir);
        }
        if let Some(dir) = &self.work_tree {
            cmd.arg("--work-tree").arg(dir);
        }
        cmd.args(&self.args);
        let cwd = self
            .cwd
            .as_deref()
            .or(self.work_tree.as_deref())
            .or(self.git_dir.as_deref())
            .unwrap_or(Path::new("/"));
        cmd.current_dir(cwd);
        cmd
    }

    fn spawn(self) -> Result<Running, GitError> {
        let op = self.op;
        let mut cmd = self.build();
        cmd.stdin(if self.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| GitError::Spawn { op, kind: e.kind() })?;
        let stdin_writer = match (self.stdin, child.stdin.take()) {
            (Some(bytes), Some(mut pipe)) => Some(thread::spawn(move || {
                // A child that exits early closes the pipe; its exit status reports the failure.
                let _ = pipe.write_all(&bytes);
            })),
            _ => None,
        };
        let stderr = child.stderr.take().map(|mut pipe| {
            thread::spawn(move || {
                let mut kept = Vec::new();
                let mut truncated = false;
                let mut buf = [0u8; 4096];
                loop {
                    match pipe.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let room = STDERR_LIMIT.saturating_sub(kept.len());
                            kept.extend_from_slice(&buf[..n.min(room)]);
                            truncated |= n > room;
                        }
                    }
                }
                Diagnostics {
                    bytes: kept,
                    truncated,
                }
            })
        });
        let stdout = child.stdout.take().ok_or(GitError::Io {
            op,
            kind: io::ErrorKind::BrokenPipe,
        })?;
        Ok(Running {
            op,
            child,
            stdout,
            stderr,
            stdin_writer,
            stdout_limit: self.stdout_limit,
        })
    }

    /// Run to completion, returning status and bounded output; does not interpret the status.
    pub(crate) fn output(self) -> Result<GitOutput, GitError> {
        let mut running = self.spawn()?;
        let mut stdout = Vec::new();
        let limit = running.stdout_limit;
        let read = (&mut running.stdout)
            .take(limit as u64 + 1)
            .read_to_end(&mut stdout);
        if let Err(e) = read {
            let op = running.op;
            running.abort();
            return Err(GitError::Io { op, kind: e.kind() });
        }
        if stdout.len() > limit {
            let op = running.op;
            running.abort();
            return Err(GitError::OutputLimit { op, limit });
        }
        let (status, stderr) = running.finish()?;
        Ok(GitOutput {
            status,
            stdout,
            stderr,
        })
    }

    /// Run and require exit status 0; returns stdout.
    pub(crate) fn run(self) -> Result<Vec<u8>, GitError> {
        let op = self.op;
        let out = self.output()?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(GitError::Failed {
                op,
                exit_code: out.status.code(),
                stderr: out.stderr,
            })
        }
    }

    /// Spawn with a streaming stdout for bounded readers (blob pages, grep).
    pub(crate) fn stream(self) -> Result<GitStream, GitError> {
        Ok(GitStream {
            running: self.spawn()?,
        })
    }
}

struct Running {
    op: &'static str,
    child: Child,
    stdout: ChildStdout,
    stderr: Option<JoinHandle<Diagnostics>>,
    stdin_writer: Option<JoinHandle<()>>,
    stdout_limit: usize,
}

impl Running {
    fn finish(mut self) -> Result<(ExitStatus, Diagnostics), GitError> {
        let status = self.child.wait().map_err(|e| GitError::Io {
            op: self.op,
            kind: e.kind(),
        })?;
        Ok((status, self.join()))
    }

    fn join(&mut self) -> Diagnostics {
        if let Some(writer) = self.stdin_writer.take() {
            let _ = writer.join();
        }
        self.stderr
            .take()
            .and_then(|h| h.join().ok())
            .unwrap_or_default()
    }

    fn abort(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.join();
    }
}

/// A running Git process whose stdout is consumed incrementally.
pub(crate) struct GitStream {
    running: Running,
}

impl GitStream {
    pub(crate) fn stdout(&mut self) -> &mut ChildStdout {
        &mut self.running.stdout
    }

    /// Wait for exit after the caller consumed what it needed (draining the rest).
    pub(crate) fn finish(mut self) -> Result<(ExitStatus, Diagnostics), GitError> {
        let _ = io::copy(&mut self.running.stdout, &mut io::sink());
        self.running.finish()
    }

    /// Stop early: kill the process and reap it.
    pub(crate) fn abort(self) {
        self.running.abort();
    }

    pub(crate) fn op(&self) -> &'static str {
        self.running.op
    }
}
