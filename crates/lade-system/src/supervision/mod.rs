//! Worker supervision contract shared by the service (WP-F1) and the launcher (WP-F2).
//!
//! The service owns product authority; the supervisor owns the sandboxed worker process, its
//! JSONL pipes and its lifetime. Every event is tagged with the immutable launch binding so the
//! service can reject stale generations.

pub mod credentials;
pub mod gateway;
pub mod launcher;
pub mod sweep;

use std::path::PathBuf;
use std::sync::mpsc::SyncSender;

use lade_protocol::worker::WorkerBody;

pub use credentials::CredentialSource;
pub use gateway::{EgressPolicy, EgressRule, Gateway, UpstreamProxy};
pub use launcher::{
    GATEWAY_SOCKET_DIR, LauncherConfig, SandboxLauncher, default_launcher, gateway_socket_path,
    socket_path_fits,
};
pub use sweep::{SweepReport, sweep_sockets, sweep_stale};

/// Immutable server-side binding for one launched worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchBinding {
    pub objective_id: String,
    pub execution_generation: u64,
}

/// Everything needed to start one sandboxed worker for an Objective.
#[derive(Clone, Debug)]
pub struct LaunchRequest {
    pub binding: LaunchBinding,
    pub worktree: PathBuf,
    /// Main repository Git common directory (bound read-only).
    pub repo_common_dir: PathBuf,
    /// Objective session directory (read-write, worker HOME and pi agent dir).
    pub session_dir: PathBuf,
    /// Provider id of the selected model; selects credentials and egress policy.
    pub provider: String,
    /// The `worker.start` body sent once the process is running.
    pub start: WorkerBody,
}

/// Something observed about a launched worker.
#[derive(Debug)]
pub enum WorkerEvent {
    /// A validated frame from the worker.
    Body(WorkerBody),
    /// The worker violated framing/schema; the link was closed and the process terminated.
    ProtocolViolation(&'static str),
    /// The process (and its sandbox) ended.
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
    /// Sandbox prerequisites missing: execution is blocked, never run unsandboxed.
    SandboxUnavailable(String),
    /// No credentials or egress policy for the provider.
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

/// Control of one running worker. Dropping it must terminate the worker and its descendants.
pub trait WorkerControl: Send {
    /// Write one service-to-worker body; an error means the link is gone (delivery uncertain).
    fn send(&mut self, body: WorkerBody) -> std::io::Result<()>;
    /// Terminate the sandbox (graceful `worker.shutdown` first if possible, then kill).
    fn terminate(&mut self);
}

/// Launches sandboxed workers. Events are delivered on the bounded `events` channel.
pub trait WorkerLauncher: Send + Sync {
    fn launch(
        &self,
        request: LaunchRequest,
        events: SyncSender<SupervisorEvent>,
    ) -> Result<Box<dyn WorkerControl>, LaunchError>;

    /// Checks, without launching, whether a worker for `provider` could start now (sandbox,
    /// egress policy, credentials), so a request can be refused synchronously. Errors name
    /// missing credential variables, never values. `launch` re-checks everything.
    fn preflight(&self, _provider: &str) -> Result<(), LaunchError> {
        Ok(())
    }
}

impl LaunchError {
    /// Human-readable explanation without secrets (variable names only).
    pub fn detail(&self) -> String {
        match self {
            Self::SandboxUnavailable(why) => format!("worker sandbox unavailable: {why}"),
            Self::ProviderUnavailable(why) => why.clone(),
            Self::Io(e) => format!("worker launch failed: {}", e.kind()),
        }
    }
}
