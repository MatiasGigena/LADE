mod compat;
pub mod convert;
mod detail_budget;
mod execution;
mod handlers;
mod inspect;
mod jobs;
mod limits;
mod project;
mod recovery;
mod review;
mod state;
mod terminal;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use lade_core::domain::ids::{GitId, ObjectiveId, WorkspaceId};
use lade_protocol::client::{
    ClientBody, ClientEnvelope, ErrorCode, Event, EventFrame, NoticeLevel, Outcome, Rejection,
    Reply, ReplyData, ReplyResult, RequestFrame, RequestName, ReviewIntegrateCommand, SafeAction,
    ServiceNotice, Settings,
};
use lade_protocol::types::Id;
use lade_protocol::{Framing, encode};
use rusqlite::Transaction;

use crate::adapters::artifacts::ArtifactStore;
use crate::adapters::sandbox::{
    STORAGE_CHECK_INTERVAL, SandboxCgroups, SandboxTools, WRITABLE_STORAGE_BYTES,
};
use crate::storage::{self, Store, StoreError};
use crate::supervision::launcher::RESERVED_GENERATION;
use crate::supervision::{
    GATEWAY_SOCKET_DIR, SupervisorEvent, WorkerLauncher, gateway_socket_path, socket_path_fits,
};
use crate::transport::{self, Inbound, Listener, Outbound, TransportError};

pub use limits::ResourceLimits;

use convert::{durability, hex, new_uuid, now_ms, wire_id};
use jobs::Pool;
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
    AlreadyRunning,
    Store(StoreError),
    Transport(TransportError),
    Recovery(String),
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
            Self::AlreadyRunning => f.write_str("another service owns this state directory"),
            Self::Store(e) => write!(f, "{e}"),
            Self::Transport(e) => write!(f, "{e}"),
            Self::Recovery(e) => write!(f, "restart recovery failed: {e}"),
        }
    }
}

impl std::error::Error for StartError {}

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
    Job { token: u64, output: JobOutput },
    Tick,
    Shutdown,
}

impl From<Inbound> for Msg {
    fn from(inbound: Inbound) -> Self {
        Self::Link(inbound)
    }
}

#[allow(clippy::large_enum_variant)]
pub(crate) enum JobOutput {
    Launch(jobs::LaunchOut),
    Capture(Result<jobs::CaptureOut, Rejection>),
    Freeze(Result<jobs::FreezeOut, Rejection>),
    Refresh(Result<jobs::RefreshOut, Rejection>),
    Verify(jobs::VerifyOut),
    Integrate(Result<crate::adapters::git::SquashOutcome, crate::adapters::git::GitError>),
    Unit(Result<(), Rejection>),
    Flag(Result<bool, Rejection>),
    Query(Result<ReplyData, Rejection>),
    Catalog(Result<compat::CatalogData, String>),
    Profile(Result<crate::adapters::profile::ImportReport, String>),
    Snapshot(Result<(lade_protocol::worker::WorkerResources, PathBuf), String>),
    Terminal(Result<crate::supervision::Terminal, Rejection>),
    Storage(Vec<execution::StorageUsage>),
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
const GENERAL_THREADS: usize = 3;
const VERIFY_THREADS: usize = 4;
const REAPER_THREADS: usize = 4;
const TICK: Duration = Duration::from_millis(100);

pub(crate) struct Service {
    store: Store,
    paths: Paths,
    artifacts: Arc<ArtifactStore>,
    large_blobs: Arc<jobs::LargeBlobs>,
    launcher: Arc<dyn WorkerLauncher>,
    supervisor_tx: SyncSender<SupervisorEvent>,
    queue_tx: SyncSender<Msg>,
    pool: Option<Pool>,
    long: Option<Pool>,
    reaper: Option<Pool>,
    epoch: Id,
    sequence: u64,
    controller: Option<Controller>,
    workspaces: BTreeMap<WorkspaceId, WsState>,
    objectives: BTreeMap<ObjectiveId, ObjState>,
    runtime: HashMap<ObjectiveId, Runtime>,
    settings: Settings,
    target_cache: HashMap<(WorkspaceId, String), GitId>,
    pending: HashMap<u64, handlers::Pending>,
    next_token: u64,
    activity_seq: HashMap<ObjectiveId, u64>,
    input_seq: u64,
    live: execution::LiveActivity,
    ticks: u64,
    sandbox: OnceLock<Option<SandboxTools>>,
    compat: compat::CompatState,
    terminals: BTreeMap<String, terminal::TermState>,
    terminals_opening: usize,
    stopping: bool,
    limits: ResourceLimits,
    free_space: limits::Storage,
    recovery_notes: Vec<String>,
    integrating: HashMap<(WorkspaceId, String), VecDeque<(ReqCtx, ReviewIntegrateCommand)>>,
    held: Option<Vec<(bool, jobs::Task)>>,
    storage_limit: u64,
    storage_check: bool,
    cgroups: Option<Arc<SandboxCgroups>>,
    test_fixture_provider: bool,
}

pub struct RunningService {
    queue: SyncSender<Msg>,
    thread: Option<JoinHandle<()>>,
    listener: Option<Listener>,
    ticker_stop: Arc<AtomicBool>,
    ticker: Option<JoinHandle<()>>,
    socket: PathBuf,
    capability: PathBuf,
    epoch: Id,
    _lock: File,
}

impl RunningService {
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    pub fn capability_path(&self) -> &Path {
        &self.capability
    }

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
        if let Some(listener) = self.listener.take() {
            listener.stop();
        }
        self.ticker_stop.store(true, Ordering::SeqCst);
        if let Some(ticker) = self.ticker.take() {
            let _ = ticker.join();
        }
        let _ = fs::remove_file(&self.socket);
        let _ = fs::remove_file(&self.capability);
        thread::sleep(Duration::from_millis(200));
    }

    pub fn shutdown(mut self) {
        if let Some(listener) = self.listener.take() {
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
    for path in [runtime.join(transport::SOCKET_NAME), widest] {
        if !socket_path_fits(&path) {
            return Err(StartError::SocketPathTooLong(path));
        }
    }
    Ok(())
}

fn private_dir(path: &Path) -> Result<(), StartError> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|e| StartError::StateDir(format!("{}: {}", path.display(), e.kind())))
}

pub(crate) fn lock_state(state: &Path) -> Result<File, StartError> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(state.join("lade.lock"))
        .map_err(|e| StartError::StateDir(format!("lock: {}", e.kind())))?;
    for _ in 0..40 {
        if rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive).is_ok() {
            return Ok(file);
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(StartError::AlreadyRunning)
}

pub fn start(
    config: ServiceConfig,
    launcher: Arc<dyn WorkerLauncher>,
) -> Result<RunningService, StartError> {
    if !config.state_dir.is_absolute() {
        return Err(StartError::InvalidPath("state directory"));
    }
    if !config.runtime_dir.is_absolute() {
        return Err(StartError::InvalidPath("runtime directory"));
    }
    check_socket_paths(&config.runtime_dir)?;
    transport::prepare_private_dir(&config.runtime_dir).map_err(StartError::Transport)?;
    private_dir(&config.state_dir)?;
    transport::prepare_private_dir(&config.state_dir).map_err(StartError::Transport)?;
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
    let lock = lock_state(&paths.state)?;
    transport::ensure_not_running(&config.runtime_dir).map_err(StartError::Transport)?;
    let store = Store::open(&paths.state.join("lade.db")).map_err(StartError::Store)?;
    let artifacts = ArtifactStore::open(&paths.state.join("artifacts"), &paths.tmp)
        .map_err(|e| StartError::StateDir(format!("artifacts: {}", e.code())))?;
    let (queue_tx, queue_rx) = mpsc::sync_channel::<Msg>(8192);
    let (supervisor_tx, supervisor_rx) = mpsc::sync_channel::<SupervisorEvent>(1024);
    let epoch = wire_id(new_uuid());
    let mut service = Service {
        store,
        paths,
        artifacts: Arc::new(artifacts),
        large_blobs: Arc::default(),
        launcher,
        supervisor_tx,
        queue_tx: queue_tx.clone(),
        pool: Some(Pool::new(GENERAL_THREADS)),
        long: Some(Pool::new(VERIFY_THREADS)),
        reaper: Some(Pool::new(REAPER_THREADS)),
        epoch: epoch.clone(),
        sequence: 0,
        controller: None,
        workspaces: BTreeMap::new(),
        objectives: BTreeMap::new(),
        runtime: HashMap::new(),
        settings: Settings {
            verification_commands: Vec::new(),
            default_model: None,
        },
        target_cache: HashMap::new(),
        pending: HashMap::new(),
        next_token: 1,
        activity_seq: HashMap::new(),
        input_seq: 0,
        live: execution::LiveActivity::default(),
        ticks: 0,
        sandbox: OnceLock::new(),
        compat: compat::CompatState::default(),
        terminals: BTreeMap::new(),
        terminals_opening: 0,
        stopping: false,
        limits: config.limits.clone(),
        free_space: limits::Storage::default(),
        recovery_notes: Vec::new(),
        integrating: HashMap::new(),
        held: None,
        storage_limit: config.storage_limit_bytes,
        storage_check: false,
        cgroups: config.cgroups.clone(),
        test_fixture_provider: config.test_fixture_provider,
    };
    service.load().map_err(StartError::Recovery)?;
    service.load_compat().map_err(StartError::Recovery)?;
    service.recover().map_err(StartError::Recovery)?;
    service.check_free_space();
    eprintln!("laded: limits {}", config.limits);

    let capability =
        transport::write_capability(&config.runtime_dir).map_err(StartError::Transport)?;
    let listener = transport::bind_socket(&config.runtime_dir).map_err(StartError::Transport)?;
    let socket = config.runtime_dir.join(transport::SOCKET_NAME);
    let listener = transport::serve(listener, socket.clone(), capability, queue_tx.clone());

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
        .map_err(|e| StartError::StateDir(format!("thread: {}", e.kind())))?;
    let ticker_stop = Arc::new(AtomicBool::new(false));
    let stop = Arc::clone(&ticker_stop);
    let tick_tx = queue_tx.clone();
    let ticker = thread::Builder::new()
        .name("laded-tick".into())
        .spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                thread::sleep(TICK);
                let _ = tick_tx.try_send(Msg::Tick);
            }
        })
        .map_err(|e| StartError::StateDir(format!("thread: {}", e.kind())))?;
    let thread = thread::Builder::new()
        .name("laded-service".into())
        .spawn(move || service.run(&queue_rx))
        .map_err(|e| StartError::StateDir(format!("thread: {}", e.kind())))?;
    Ok(RunningService {
        queue: queue_tx,
        thread: Some(thread),
        listener: Some(listener),
        ticker_stop,
        ticker: Some(ticker),
        socket,
        capability: config.runtime_dir.join(transport::CAPABILITY_NAME),
        epoch,
        _lock: lock,
    })
}

fn recordable(rejection: &Rejection) -> bool {
    rejection.code != ErrorCode::DurabilityFailure
}

impl Service {
    fn run(mut self, queue: &Receiver<Msg>) {
        while let Ok(message) = queue.recv() {
            match message {
                Msg::Link(inbound) => self.on_link(inbound),
                Msg::Supervisor(event) => self.on_supervisor(event),
                Msg::Job { token, output } => self.on_job(token, output),
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
        for pool in [self.reaper.take(), self.pool.take(), self.long.take()]
            .into_iter()
            .flatten()
        {
            pool.shutdown();
        }
    }

    fn on_link(&mut self, inbound: Inbound) {
        match inbound {
            Inbound::Attach { conn, out } => {
                if self.controller.is_some() {
                    let notice = self.encode_event_frame(
                        0,
                        Event::ServiceNotice(ServiceNotice {
                            level: NoticeLevel::Error,
                            code: "unauthorized".into(),
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
                let pushed = encode(&welcome, Framing::Client)
                    .ok()
                    .is_some_and(|bytes| out.push(bytes).is_ok());
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
                        self.notice(NoticeLevel::Warning, "recovery", &note);
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
            sequence: convert::counter(sequence),
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
                code: "link_overflow".into(),
                message: why.into(),
            }),
        );
        eprintln!(
            "laded: dropped client link conn={}: overflow",
            controller.conn
        );
        controller.out.close_with(notice);
    }

    pub(crate) fn notice(&mut self, level: NoticeLevel, code: &str, message: &str) {
        self.emit(Event::ServiceNotice(ServiceNotice {
            level,
            code: convert::preview(code, 64),
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
        let digest = frame.canonical_digest();
        let name = frame.request.name();
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
            match storage::load_operation(self.store.conn(), op.id.as_str()) {
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
                    let reply = self.duplicate_reply(frame.request_id, name, op, &row);
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
                op.id.as_str(),
                &op.digest,
                name.as_str(),
                "pending",
                None,
                now_ms(),
            );
            if recorded.is_err() {
                eprintln!("laded: could not journal pending operation");
            }
        }
        for (long, task) in held {
            let pool = if long { &self.long } else { &self.pool };
            if let Some(pool) = pool {
                pool.submit(task);
            }
        }
    }

    fn duplicate_reply(
        &self,
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
                "completed"
            } else {
                "uncertain"
            };
            let recorded = storage::record_operation(
                self.store.conn(),
                op.id.as_str(),
                &op.digest,
                ctx.name.as_str(),
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
            .map_err(|_| durability("store unavailable"))?;
        writes(&tx).map_err(|e| {
            eprintln!("laded: store write failed: {}", sql_code(&e));
            durability("state could not be recorded")
        })?;
        let mut journaled = false;
        if let (Some(ctx), Some(data)) = (ctx.as_deref(), outcome)
            && let Some(op) = &ctx.op
        {
            storage::record_operation(
                &tx,
                op.id.as_str(),
                &op.digest,
                ctx.name.as_str(),
                "completed",
                Some(&Outcome::Ok(Box::new(data.clone()))),
                now,
            )
            .map_err(|_| durability("operation outcome could not be recorded"))?;
            journaled = true;
        }
        tx.commit()
            .map_err(|_| durability("state could not be recorded"))?;
        if journaled && let Some(ctx) = ctx {
            ctx.recorded = true;
        }
        Ok(())
    }

    pub(crate) fn spawn(
        &mut self,
        pending: handlers::Pending,
        work: impl FnOnce() -> JobOutput + Send + 'static,
    ) {
        self.spawn_in(false, pending, work);
    }

    pub(crate) fn spawn_long(
        &mut self,
        pending: handlers::Pending,
        work: impl FnOnce() -> JobOutput + Send + 'static,
    ) {
        self.spawn_in(true, pending, work);
    }

    fn spawn_in(
        &mut self,
        long: bool,
        pending: handlers::Pending,
        work: impl FnOnce() -> JobOutput + Send + 'static,
    ) {
        let token = self.next_token;
        self.next_token += 1;
        self.pending.insert(token, pending);
        let tx = self.queue_tx.clone();
        let task: jobs::Task = Box::new(move || {
            let output = work();
            let _ = tx.send(Msg::Job { token, output });
        });
        if let Some(held) = self.held.as_mut() {
            held.push((long, task));
            return;
        }
        let pool = if long { &self.long } else { &self.pool };
        if let Some(pool) = pool {
            pool.submit(task);
        }
    }

    pub(crate) fn reap(&self, work: impl FnOnce() + Send + 'static) {
        if let Some(reaper) = &self.reaper {
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
        if self
            .ticks
            .is_multiple_of(STORAGE_CHECK_INTERVAL.as_millis() as u64 / TICK.as_millis() as u64)
        {
            self.check_storage();
        }
        self.check_timeouts();
        self.compat_timeouts();
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

fn sql_code(error: &rusqlite::Error) -> &'static str {
    match error {
        rusqlite::Error::SqliteFailure(_, _) => "sqlite_failure",
        rusqlite::Error::InvalidColumnType(..) => "invalid_column",
        _ => "sql_error",
    }
}
