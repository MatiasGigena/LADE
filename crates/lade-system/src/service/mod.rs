mod agent_session;
mod capture;
pub mod convert;
mod detail_budget;
mod execution;
mod feedback;
mod handlers;
mod inspect;
mod jobs;
mod limits;
mod models;
mod objectives;
mod project;
mod recovery;
mod resources;
mod review;
mod state;
mod terminal;
#[cfg(test)]
mod test_harness;

/// The repository fixtures shared with the adapter integration tests, for unit tests here.
#[cfg(test)]
#[path = "../../tests/adapters_support/mod.rs"]
mod adapters_support;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use lade_core::domain::ids::{CandidateId, ObjectiveId, Uuid, VerificationId, WorkspaceId};
use lade_core::domain::review::IntegrationPlan;
use lade_protocol::client::{
    ClientBody, ClientEnvelope, ErrorCode, Event, EventFrame, NoticeCode, NoticeLevel, Outcome,
    Rejection, Reply, ReplyData, ReplyResult, RequestFrame, RequestName, SafeAction, ServiceNotice,
    Settings,
};
use lade_protocol::types::{CONTROL_CAPABILITY_NAME, CONTROL_SOCKET_NAME, Counter, Id};
use lade_protocol::worker::WorkerResources;
use lade_protocol::{Framing, encode};
use rusqlite::Transaction;

use crate::adapters::artifacts::ArtifactStore;
use crate::adapters::git::{GitError, SquashOutcome};
use crate::adapters::profile::ImportReport;
use crate::adapters::sandbox::{
    STORAGE_CHECK_INTERVAL, SandboxCgroups, SandboxTools, WRITABLE_STORAGE_BYTES,
};
use crate::storage::{self, OperationKey, OperationState, Store, StoreError};
use crate::supervision::launcher::RESERVED_GENERATION;
use crate::supervision::{
    GATEWAY_SOCKET_DIR, SupervisorEvent, Terminal, WorkerLauncher, gateway_socket_path,
    socket_path_fits,
};
use crate::support::fs::ensure_private_dir;
use crate::transport::{self, Inbound, Listener, Outbound, TransportError};

pub use limits::ResourceLimits;

use crate::support::{hex, new_uuid, now_ms};
use convert::{durability, wire_id};
use state::{ObjState, Runtime, WsState};

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

#[derive(Clone, Debug)]
pub(crate) struct Paths {
    pub state: PathBuf,
    pub worktrees: PathBuf,
    pub sessions: PathBuf,
    pub verify: PathBuf,
    pub tmp: PathBuf,
    pub run: PathBuf,
    pub sockets: PathBuf,
}

pub(crate) enum Msg {
    Link(Inbound),
    Supervisor(SupervisorEvent),
    Job(Completed),
    Tick,
    Shutdown,
}

impl From<Inbound> for Msg {
    fn from(inbound: Inbound) -> Self {
        Self::Link(inbound)
    }
}

/// A finished background job together with everything the service needs to continue the
/// request or event that started it. The job closure captures that context and returns it, so
/// each continuation is paired with its result by construction.
pub(crate) enum Completed {
    Launch {
        oid: ObjectiveId,
        generation: u64,
        out: jobs::LaunchOut,
    },
    Capture {
        oid: ObjectiveId,
        purpose: capture::CapturePurpose,
        result: Result<jobs::CaptureOut, Rejection>,
    },
    Freeze {
        oid: ObjectiveId,
        ctx: ReqCtx,
        candidate_id: CandidateId,
        result: Result<jobs::FreezeOut, Rejection>,
    },
    Refresh {
        oid: ObjectiveId,
        ctx: ReqCtx,
        candidate_id: CandidateId,
        new_id: CandidateId,
        result: Result<jobs::RefreshOut, Rejection>,
    },
    Verify {
        oid: ObjectiveId,
        verification_id: VerificationId,
        out: jobs::VerifyOut,
    },
    Integrate {
        oid: ObjectiveId,
        ctx: ReqCtx,
        plan: IntegrationPlan,
        journal_id: Uuid,
        result: Result<SquashOutcome, GitError>,
    },
    Cleanup {
        oid: ObjectiveId,
        ctx: ReqCtx,
        confirm: bool,
        result: Result<(), Rejection>,
    },
    DeleteHistory {
        oid: ObjectiveId,
        ctx: ReqCtx,
        result: Result<(), Rejection>,
    },
    Complete {
        oid: ObjectiveId,
        ctx: ReqCtx,
        result: Result<bool, Rejection>,
    },
    Query {
        ctx: ReqCtx,
        result: Result<ReplyData, Rejection>,
    },
    Catalog {
        result: Result<models::CatalogData, String>,
    },
    ProfileImport {
        ctx: ReqCtx,
        result: Result<ImportReport, String>,
    },
    Reload {
        oid: ObjectiveId,
        ctx: ReqCtx,
        result: Result<(WorkerResources, PathBuf), String>,
    },
    TerminalOpen {
        oid: ObjectiveId,
        ctx: ReqCtx,
        terminal_id: Id,
        writable: bool,
        size: (u16, u16),
        result: Result<Terminal, Rejection>,
    },
    Storage {
        usage: Vec<execution::StorageUsage>,
    },
    Done,
}

pub(crate) struct ReqCtx {
    pub conn: u64,
    pub request_id: Id,
    pub name: RequestName,
    pub op: Option<OpInfo>,
    pub recorded: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct OpInfo {
    pub id: Id,
    pub digest: String,
}

impl OpInfo {
    pub(crate) fn key(&self, name: RequestName) -> OperationKey<'_> {
        OperationKey {
            id: &self.id,
            digest: &self.digest,
            name,
        }
    }
}

pub(crate) enum Flow {
    Done,
    Deferred,
}

struct Controller {
    conn: u64,
    out: Outbound,
    request_ids: HashMap<String, [u8; 32]>,
    request_order: VecDeque<String>,
}

const REQUEST_ID_MEMORY: usize = 4096;
/// How often the ticker wakes the service thread to flush deltas and check timeouts.
const TICK: Duration = Duration::from_millis(100);
/// Ticks between storage-usage checks.
const STORAGE_CHECK_TICKS: u128 = STORAGE_CHECK_INTERVAL.as_millis() / TICK.as_millis();
/// How long shutdown waits for client links to flush their last frames.
const LINK_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// The service's single-threaded state. Each capability keeps its state in one field owned by
/// its module; the comments name the module that changes each group.
pub(crate) struct Service {
    // Infrastructure (mod.rs).
    store: Store,
    paths: Paths,
    artifacts: Arc<ArtifactStore>,
    large_blobs: Arc<jobs::LargeBlobs>,
    launcher: Arc<dyn WorkerLauncher>,
    supervisor_tx: SyncSender<SupervisorEvent>,
    queue_tx: SyncSender<Msg>,
    pools: jobs::Pools,
    held: Option<Vec<(bool, jobs::Task)>>,
    sandbox: OnceLock<Option<SandboxTools>>,
    cgroups: Option<Arc<SandboxCgroups>>,
    limits: ResourceLimits,
    test_fixture_provider: bool,
    ticks: u64,
    stopping: bool,
    // The client link and event stream (mod.rs).
    epoch: Id,
    sequence: u64,
    controller: Option<Controller>,
    recovery_notes: Vec<String>,
    // Workspaces, Objectives and settings (handlers.rs, review.rs, execution.rs).
    workspaces: BTreeMap<WorkspaceId, WsState>,
    objectives: BTreeMap<ObjectiveId, ObjState>,
    runtime: HashMap<ObjectiveId, Runtime>,
    settings: Settings,
    // Execution activity and accepted input (execution.rs).
    activity_seq: HashMap<ObjectiveId, u64>,
    input_seq: u64,
    live: execution::LiveActivity,
    // One field per remaining capability, owned by the module of the same name.
    catalog: models::CatalogCache,
    extension_approvals: HashMap<(String, String), bool>,
    agent_sessions: agent_session::AgentSessions,
    terminals: terminal::Terminals,
    integrations: review::Integrations,
    storage: limits::StorageGuard,
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

fn recordable(rejection: &Rejection) -> bool {
    rejection.code != ErrorCode::DurabilityFailure
}

impl Service {
    /// Builds a service with empty in-memory state and no job threads; `start` then loads the
    /// durable state and starts the pools.
    fn new(
        config: &ServiceConfig,
        store: Store,
        paths: Paths,
        artifacts: Arc<ArtifactStore>,
        launcher: Arc<dyn WorkerLauncher>,
        supervisor_tx: SyncSender<SupervisorEvent>,
        queue_tx: SyncSender<Msg>,
    ) -> Self {
        Self {
            store,
            paths,
            artifacts,
            large_blobs: Arc::default(),
            launcher,
            supervisor_tx,
            queue_tx,
            pools: jobs::Pools::default(),
            epoch: wire_id(new_uuid()),
            sequence: 0,
            controller: None,
            workspaces: BTreeMap::new(),
            objectives: BTreeMap::new(),
            runtime: HashMap::new(),
            settings: Settings {
                verification_commands: Vec::new(),
                default_model: None,
            },
            activity_seq: HashMap::new(),
            input_seq: 0,
            live: execution::LiveActivity::default(),
            ticks: 0,
            sandbox: OnceLock::new(),
            catalog: models::CatalogCache::default(),
            extension_approvals: HashMap::new(),
            agent_sessions: agent_session::AgentSessions::default(),
            terminals: terminal::Terminals::default(),
            stopping: false,
            limits: config.limits.clone(),
            recovery_notes: Vec::new(),
            integrations: review::Integrations::default(),
            held: None,
            storage: limits::StorageGuard::new(config.storage_limit_bytes),
            cgroups: config.cgroups.clone(),
            test_fixture_provider: config.test_fixture_provider,
        }
    }

    fn run(mut self, queue: &Receiver<Msg>) {
        while let Ok(message) = queue.recv() {
            match message {
                Msg::Link(inbound) => self.on_link(inbound),
                Msg::Supervisor(event) => self.on_supervisor(event),
                Msg::Job(completed) => self.on_job(completed),
                Msg::Tick => self.on_tick(),
                Msg::Shutdown => break,
            }
            if self.stopping {
                eprintln!("laded: stopping on request of the controlling client");
                break;
            }
        }
        self.flush_activity();
        self.drop_terminals(None, true);
        for runtime in self.runtime.values_mut() {
            if let Some(mut control) = runtime.control.take() {
                control.terminate();
            }
        }
        if let Some(controller) = self.controller.take() {
            controller.out.close();
        }
        self.pools.shutdown();
    }

    fn on_link(&mut self, inbound: Inbound) {
        match inbound {
            Inbound::Attach { conn, out } => {
                if self.controller.is_some() {
                    let notice = self.encode_event_frame(
                        0,
                        Event::ServiceNotice(ServiceNotice {
                            level: NoticeLevel::Error,
                            code: NoticeCode::LeaseHeld,
                            message: "another client holds the controlling lease".into(),
                        }),
                    );
                    out.close_with(notice);
                    eprintln!("laded: refused second client conn={conn}: lease_held");
                    return;
                }
                let lease_id = wire_id(new_uuid());
                let welcome = ClientEnvelope::new(ClientBody::Welcome {
                    service_epoch: self.epoch.clone(),
                    lease_id,
                });
                let pushed =
                    encode(&welcome, Framing::Client).is_ok_and(|bytes| out.push(bytes).is_ok());
                if pushed {
                    self.controller = Some(Controller {
                        conn,
                        out,
                        request_ids: HashMap::new(),
                        request_order: VecDeque::new(),
                    });
                    self.announce_dialogs();
                    self.announce_storage(false);
                    for note in std::mem::take(&mut self.recovery_notes) {
                        self.notice(NoticeLevel::Warning, NoticeCode::Recovery, &note);
                    }
                } else {
                    out.close();
                }
            }
            Inbound::Detach { conn } => {
                if self.controller.as_ref().is_some_and(|c| c.conn == conn) {
                    self.controller = None;
                    self.drop_terminals(Some(conn), false);
                }
            }
            Inbound::Invalid { conn, request_id } => {
                let reply = Reply::rejected(
                    request_id,
                    None,
                    Rejection::new(
                        ErrorCode::InvalidRequest,
                        SafeAction::None,
                        "request failed schema validation",
                    ),
                );
                self.send_reply(conn, reply);
            }
            Inbound::Request { conn, frame } => {
                if self.controller.as_ref().is_some_and(|c| c.conn == conn) {
                    self.on_request(conn, frame);
                }
            }
        }
    }

    fn encode_event_frame(&self, sequence: u64, event: Event) -> Option<Vec<u8>> {
        let frame = ClientEnvelope::new(ClientBody::Event(EventFrame {
            service_epoch: self.epoch.clone(),
            sequence: Counter::new(sequence),
            event,
        }));
        encode(&frame, Framing::Client).ok()
    }

    pub(crate) fn emit(&mut self, event: Event) {
        self.sequence += 1;
        if self.controller.is_none() {
            return;
        }
        let name = event.name();
        let Some(bytes) = self.encode_event_frame(self.sequence, event) else {
            eprintln!("laded: event too large to encode: {}", name.as_str());
            return;
        };
        let overflow = self
            .controller
            .as_ref()
            .is_some_and(|c| c.out.push(bytes).is_err());
        if overflow {
            self.drop_controller("outbound queue overflow; re-query snapshot");
        }
    }

    fn drop_controller(&mut self, why: &str) {
        let Some(controller) = self.controller.take() else {
            return;
        };
        self.drop_terminals(Some(controller.conn), false);
        let notice = self.encode_event_frame(
            self.sequence,
            Event::ServiceNotice(ServiceNotice {
                level: NoticeLevel::Warning,
                code: NoticeCode::LinkOverflow,
                message: why.into(),
            }),
        );
        eprintln!(
            "laded: dropped client link conn={}: overflow",
            controller.conn
        );
        controller.out.close_with(notice);
    }

    pub(crate) fn notice(&mut self, level: NoticeLevel, code: NoticeCode, message: &str) {
        self.emit(Event::ServiceNotice(ServiceNotice {
            level,
            code,
            message: convert::preview(message, 512),
        }));
    }

    fn send_reply(&mut self, conn: u64, reply: Reply) {
        let Some(controller) = self.controller.as_ref().filter(|c| c.conn == conn) else {
            return;
        };
        let request_id = reply.request_id.clone();
        let name = reply.result.name();
        let bytes = encode(
            &ClientEnvelope::new(ClientBody::Reply(reply)),
            Framing::Client,
        )
        .or_else(|_| {
            let fallback = Reply::rejected(
                request_id,
                name,
                Rejection::new(
                    ErrorCode::LimitReached,
                    SafeAction::None,
                    "reply exceeds the frame budget",
                ),
            );
            encode(
                &ClientEnvelope::new(ClientBody::Reply(fallback)),
                Framing::Client,
            )
        });
        let Ok(bytes) = bytes else {
            return;
        };
        if controller.out.push(bytes).is_err() {
            self.drop_controller("outbound queue overflow; re-query snapshot");
        }
    }

    fn on_request(&mut self, conn: u64, frame: RequestFrame) {
        let name = frame.request.name();
        let Ok(digest) = frame.canonical_digest() else {
            let reply = Reply::rejected(
                frame.request_id,
                Some(name),
                Rejection::new(
                    ErrorCode::InvalidRequest,
                    SafeAction::None,
                    "request payload cannot be represented as JSON",
                ),
            );
            self.send_reply(conn, reply);
            return;
        };
        let request_key = frame.request_id.as_str().to_owned();
        if let Some(controller) = self.controller.as_mut() {
            match controller.request_ids.get(&request_key) {
                Some(seen) if *seen != digest => {
                    let reply = Reply::rejected(
                        frame.request_id,
                        Some(name),
                        Rejection::new(
                            ErrorCode::DuplicateConflict,
                            SafeAction::None,
                            "request id reused with different content",
                        ),
                    );
                    self.send_reply(conn, reply);
                    return;
                }
                Some(_) => {}
                None => {
                    controller.request_ids.insert(request_key.clone(), digest);
                    controller.request_order.push_back(request_key);
                    if controller.request_order.len() > REQUEST_ID_MEMORY
                        && let Some(old) = controller.request_order.pop_front()
                    {
                        controller.request_ids.remove(&old);
                    }
                }
            }
        }
        let op = frame.operation_id.clone().map(|id| OpInfo {
            id,
            digest: hex(&digest),
        });
        if let Some(op) = &op {
            match storage::load_operation(self.store.conn(), &op.id) {
                Err(_) => {
                    let reply = Reply::rejected(
                        frame.request_id,
                        Some(name),
                        durability("operation journal unavailable"),
                    );
                    self.send_reply(conn, reply);
                    return;
                }
                Ok(Some(row)) => {
                    let reply = Self::duplicate_reply(frame.request_id, name, op, &row);
                    self.send_reply(conn, reply);
                    return;
                }
                Ok(None) => {}
            }
        }
        let ctx = ReqCtx {
            conn,
            request_id: frame.request_id,
            name,
            op: op.clone(),
            recorded: false,
        };
        self.held = Some(Vec::new());
        let flow = self.dispatch(ctx, frame.request);
        let held = self.held.take().unwrap_or_default();
        if let Flow::Deferred = flow
            && let Some(op) = op
        {
            let recorded = storage::record_operation(
                self.store.conn(),
                op.key(name),
                OperationState::Pending,
                None,
                now_ms(),
            );
            if recorded.is_err() {
                eprintln!("laded: could not journal pending operation");
            }
        }
        for (verify, task) in held {
            self.pools.submit(verify, task);
        }
    }

    fn duplicate_reply(
        request_id: Id,
        name: RequestName,
        op: &OpInfo,
        row: &storage::OperationRow,
    ) -> Reply {
        if row.digest != op.digest {
            return Reply::rejected(
                request_id,
                Some(name),
                Rejection::new(
                    ErrorCode::DuplicateConflict,
                    SafeAction::QueryOperation,
                    "operation id reused with different content",
                ),
            );
        }
        match &row.outcome {
            Some(Outcome::Ok(data)) => Reply {
                request_id,
                result: ReplyResult::Ok(data.clone()),
            },
            Some(Outcome::Rejected { name, rejection }) => {
                Reply::rejected(request_id, Some(*name), rejection.clone())
            }
            None => Reply::rejected(
                request_id,
                Some(name),
                Rejection::new(
                    ErrorCode::Conflict,
                    SafeAction::QueryOperation,
                    "operation is still in progress",
                ),
            ),
        }
    }

    pub(crate) fn finish(&mut self, ctx: ReqCtx, result: Result<ReplyData, Rejection>) {
        if let Some(op) = &ctx.op
            && !ctx.recorded
        {
            let outcome = match &result {
                Ok(data) => Some(Outcome::Ok(Box::new(data.clone()))),
                Err(rejection) if recordable(rejection) => Some(Outcome::Rejected {
                    name: ctx.name,
                    rejection: rejection.clone(),
                }),
                Err(_) => None,
            };
            let state = if outcome.is_some() {
                OperationState::Completed
            } else {
                OperationState::Uncertain
            };
            let recorded = storage::record_operation(
                self.store.conn(),
                op.key(ctx.name),
                state,
                outcome.as_ref(),
                now_ms(),
            );
            if recorded.is_err() {
                let reply = Reply::rejected(
                    ctx.request_id,
                    Some(ctx.name),
                    durability("operation outcome could not be recorded"),
                );
                self.send_reply(ctx.conn, reply);
                return;
            }
        }
        let reply = match result {
            Ok(data) => Reply::ok(ctx.request_id, data),
            Err(rejection) => Reply::rejected(ctx.request_id, Some(ctx.name), rejection),
        };
        self.send_reply(ctx.conn, reply);
    }

    pub(crate) fn commit(
        &mut self,
        ctx: Option<&mut ReqCtx>,
        outcome: Option<&ReplyData>,
        writes: impl FnOnce(&Transaction<'_>) -> rusqlite::Result<()>,
    ) -> Result<(), Rejection> {
        let now = now_ms();
        let tx = self
            .store
            .begin()
            .map_err(store_failed("begin", "store unavailable"))?;
        writes(&tx).map_err(store_failed("write", "state could not be recorded"))?;
        let mut journaled = false;
        if let (Some(ctx), Some(data)) = (ctx.as_deref(), outcome)
            && let Some(op) = &ctx.op
        {
            storage::record_operation(
                &tx,
                op.key(ctx.name),
                OperationState::Completed,
                Some(&Outcome::Ok(Box::new(data.clone()))),
                now,
            )
            .map_err(store_failed(
                "journal",
                "operation outcome could not be recorded",
            ))?;
            journaled = true;
        }
        tx.commit()
            .map_err(store_failed("commit", "state could not be recorded"))?;
        if journaled && let Some(ctx) = ctx {
            ctx.recorded = true;
        }
        Ok(())
    }

    /// Applies the policy for state changes that have no request to reject (worker events,
    /// job completions): a failed write is logged once with what was lost and its cause, and
    /// the caller sees `false` and decides whether to go on without it.
    pub(crate) fn persisted(what: &str, oid: ObjectiveId, result: Result<(), Rejection>) -> bool {
        match result {
            Ok(()) => true,
            Err(rejection) => {
                eprintln!(
                    "laded: could not record {what} objective={oid}: {}",
                    rejection.detail
                );
                false
            }
        }
    }

    pub(crate) fn spawn(&mut self, work: impl FnOnce() -> Completed + Send + 'static) {
        self.spawn_in(false, work);
    }

    pub(crate) fn spawn_verify(&mut self, work: impl FnOnce() -> Completed + Send + 'static) {
        self.spawn_in(true, work);
    }

    fn spawn_in(&mut self, verify: bool, work: impl FnOnce() -> Completed + Send + 'static) {
        let tx = self.queue_tx.clone();
        let task: jobs::Task = Box::new(move || {
            let _ = tx.send(Msg::Job(work()));
        });
        if let Some(held) = self.held.as_mut() {
            held.push((verify, task));
            return;
        }
        self.pools.submit(verify, task);
    }

    pub(crate) fn reap(&self, work: impl FnOnce() + Send + 'static) {
        if let Some(reaper) = &self.pools.reaper {
            reaper.submit(work);
        }
    }

    fn on_tick(&mut self) {
        self.ticks += 1;
        self.flush_deltas();
        self.pump_terminals();
        if self.ticks.is_multiple_of(10) {
            self.flush_activity();
            self.check_free_space();
        }
        if u128::from(self.ticks).is_multiple_of(STORAGE_CHECK_TICKS) {
            self.check_storage();
        }
        self.check_timeouts();
        self.expire_session_commands();
    }

    pub(crate) fn sandbox_tools(&self) -> Option<SandboxTools> {
        self.sandbox
            .get_or_init(|| {
                let tools = SandboxTools::locate().ok()?;
                crate::adapters::sandbox::sandbox_available(&tools).ok()?;
                Some(tools)
            })
            .clone()
    }
}

/// Logs a store failure in `Service::commit` with its stage and cause, then rejects with
/// `detail`.
fn store_failed(
    stage: &'static str,
    detail: &'static str,
) -> impl Fn(rusqlite::Error) -> Rejection {
    move |e| {
        eprintln!("laded: store {stage} failed: {}", sql_code(&e));
        durability(detail)
    }
}

fn sql_code(error: &rusqlite::Error) -> &'static str {
    match error {
        rusqlite::Error::SqliteFailure(_, _) => "sqlite_failure",
        rusqlite::Error::InvalidColumnType(..) => "invalid_column",
        _ => "sql_error",
    }
}
