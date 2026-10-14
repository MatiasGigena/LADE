pub mod credentials;
pub mod gateway;
pub mod launcher;
pub mod providers;
pub mod secret_service;
pub mod sweep;
pub mod terminal;

use std::path::{Path, PathBuf};
use std::sync::mpsc::SyncSender;

use lade_protocol::worker::WorkerBody;

pub use credentials::CredentialSource;
pub use gateway::{EgressPolicy, EgressRule, Gateway, UpstreamProxy};
pub use launcher::{
    GATEWAY_SOCKET_DIR, LauncherConfig, SandboxLauncher, default_launcher, gateway_socket_path,
    socket_path_fits,
};
pub use providers::{AccessStatus, AuthKind, ProviderAccess, ProviderFacts};
pub use sweep::{SweepReport, sweep_sockets, sweep_stale};
pub use terminal::{Terminal, TerminalExit, TerminalRequest};

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
#[allow(clippy::large_enum_variant)]
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

pub trait WorkerControl: Send {
    fn send(&mut self, body: WorkerBody) -> std::io::Result<()>;
    fn terminate(&mut self);
}

pub trait WorkerLauncher: Send + Sync {
    fn launch(
        &self,
        request: LaunchRequest,
        events: SyncSender<SupervisorEvent>,
    ) -> Result<Box<dyn WorkerControl>, LaunchError>;

    fn preflight(&self, _provider: &str) -> Result<(), LaunchError> {
        Ok(())
    }

    fn catalog(&self, _scratch: &Path, _models: Option<&Path>) -> Result<Vec<u8>, LaunchError> {
        Err(LaunchError::SandboxUnavailable(
            "the model catalogue runs only in the worker sandbox".into(),
        ))
    }

    fn provider_access(&self, facts: &ProviderFacts) -> ProviderAccess {
        providers::classify(
            facts,
            &gateway::default_policies(),
            &CredentialSource::empty(),
            &[],
        )
    }
}

impl LaunchError {
    pub fn detail(&self) -> String {
        match self {
            Self::SandboxUnavailable(why) => format!("worker sandbox unavailable: {why}"),
            Self::ProviderUnavailable(why) => why.clone(),
            Self::Io(e) => format!("worker launch failed: {}", e.kind()),
        }
    }
}
