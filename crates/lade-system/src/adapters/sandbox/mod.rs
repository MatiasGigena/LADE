pub mod cgroup;
mod command;
mod launches;
pub mod limits;
mod run;
pub mod seccomp;
mod spec;

use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use cgroup::{CgroupError, CgroupLeaf, SandboxCgroups};
pub use command::{Gate, SandboxCommand};
pub use launches::{
    TerminalLaunch, VerificationLaunch, WorkerLaunch, terminal_spec, verification_spec, worker_spec,
};
pub use limits::{
    CgroupLimits, Limits, OBJECTIVE_CGROUP_LIMITS, SANDBOX_TMP_BYTES, STORAGE_CHECK_INTERVAL,
    TERMINAL_LIMITS, VERIFICATION_OUTPUT_LIMIT, VERIFICATION_TIMEOUT, WORKER_LIMITS,
    WRITABLE_STORAGE_BYTES,
};
pub use run::{RunOutcome, run_bounded, run_bounded_with};
pub use spec::SandboxSpec;

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
    /// Finds `bwrap` and `prlimit` on `PATH` (or in `/usr/bin` and `/bin`).
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::ToolMissing`] naming the first tool that is not installed.
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

/// Runs `true` in a worker sandbox to check that sandboxes work on this machine.
///
/// # Errors
///
/// Returns [`SandboxError::ToolMissing`] when `true` is not installed, [`SandboxError::Io`] when
/// the probe cannot run, or [`SandboxError::Unavailable`] with its output when it fails or times
/// out.
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
