#![allow(dead_code, unused_imports)]

#[path = "../adapters_support/mod.rs"]
pub mod adapters_support;

use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use lade_protocol::client::{
    ClientBody, ClientEnvelope, Event, EventFrame, Rejection, Reply, ReplyData, ReplyResult,
    Request, RequestFrame,
};
use lade_protocol::types::{Capability, Id};
use lade_protocol::worker::{TurnState, WorkerBody, WorkerEnvelope};
use lade_protocol::{FrameReader, FrameWriter, Framing};
use lade_system::adapters::sandbox::FIXTURE_SWITCH;
use lade_system::service::{self, RunningService, ServiceConfig};
use lade_system::storage::Store;
use lade_system::supervision::credentials::CredentialSource;
use lade_system::supervision::launcher::PI_FLAGS;
use lade_system::supervision::{
    LaunchError, LaunchRequest, SupervisorEvent, WorkerControl, WorkerEvent, WorkerLauncher,
};

pub use adapters_support::{TempDir, commit_all, git, init_repo};

pub const TIMEOUT: Duration = Duration::from_secs(30);

pub fn new_id() -> Id {
    let mut bytes = [0u8; 16];
    lade_system::support::random_fill(&mut bytes).unwrap();
    Id::from_random_bytes(bytes)
}

pub struct Env {
    pub root: TempDir,
    pub repo: PathBuf,
}

impl Env {
    pub fn new(tag: &str) -> Self {
        let root = TempDir::new(tag);
        let repo = init_repo(root.path(), "sha1");
        Self { root, repo }
    }

    pub fn config(&self) -> ServiceConfig {
        ServiceConfig {
            state_dir: self.root.path().join("state"),
            runtime_dir: self.root.path().join("run"),
            limits: lade_system::service::ResourceLimits::default(),
            storage_limit_bytes: lade_system::adapters::sandbox::WRITABLE_STORAGE_BYTES,
            cgroups: None,
            test_fixture_provider: true,
        }
    }

    /// The service's database file.
    pub fn database(&self) -> PathBuf {
        self.root.path().join("state/lade.db")
    }

    /// The service's store, for checking or seeding durable state while the service is stopped.
    pub fn store(&self) -> Store {
        Store::open(&self.database()).expect("store opens")
    }

    /// A raw connection to the service's database, for state the store has no accessor for.
    pub fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.database()).expect("database opens")
    }

    pub fn start(&self, launcher: Arc<dyn WorkerLauncher>) -> RunningService {
        service::start(&self.config(), launcher).expect("service starts")
    }

    pub fn start_limited(
        &self,
        launcher: Arc<dyn WorkerLauncher>,
        limits: service::ResourceLimits,
    ) -> RunningService {
        let mut config = self.config();
        config.limits = limits;
        service::start(&config, launcher).expect("service starts")
    }

    pub fn capability(&self) -> String {
        fs::read_to_string(self.root.path().join("run/control.cap")).expect("capability")
    }

    pub fn socket(&self) -> PathBuf {
        self.root.path().join("run/control.sock")
    }

    pub fn connect(&self) -> Client {
        Client::connect(&self.socket(), &self.capability()).expect("admitted")
    }
}

pub struct Client {
    writer: FrameWriter<UnixStream>,
    frames: Receiver<ClientBody>,
    pub events: VecDeque<EventFrame>,
    replies: Vec<Reply>,
    pub service_epoch: Id,
}

pub fn raw_connect(socket: &Path) -> (FrameWriter<UnixStream>, Receiver<ClientBody>) {
    let stream = UnixStream::connect(socket).expect("connect");
    let read = stream.try_clone().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = FrameReader::new(read, Framing::Client);
        while let Ok(Some(envelope)) = reader.read::<ClientEnvelope>() {
            if tx.send(envelope.body).is_err() {
                break;
            }
        }
    });
    (FrameWriter::new(stream, Framing::Client), rx)
}

impl Client {
    pub fn connect(socket: &Path, capability: &str) -> Result<Self, String> {
        let (mut writer, frames) = raw_connect(socket);
        writer
            .write(&ClientEnvelope::new(ClientBody::Hello))
            .map_err(|e| e.to_string())?;
        match frames.recv_timeout(TIMEOUT) {
            Ok(ClientBody::Hello) => {}
            other => return Err(format!("expected hello: {other:?}")),
        }
        writer
            .write(&ClientEnvelope::new(ClientBody::Auth {
                capability: Capability::parse(capability.trim()).map_err(|e| e.to_string())?,
            }))
            .map_err(|e| e.to_string())?;
        match frames.recv_timeout(TIMEOUT) {
            Ok(ClientBody::Welcome { service_epoch, .. }) => Ok(Self {
                writer,
                frames,
                events: VecDeque::new(),
                replies: Vec::new(),
                service_epoch,
            }),
            Ok(ClientBody::Event(event)) => Err(format!("refused: {:?}", event.event)),
            other => Err(format!("expected welcome: {other:?}")),
        }
    }

    fn pump(&mut self, deadline: Instant) -> Option<ClientBody> {
        let left = deadline.saturating_duration_since(Instant::now());
        self.frames.recv_timeout(left).ok()
    }

    pub fn send(&mut self, request: Request, mutating: bool) -> Id {
        let request_id = new_id();
        let operation = mutating.then(new_id);
        self.send_frame(RequestFrame::new(request_id.clone(), operation, request).unwrap());
        request_id
    }

    pub fn write_raw(&mut self, bytes: &[u8]) {
        let mut stream: &UnixStream = self.writer.get_ref();
        stream.write_all(bytes).expect("write raw");
    }

    pub fn send_frame(&mut self, frame: RequestFrame) {
        self.writer
            .write(&ClientEnvelope::new(ClientBody::Request(frame)))
            .expect("write request");
    }

    pub fn wait_reply(&mut self, request_id: &Id) -> Reply {
        if let Some(index) = self
            .replies
            .iter()
            .position(|r| r.request_id == *request_id)
        {
            return self.replies.remove(index);
        }
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match self.pump(deadline) {
                Some(ClientBody::Reply(reply)) if reply.request_id == *request_id => return reply,
                Some(ClientBody::Reply(reply)) => self.replies.push(reply),
                Some(ClientBody::Event(event)) => self.events.push_back(event),
                Some(_) => {}
                None => panic!("no reply for {request_id}"),
            }
        }
    }

    pub fn call(&mut self, request: Request) -> Result<ReplyData, Rejection> {
        let mutating = request.name().is_mutating();
        let id = self.send(request, mutating);
        result(self.wait_reply(&id))
    }

    pub fn call_op(&mut self, operation: Id, request: Request) -> Result<ReplyData, Rejection> {
        let request_id = new_id();
        self.send_frame(RequestFrame::new(request_id.clone(), Some(operation), request).unwrap());
        result(self.wait_reply(&request_id))
    }

    pub fn wait_event(&mut self, what: &str, predicate: impl Fn(&Event) -> bool) -> Event {
        if let Some(index) = self.events.iter().position(|e| predicate(&e.event)) {
            return self.events.remove(index).unwrap().event;
        }
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match self.pump(deadline) {
                Some(ClientBody::Event(event)) if predicate(&event.event) => return event.event,
                Some(ClientBody::Event(event)) => self.events.push_back(event),
                Some(_) => {}
                None => panic!("timed out waiting for {what}"),
            }
        }
    }

    pub fn closed(&mut self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self
                .frames
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(ClientBody::Event(event)) => self.events.push_back(event),
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return true,
                Err(mpsc::RecvTimeoutError::Timeout) => return false,
            }
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.writer.get_ref().shutdown(std::net::Shutdown::Both);
    }
}

pub fn result(reply: Reply) -> Result<ReplyData, Rejection> {
    match reply.result {
        ReplyResult::Ok(data) => Ok(*data),
        ReplyResult::Rejected { rejection, .. } => Err(rejection),
    }
}

pub fn eventually<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out: {what}");
        thread::sleep(Duration::from_millis(100));
    }
}

pub struct NodeLauncher {
    pub entry: PathBuf,
}

impl NodeLauncher {
    pub fn shared() -> Arc<dyn WorkerLauncher> {
        let entry = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../packages/pi-worker/src/worker.ts")
            .canonicalize()
            .expect("worker entry");
        Arc::new(Self { entry })
    }
}

struct NodeControl {
    stdin: Option<FrameWriter<ChildStdin>>,
    child: Arc<Mutex<Child>>,
}

impl WorkerControl for NodeControl {
    fn send(&mut self, body: WorkerBody) -> std::io::Result<()> {
        let writer = self
            .stdin
            .as_mut()
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::BrokenPipe))?;
        writer
            .write(&WorkerEnvelope::new(body))
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe))
    }

    fn terminate(&mut self) {
        let _ = self.send(WorkerBody::Shutdown);
        self.stdin = None;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let done = self
                .child
                .lock()
                .unwrap()
                .try_wait()
                .ok()
                .flatten()
                .is_some();
            if done || Instant::now() > deadline {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.lock().unwrap().kill();
    }
}

impl Drop for NodeControl {
    fn drop(&mut self) {
        self.terminate();
    }
}

impl WorkerLauncher for NodeLauncher {
    fn preflight(&self, provider: &str) -> Result<(), LaunchError> {
        CredentialSource::empty()
            .check(provider)
            .map_err(|error| LaunchError::ProviderUnavailable(error.to_string()))
    }

    fn catalog(&self, scratch: &Path, models: Option<&Path>) -> Result<Vec<u8>, LaunchError> {
        let script = self.entry.with_file_name("worker").join("catalog.ts");
        let output = scratch.join("catalog.json");
        let mut command = Command::new("node");
        command
            .arg(&script)
            .arg(&output)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", scratch)
            .env("PI_CODING_AGENT_DIR", scratch)
            .envs(PI_FLAGS)
            .env(FIXTURE_SWITCH, "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(models) = models {
            command.arg(models);
        }
        let status = command.status().map_err(LaunchError::Io)?;
        if !status.success() {
            return Err(LaunchError::Io(std::io::Error::other("catalogue failed")));
        }
        std::fs::read(&output).map_err(LaunchError::Io)
    }

    fn launch(
        &self,
        request: LaunchRequest,
        events: SyncSender<SupervisorEvent>,
    ) -> Result<Box<dyn WorkerControl>, LaunchError> {
        self.preflight(&request.provider)?;
        let mut child = Command::new("node")
            .arg(&self.entry)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &request.session_dir)
            .env("LANG", "C.UTF-8")
            .env("TERM", "dumb")
            .env("PI_CODING_AGENT_DIR", request.session_dir.join("agent"))
            .env(
                "PI_CODING_AGENT_SESSION_DIR",
                request.session_dir.join("sessions"),
            )
            .envs(PI_FLAGS)
            .env(FIXTURE_SWITCH, "1")
            .current_dir(&request.worktree)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(LaunchError::Io)?;
        let stdout = child.stdout.take().unwrap();
        let mut stdin = FrameWriter::new(child.stdin.take().unwrap(), Framing::Worker);
        stdin
            .write(&WorkerEnvelope::new(request.start.clone()))
            .map_err(|_| LaunchError::Io(std::io::ErrorKind::BrokenPipe.into()))?;
        let child = Arc::new(Mutex::new(child));
        let waiter = Arc::clone(&child);
        let binding = request.binding.clone();
        thread::spawn(move || {
            let mut reader = FrameReader::new(stdout, Framing::Worker);
            loop {
                let event = match reader.read::<WorkerEnvelope>() {
                    Ok(Some(envelope)) if envelope.body.is_from_worker() => {
                        WorkerEvent::Body(envelope.body)
                    }
                    Ok(None) => break,
                    Ok(Some(_)) | Err(_) => {
                        let _ = waiter.lock().unwrap().kill();
                        WorkerEvent::ProtocolViolation("invalid_frame")
                    }
                };
                let violation = matches!(event, WorkerEvent::ProtocolViolation(_));
                let _ = events.send(SupervisorEvent {
                    binding: binding.clone(),
                    event,
                });
                if violation {
                    break;
                }
            }
            let status = loop {
                if let Ok(Some(status)) = waiter.lock().unwrap().try_wait() {
                    break status;
                }
                thread::sleep(Duration::from_millis(20));
            };
            let _ = events.send(SupervisorEvent {
                binding,
                event: WorkerEvent::Exited {
                    code: status.code(),
                    signal: status.signal(),
                },
            });
        });
        Ok(Box::new(NodeControl {
            stdin: Some(stdin),
            child,
        }))
    }
}

/// Whether the bubblewrap sandbox works here; a test that needs it skips otherwise, and fails
/// under `LADE_REQUIRE_SANDBOX=1`.
pub fn sandbox_works() -> bool {
    use lade_system::adapters::sandbox::{SandboxTools, sandbox_available};
    match SandboxTools::locate().and_then(|tools| sandbox_available(&tools)) {
        Ok(()) => true,
        Err(e) => {
            adapters_support::skip_without_sandbox(e);
            false
        }
    }
}

pub fn best_launcher(env: &Env) -> (Arc<dyn WorkerLauncher>, bool) {
    best_launcher_with(env, true)
}

pub fn best_launcher_with(env: &Env, fixture: bool) -> (Arc<dyn WorkerLauncher>, bool) {
    use lade_system::supervision::{LauncherConfig, default_launcher};
    if sandbox_works()
        && let Ok(launcher) = LauncherConfig::new(
            env.root.path().join("state/run"),
            env.root.path().join("run/g"),
            std::env::var_os("LADE_NODE").map(PathBuf::from),
            std::env::var_os("LADE_WORKER_DIR").map(PathBuf::from),
        )
        .and_then(|config| {
            default_launcher(LauncherConfig {
                test_fixture_provider: fixture,
                credentials: CredentialSource::empty(),
                ..config
            })
        })
    {
        return (Arc::from(launcher), true);
    }
    eprintln!("note: sandbox unavailable; using the unsandboxed test launcher");
    (NodeLauncher::shared(), false)
}

pub struct FakeLauncher {
    ack: bool,
}

impl FakeLauncher {
    pub fn shared() -> Arc<dyn WorkerLauncher> {
        Arc::new(Self { ack: true })
    }

    pub fn silent() -> Arc<dyn WorkerLauncher> {
        Arc::new(Self { ack: false })
    }
}

struct FakeControl {
    binding: lade_system::supervision::LaunchBinding,
    events: SyncSender<SupervisorEvent>,
    alive: bool,
    ack: bool,
}

impl FakeControl {
    fn emit(&self, event: WorkerEvent) {
        let _ = self.events.send(SupervisorEvent {
            binding: self.binding.clone(),
            event,
        });
    }
}

impl WorkerControl for FakeControl {
    fn send(&mut self, body: WorkerBody) -> std::io::Result<()> {
        if !self.alive {
            return Err(std::io::ErrorKind::BrokenPipe.into());
        }
        if self.ack
            && let WorkerBody::Deliver { input_id, .. } = body
        {
            self.emit(WorkerEvent::Body(WorkerBody::Received { input_id }));
            self.emit(WorkerEvent::Body(WorkerBody::TurnState {
                state: TurnState::Running,
            }));
            self.emit(WorkerEvent::Body(WorkerBody::TurnState {
                state: TurnState::Idle,
            }));
        }
        Ok(())
    }

    fn terminate(&mut self) {
        if std::mem::take(&mut self.alive) {
            self.emit(WorkerEvent::Exited {
                code: Some(0),
                signal: None,
            });
        }
    }
}

impl WorkerLauncher for FakeLauncher {
    fn launch(
        &self,
        request: LaunchRequest,
        events: SyncSender<SupervisorEvent>,
    ) -> Result<Box<dyn WorkerControl>, LaunchError> {
        let control = FakeControl {
            binding: request.binding,
            events,
            alive: true,
            ack: self.ack,
        };
        control.emit(WorkerEvent::Body(WorkerBody::Ready {
            pi_session_id: "fake-session".into(),
            session_file: String::new(),
        }));
        Ok(Box::new(control))
    }
}

pub fn raw_write(socket: &Path, bytes: &[u8]) -> UnixStream {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream.write_all(bytes).unwrap();
    stream
}

use lade_protocol::client::{
    Basis, ChangeList, ExecutionStartCommand, ExecutionState, FileContent, FileReadQuery, Intent,
    ObjectiveCreateCommand, ObjectiveDetail, ObjectiveSelector, Side, Start, Workspace,
    WorkspaceOpenCommand,
};
use lade_protocol::types::{Counter, Model, ReadLimit, WirePath};

pub fn open_workspace(client: &mut Client, path: &Path) -> Workspace {
    match client.call(Request::WorkspaceOpen(WorkspaceOpenCommand {
        path: WirePath::from_os_str(path.as_os_str()).unwrap(),
    })) {
        Ok(ReplyData::WorkspaceOpen(ws)) => ws,
        other => panic!("workspace.open: {other:?}"),
    }
}

pub fn create_objective(client: &mut Client, workspace: &Id, title: &str) -> ObjectiveDetail {
    match client.call(Request::ObjectiveCreate(ObjectiveCreateCommand {
        workspace_id: workspace.clone(),
        title: title.into(),
        intent: Intent {
            outcome: format!("{title} outcome"),
            constraints: String::new(),
            acceptance: "tests pass".into(),
        },
        integration_target: "main".into(),
        start: Start::Target,
    })) {
        Ok(ReplyData::ObjectiveCreate(detail)) => *detail,
        other => panic!("objective.create: {other:?}"),
    }
}

pub fn detail(client: &mut Client, objective: &Id) -> ObjectiveDetail {
    match client.call(Request::ObjectiveGet(ObjectiveSelector {
        objective_id: objective.clone(),
    })) {
        Ok(ReplyData::ObjectiveGet(detail)) => *detail,
        other => panic!("objective.get: {other:?}"),
    }
}

pub fn start_execution(client: &mut Client, objective: &Id) -> ObjectiveDetail {
    match client.call(Request::ExecutionStart(ExecutionStartCommand {
        objective_id: objective.clone(),
        model: Model {
            provider: "lade-fixture".into(),
            id: "scripted".into(),
        },
        thinking: None,
    })) {
        Ok(ReplyData::ExecutionStart(detail)) => *detail,
        other => panic!("execution.start: {other:?}"),
    }
}

pub fn wait_state(client: &mut Client, objective: &Id, state: ExecutionState) -> ObjectiveDetail {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let d = detail(client, objective);
        if d.execution.state == state {
            return d;
        }
        assert!(
            Instant::now() < deadline,
            "timed out: execution {state:?}; last {:?}",
            d.execution
        );
        thread::sleep(Duration::from_millis(100));
    }
}

pub fn changes(client: &mut Client, objective: &Id, basis: Basis) -> ChangeList {
    match client.call(Request::ChangesList(
        lade_protocol::client::ChangesListQuery {
            objective_id: objective.clone(),
            basis,
        },
    )) {
        Ok(ReplyData::ChangesList(list)) => list,
        other => panic!("changes.list: {other:?}"),
    }
}

pub fn read_file(client: &mut Client, objective: &Id, basis: Basis, path: &str) -> FileContent {
    match client.call(Request::FileRead(FileReadQuery {
        objective_id: objective.clone(),
        basis,
        path: WirePath::from_bytes(path.as_bytes().to_vec()).unwrap(),
        side: Side::New,
        offset: Counter::new(0),
        limit: ReadLimit::parse("262144").unwrap(),
    })) {
        Ok(ReplyData::FileRead(content)) => content,
        other => panic!("file.read: {other:?}"),
    }
}

pub fn action_enabled(detail: &ObjectiveDetail, action: lade_protocol::client::ActionName) -> bool {
    detail
        .available_actions
        .iter()
        .any(|a| a.action == action && a.enabled)
}

pub fn worktree(env: &Env, objective: &Id) -> PathBuf {
    env.root
        .path()
        .join("state/worktrees")
        .join(objective.as_str())
}

pub struct Daemon {
    child: Child,
    pub log: PathBuf,
}

static DAEMON_RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl Env {
    pub fn spawn_laded(&self, fault: Option<&str>, extra: &[&str]) -> Daemon {
        let run = DAEMON_RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let log = self.root.path().join(format!("laded-{run}.log"));
        let mut command = Command::new(env!("CARGO_BIN_EXE_laded"));
        command
            .arg("serve")
            .arg("--state-dir")
            .arg(self.root.path().join("state"))
            .arg("--runtime-dir")
            .arg(self.root.path().join("run"))
            .args(extra)
            .env(FIXTURE_SWITCH, "1")
            .env_remove("LADE_FAULT_POINT")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(&log).unwrap());
        if let Some(point) = fault {
            command.env("LADE_FAULT_POINT", point);
        }
        let mut daemon = Daemon {
            child: command.spawn().expect("spawn laded"),
            log,
        };
        let deadline = Instant::now() + TIMEOUT;
        while !daemon.log_text().contains("laded: serving epoch=") {
            if let Ok(Some(status)) = daemon.child.try_wait() {
                panic!("laded exited early ({status}): {}", daemon.log_text());
            }
            assert!(
                Instant::now() < deadline,
                "laded did not start: {}",
                daemon.log_text()
            );
            thread::sleep(Duration::from_millis(50));
        }
        daemon
    }
}

impl Daemon {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn log_text(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    pub fn wait_exit(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "laded did not exit: {}",
                self.log_text()
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.kill();
    }
}

pub fn processes_mentioning(needle: &str) -> Vec<u32> {
    let me = std::process::id();
    fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| *pid != me)
        .filter(|pid| {
            let live = fs::read_to_string(format!("/proc/{pid}/stat"))
                .is_ok_and(|s| s.rsplit(") ").next().is_some_and(|r| !r.starts_with('Z')));
            live && fs::read(format!("/proc/{pid}/cmdline"))
                .is_ok_and(|c| c.windows(needle.len()).any(|w| w == needle.as_bytes()))
        })
        .collect()
}
