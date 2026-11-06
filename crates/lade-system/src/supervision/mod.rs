mod child;
pub mod credentials;
pub mod gateway;
mod input;
pub mod launcher;
pub mod provider_access;
pub mod secret_service;
pub mod sweep;
pub mod terminal;

use std::path::{Path, PathBuf};
use std::sync::mpsc::SyncSender;

use lade_protocol::worker::WorkerBody;

use crate::adapters::sandbox::SandboxError;

pub use credentials::CredentialSource;
pub use gateway::{EgressPolicy, EgressRule, Gateway, UpstreamProxy};
pub use launcher::{
    GATEWAY_SOCKET_DIR, LauncherConfig, SandboxLauncher, default_launcher, gateway_socket_path,
    socket_path_fits,
};
pub use provider_access::{AccessStatus, AuthKind, ProviderAccess, ProviderFacts};
pub use sweep::{SweepReport, sweep_sockets, sweep_stale};
pub use terminal::{Terminal, TerminalExit, TerminalRequest};

/// Whether `id` is safe in a file or socket name: 1 to 64 ASCII letters, digits or dashes.
pub(crate) fn safe_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchBinding {
    pub objective_id: String,
    pub execution_generation: u64,
}

#[derive(Clone, Debug)]
pub struct LaunchRequest {
    pub binding: LaunchBinding,
    pub worktree: PathBuf,
    pub repo_common_dir: PathBuf,
    pub session_dir: PathBuf,
    pub provider: String,
    pub start: WorkerBody,
}

#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "nearly every event is a Body; boxing it would allocate per worker message to \
              shrink the rare violation and exit events"
)]
pub enum WorkerEvent {
    Body(WorkerBody),
    ProtocolViolation(&'static str),
    Exited {
        code: Option<i32>,
        signal: Option<i32>,
    },
}

#[derive(Debug)]
pub struct SupervisorEvent {
    pub binding: LaunchBinding,
    pub event: WorkerEvent,
}

#[derive(Debug)]
pub enum LaunchError {
    SandboxUnavailable(String),
    ProviderUnavailable(String),
    Io(std::io::Error),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SandboxUnavailable(_) => f.write_str("sandbox_unavailable"),
            Self::ProviderUnavailable(_) => f.write_str("provider_unavailable"),
            Self::Io(_) => f.write_str("launch_io"),
        }
    }
}

impl std::error::Error for LaunchError {}

impl From<SandboxError> for LaunchError {
    fn from(error: SandboxError) -> Self {
        Self::SandboxUnavailable(error.to_string())
    }
}

pub trait WorkerControl: Send {
    /// Sends one message to the worker.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the worker's input is closed or the message cannot be
    /// written.
    fn send(&mut self, body: WorkerBody) -> std::io::Result<()>;
    fn terminate(&mut self);
}

pub trait WorkerLauncher: Send + Sync {
    /// Starts the worker `request` describes; its events arrive on `events`.
    ///
    /// # Errors
    ///
    /// Returns a [`LaunchError`] when the sandbox, the provider or the worker process cannot be set up.
    fn launch(
        &self,
        request: LaunchRequest,
        events: SyncSender<SupervisorEvent>,
    ) -> Result<Box<dyn WorkerControl>, LaunchError>;

    /// Checks, before a launch, that `provider` can run; the default accepts every provider.
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError::ProviderUnavailable`] when the provider has no credentials or policy.
    fn preflight(&self, _provider: &str) -> Result<(), LaunchError> {
        Ok(())
    }

    /// Runs the model catalogue in a sandbox and returns its JSON output.
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError::SandboxUnavailable`] when the launcher has no sandbox (the default), or
    /// the launcher's error when the catalogue run fails.
    fn catalog(&self, _scratch: &Path, _models: Option<&Path>) -> Result<Vec<u8>, LaunchError> {
        Err(LaunchError::SandboxUnavailable(
            "the model catalogue runs only in the worker sandbox".into(),
        ))
    }

    fn provider_access(&self, facts: &ProviderFacts) -> ProviderAccess {
        provider_access::classify(
            facts,
            &gateway::default_policies(),
            &CredentialSource::empty(),
            &[],
        )
    }
}

impl LaunchError {
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::SandboxUnavailable(why) => format!("worker sandbox unavailable: {why}"),
            Self::ProviderUnavailable(why) => why.clone(),
            Self::Io(e) => format!("worker launch failed: {}", e.kind()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_file_safe() {
        assert!(safe_id("0b8a4c1e-7a1f-4f7e-9d7a-0d6c8f2a9b11"));
        assert!(safe_id(&"A".repeat(64)));
        assert!(!safe_id(&"a".repeat(65)));
        for bad in ["", "../x", "a/b", "a b", "a_b", "é"] {
            assert!(!safe_id(bad), "{bad:?}");
        }
    }
}
