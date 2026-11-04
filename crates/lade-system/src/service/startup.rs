//! Starting and stopping the service: its configuration, the directories and lock it owns,
//! and the threads it runs on.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use lade_protocol::types::{CONTROL_CAPABILITY_NAME, CONTROL_SOCKET_NAME, Id};

use crate::adapters::artifacts::ArtifactStore;
use crate::adapters::sandbox::{SandboxCgroups, WRITABLE_STORAGE_BYTES};
use crate::storage::{self, Store, StoreError};
use crate::supervision::launcher::RESERVED_GENERATION;
use crate::supervision::{
    GATEWAY_SOCKET_DIR, SupervisorEvent, WorkerLauncher, gateway_socket_path, socket_path_fits,
};
use crate::support::fs::ensure_private_dir;
use crate::transport::{self, Listener, TransportError};

use super::{Msg, Paths, ResourceLimits, Service, TICK, jobs};

/// How long shutdown waits for client links to flush their last frames.
const LINK_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug)]
pub struct ServiceConfig {
    pub state_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub limits: ResourceLimits,
    pub storage_limit_bytes: u64,
    pub cgroups: Option<Arc<SandboxCgroups>>,
    pub test_fixture_provider: bool,
}

impl ServiceConfig {
    #[must_use]
    pub fn from_environment(state: Option<PathBuf>, runtime: Option<PathBuf>) -> Option<Self> {
        let env_dir = |name: &str| {
            std::env::var_os(name)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
        };
        let state_dir = state.or_else(|| {
            env_dir("XDG_STATE_HOME")
                .or_else(|| env_dir("HOME").map(|h| h.join(".local/state")))
                .map(|d| d.join("lade"))
        })?;
        let runtime_dir = runtime
            .or_else(|| env_dir("XDG_RUNTIME_DIR").map(|d| d.join("lade")))
            .unwrap_or_else(|| state_dir.join("run"));
        Some(Self {
            state_dir,
            runtime_dir,
            limits: ResourceLimits::default(),
            storage_limit_bytes: WRITABLE_STORAGE_BYTES,
            cgroups: None,
            test_fixture_provider: false,
        })
    }
}

#[derive(Debug)]
pub enum StartError {
    InvalidPath(&'static str),
    SocketPathTooLong(PathBuf),
    StateDir(String),
    PrivateDir(PathBuf, io::Error),
    AlreadyRunning,
    Store(StoreError),
    Transport(TransportError),
    Recovery(String),
    Thread(&'static str, io::Error),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPath(what) => write!(f, "{what} must be an absolute path"),
            Self::SocketPathTooLong(path) => write!(
                f,
                "socket path {} is longer than the {} bytes a Unix socket address holds; \
                 choose a shorter --runtime-dir (for example under $XDG_RUNTIME_DIR)",
                path.display(),
                crate::supervision::launcher::MAX_SOCKET_PATH
            ),
            Self::StateDir(e) => write!(f, "state directory: {e}"),
            Self::PrivateDir(path, e) => write!(f, "{}: {e}", path.display()),
            Self::AlreadyRunning => f.write_str("another service owns this state directory"),
            Self::Store(e) => write!(f, "{e}"),
            Self::Transport(e) => write!(f, "{e}"),
            Self::Recovery(e) => write!(f, "restart recovery failed: {e}"),
            Self::Thread(name, e) => write!(f, "could not start the {name} thread: {e}"),
        }
    }
}

impl std::error::Error for StartError {}

impl From<rusqlite::Error> for StartError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(error.into())
    }
}

pub struct RunningService {
    queue: SyncSender<Msg>,
    thread: Option<JoinHandle<()>>,
    listener: Option<Listener>,
    /// Dropping this sender wakes and stops the ticker thread.
    ticker_stop: Option<mpsc::Sender<()>>,
    ticker: Option<JoinHandle<()>>,
    socket: PathBuf,
    capability: PathBuf,
    epoch: Id,
    _lock: File,
}

impl RunningService {
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    #[must_use]
    pub fn capability_path(&self) -> &Path {
        &self.capability
    }

    #[must_use]
    pub fn service_epoch(&self) -> &str {
        self.epoch.as_str()
    }

    pub fn wait(mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.release();
    }

    fn release(&mut self) {
        if let Some(mut listener) = self.listener.take() {
            listener.stop();
            listener.drain(LINK_DRAIN_TIMEOUT);
        }
        self.ticker_stop = None;
        if let Some(ticker) = self.ticker.take() {
            let _ = ticker.join();
        }
        let _ = fs::remove_file(&self.socket);
        let _ = fs::remove_file(&self.capability);
    }

    pub fn shutdown(mut self) {
        if let Some(listener) = self.listener.as_mut() {
            listener.stop();
        }
        let _ = self.queue.send(Msg::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.release();
    }
}

fn check_socket_paths(runtime: &Path) -> Result<(), StartError> {
    let widest = gateway_socket_path(
        &runtime.join(GATEWAY_SOCKET_DIR),
        "ffffffff-ffff-4fff-bfff-ffffffffffff",
        RESERVED_GENERATION,
    );
    for path in [runtime.join(CONTROL_SOCKET_NAME), widest] {
        if !socket_path_fits(&path) {
            return Err(StartError::SocketPathTooLong(path));
        }
    }
    Ok(())
}

fn private_dir(path: &Path) -> Result<(), StartError> {
    ensure_private_dir(path).map_err(|e| StartError::PrivateDir(path.to_path_buf(), e))
}

/// Validates the configured directories, creates the private ones the service owns and
/// returns their canonical layout.
fn service_paths(config: &ServiceConfig) -> Result<Paths, StartError> {
    if !config.state_dir.is_absolute() {
        return Err(StartError::InvalidPath("state directory"));
    }
    if !config.runtime_dir.is_absolute() {
        return Err(StartError::InvalidPath("runtime directory"));
    }
    check_socket_paths(&config.runtime_dir)?;
    private_dir(&config.runtime_dir)?;
    private_dir(&config.state_dir)?;
    let state = config
        .state_dir
        .canonicalize()
        .map_err(|e| StartError::StateDir(e.kind().to_string()))?;
    let paths = Paths {
        worktrees: state.join("worktrees"),
        sessions: state.join("sessions"),
        verify: state.join("verify"),
        tmp: state.join("tmp"),
        run: state.join("run"),
        sockets: config.runtime_dir.join(GATEWAY_SOCKET_DIR),
        state,
    };
    for dir in [
        &paths.worktrees,
        &paths.sessions,
        &paths.verify,
        &paths.tmp,
        &paths.run,
    ] {
        private_dir(dir)?;
    }
    Ok(paths)
}

/// Starts the service: takes the state lock, loads and recovers durable state, then binds the
/// control socket and starts the service, supervisor-forwarding and ticker threads.
///
/// # Errors
///
/// Returns a [`StartError`] when a directory is relative, unusable or too deep for socket
/// paths, another service holds the state lock or answers on the socket, the store or artifact
/// store cannot be opened, restart recovery fails, or a thread cannot be spawned.
pub fn start(
    config: &ServiceConfig,
    launcher: Arc<dyn WorkerLauncher>,
) -> Result<RunningService, StartError> {
    let paths = service_paths(config)?;
    let lock = storage::lock_state(&paths.state).map_err(|e| match e.kind() {
        io::ErrorKind::WouldBlock => StartError::AlreadyRunning,
        kind => StartError::StateDir(format!("lock: {kind}")),
    })?;
    transport::ensure_not_running(&config.runtime_dir).map_err(StartError::Transport)?;
    let store = Store::open(&paths.state.join("lade.db")).map_err(StartError::Store)?;
    let artifacts = ArtifactStore::open(&paths.state.join("artifacts"), &paths.tmp)
        .map_err(|e| StartError::StateDir(format!("artifacts: {}", e.code())))?;
    let (queue_tx, queue_rx) = mpsc::sync_channel::<Msg>(8192);
    let (supervisor_tx, supervisor_rx) = mpsc::sync_channel::<SupervisorEvent>(1024);
    let mut service = Service::new(
        config,
        store,
        paths,
        Arc::new(artifacts),
        launcher,
        supervisor_tx,
        queue_tx.clone(),
    );
    service.pools = jobs::Pools::start().map_err(|e| StartError::Thread("job pool", e))?;
    service.load()?;
    service.load_resources()?;
    service.load_agent_sessions()?;
    service.recover()?;
    service.check_free_space();
    eprintln!("laded: limits {}", config.limits);
    let epoch = service.epoch.clone();

    let capability =
        transport::write_capability(&config.runtime_dir).map_err(StartError::Transport)?;
    let listener = transport::bind_socket(&config.runtime_dir).map_err(StartError::Transport)?;
    let socket = config.runtime_dir.join(CONTROL_SOCKET_NAME);
    let listener = transport::serve(listener, socket.clone(), capability, queue_tx.clone())
        .map_err(|e| StartError::Thread("accept", e))?;

    let forward = queue_tx.clone();
    thread::Builder::new()
        .name("laded-supervisor".into())
        .spawn(move || {
            for event in supervisor_rx {
                if forward.send(Msg::Supervisor(event)).is_err() {
                    break;
                }
            }
        })
        .map_err(|e| StartError::Thread("supervisor", e))?;
    let (ticker_stop, ticker) = start_ticker(queue_tx.clone())?;
    let thread = thread::Builder::new()
        .name("laded-service".into())
        .spawn(move || service.run(&queue_rx))
        .map_err(|e| StartError::Thread("service", e))?;
    Ok(RunningService {
        queue: queue_tx,
        thread: Some(thread),
        listener: Some(listener),
        ticker_stop: Some(ticker_stop),
        ticker: Some(ticker),
        socket,
        capability: config.runtime_dir.join(CONTROL_CAPABILITY_NAME),
        epoch,
        _lock: lock,
    })
}

/// Starts the thread that queues a [`Msg::Tick`] every [`TICK`] until the returned sender is
/// dropped, which wakes it at once.
fn start_ticker(queue: SyncSender<Msg>) -> Result<(mpsc::Sender<()>, JoinHandle<()>), StartError> {
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let ticker = thread::Builder::new()
        .name("laded-tick".into())
        .spawn(move || {
            while let Err(RecvTimeoutError::Timeout) = stop_rx.recv_timeout(TICK) {
                let _ = queue.try_send(Msg::Tick);
            }
        })
        .map_err(|e| StartError::Thread("ticker", e))?;
    Ok((stop_tx, ticker))
}
