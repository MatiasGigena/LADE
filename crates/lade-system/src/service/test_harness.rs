//! A [`Service`] over a temporary state directory and one fixture repository, for unit tests
//! that drive handlers in process: no socket and no sandbox. Background jobs are held and run
//! on the test thread, workers are recorded fakes, and replies and events are read back from
//! the controlling link's queue.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};

use lade_core::domain::ids::ObjectiveId;
use lade_protocol::client::{
    CandidateSummary, ClientBody, Event, ExecutionStartCommand, Intent, ObjectiveCreateCommand,
    Rejection, ReplyData, ReplyResult, Request, RequestName, ReviewApproveCommand, Start,
    WorkspaceOpenCommand,
};
use lade_protocol::decode_client_frame;
use lade_protocol::types::{GitId as WireGitId, Id, Model, WirePath};
use lade_protocol::worker::WorkerBody;

use super::jobs::Pools;
use super::link::Controller;
use super::{Flow, Msg, OpInfo, Paths, ReqCtx, ResourceLimits, Service, ServiceConfig};
use crate::adapters::artifacts::ArtifactStore;
use crate::adapters::sandbox::WRITABLE_STORAGE_BYTES;
use crate::adapters_support::{TempDir, git, init_repo};
use crate::storage::Store;
use crate::supervision::{
    LaunchBinding, LaunchError, LaunchRequest, SupervisorEvent, WorkerControl, WorkerEvent,
    WorkerLauncher,
};
use crate::support::fs::ensure_private_dir;
use crate::support::new_uuid;
use crate::transport::Outbound;

const CONN: u64 = 1;

/// What a fake worker was sent, and whether it was terminated.
#[derive(Default)]
pub(crate) struct WorkerLog {
    pub sent: Vec<WorkerBody>,
    pub terminated: bool,
}

struct FakeControl(Arc<Mutex<WorkerLog>>);

impl WorkerControl for FakeControl {
    fn send(&mut self, body: WorkerBody) -> std::io::Result<()> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .sent
            .push(body);
        Ok(())
    }

    fn terminate(&mut self) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .terminated = true;
    }
}

/// Records every launch and hands back a [`FakeControl`]; set `refuse` to make the next
/// launches fail as an unavailable sandbox would.
#[derive(Default)]
pub(crate) struct FakeLauncher {
    pub launches: Mutex<Vec<(LaunchRequest, Arc<Mutex<WorkerLog>>)>>,
    pub refuse: Mutex<Option<String>>,
    pub unavailable_provider: Mutex<Option<String>>,
}

impl WorkerLauncher for FakeLauncher {
    fn launch(
        &self,
        request: LaunchRequest,
        _events: SyncSender<SupervisorEvent>,
    ) -> Result<Box<dyn WorkerControl>, LaunchError> {
        if let Some(why) = self
            .refuse
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        {
            return Err(LaunchError::SandboxUnavailable(why));
        }
        let log = Arc::new(Mutex::new(WorkerLog::default()));
        self.launches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((request, Arc::clone(&log)));
        Ok(Box::new(FakeControl(log)))
    }

    fn preflight(&self, provider: &str) -> Result<(), LaunchError> {
        let unavailable = self
            .unavailable_provider
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match unavailable.as_deref() {
            Some(name) if name == provider => Err(LaunchError::ProviderUnavailable(format!(
                "{provider} has no credentials"
            ))),
            _ => Ok(()),
        }
    }
}

pub(crate) struct Harness {
    pub service: Service,
    pub repo: PathBuf,
    pub launcher: Arc<FakeLauncher>,
    out: Outbound,
    queue: Receiver<Msg>,
    /// Held so the supervisor channel stays open for the harness's lifetime.
    supervisor: Receiver<SupervisorEvent>,
    /// Events read off the link while looking for replies, oldest first.
    pub events: VecDeque<Event>,
    replies: Vec<(Id, ReplyResult)>,
    root: TempDir,
}

impl Harness {
    /// A service with limits `limits`, one SHA-1 fixture repository, and a controlling client.
    pub fn with_limits(tag: &str, limits: ResourceLimits) -> Self {
        let root = TempDir::new(tag);
        let repo = init_repo(root.path(), "sha1");
        let state = root.path().join("state");
        let paths = Paths {
            worktrees: state.join("worktrees"),
            sessions: state.join("sessions"),
            verify: state.join("verify"),
            tmp: state.join("tmp"),
            run: state.join("run"),
            sockets: state.join("sockets"),
            state,
        };
        for dir in [
            &paths.state,
            &paths.worktrees,
            &paths.sessions,
            &paths.verify,
            &paths.tmp,
        ] {
            ensure_private_dir(dir).expect("private dir");
        }
        let launcher = Arc::new(FakeLauncher::default());
        let (mut service, queue, supervisor) = boot(paths, limits, &launcher);
        let out = attach(&mut service);
        Self {
            service,
            repo,
            launcher,
            out,
            queue,
            supervisor,
            events: VecDeque::new(),
            replies: Vec::new(),
            root,
        }
    }

    /// Replaces the service with a new one over the same state, as a restart would: the
    /// durable state is loaded and recovered, and a fresh client attaches.
    pub fn restart(&mut self) {
        let paths = self.service.paths.clone();
        let limits = self.service.limits.clone();
        let (mut service, queue, supervisor) = boot(paths, limits, &self.launcher);
        self.out = attach(&mut service);
        self.service = service;
        self.queue = queue;
        self.supervisor = supervisor;
        self.events.clear();
        self.replies.clear();
    }

    pub fn new(tag: &str) -> Self {
        Self::with_limits(tag, ResourceLimits::default())
    }

    pub fn root(&self) -> &Path {
        self.root.path()
    }

    /// A request context as the controlling client sends it: mutating requests carry an
    /// operation id, as the protocol requires.
    pub fn ctx(name: RequestName) -> ReqCtx {
        ReqCtx {
            conn: CONN,
            request_id: super::wire_id(new_uuid()),
            name,
            op: name.is_mutating().then(|| OpInfo {
                id: super::wire_id(new_uuid()),
                digest: "test".into(),
            }),
            recorded: false,
        }
    }

    /// Runs held background jobs, and the jobs their completions start, until none are left.
    pub fn settle(&mut self) {
        loop {
            let tasks = self.service.held.replace(Vec::new()).unwrap_or_default();
            let mut ran = !tasks.is_empty();
            for (_, task) in tasks {
                task();
            }
            while let Ok(message) = self.queue.try_recv() {
                if let Msg::Job(completed) = message {
                    self.service.on_job(completed);
                    ran = true;
                }
            }
            if !ran {
                return;
            }
        }
    }

    fn read_link(&mut self) {
        for frame in self.out.drain() {
            let envelope = decode_client_frame(&frame[4..]).expect("service frames decode");
            match envelope.body {
                ClientBody::Reply(reply) => self.replies.push((reply.request_id, reply.result)),
                ClientBody::Event(frame) => self.events.push_back(frame.event),
                _ => {}
            }
        }
    }

    /// Dispatches `request` as the controlling client would, settles the jobs it starts, and
    /// returns its reply.
    pub fn call(&mut self, request: Request) -> Result<ReplyData, Rejection> {
        let ctx = Self::ctx(request.name());
        let request_id = ctx.request_id.clone();
        self.service.dispatch(ctx, request);
        self.settle();
        self.reply(&request_id)
            .unwrap_or_else(|| panic!("no reply to {request_id:?}"))
    }

    /// The reply to `request_id`, once one was sent.
    pub fn reply(&mut self, request_id: &Id) -> Option<Result<ReplyData, Rejection>> {
        self.read_link();
        let index = self.replies.iter().position(|(id, _)| id == request_id)?;
        Some(match self.replies.remove(index).1 {
            ReplyResult::Ok(data) => Ok(*data),
            ReplyResult::Rejected { rejection, .. } => Err(rejection),
        })
    }

    /// Takes the events emitted so far.
    pub fn take_events(&mut self) -> Vec<Event> {
        self.read_link();
        self.events.drain(..).collect()
    }

    pub fn open_workspace(&mut self) -> Id {
        let path = WirePath::from_os_str(self.repo.as_os_str()).expect("path");
        match self.call(Request::WorkspaceOpen(WorkspaceOpenCommand { path })) {
            Ok(ReplyData::WorkspaceOpen(workspace)) => workspace.workspace_id,
            other => panic!("workspace open failed: {other:?}"),
        }
    }

    /// Opens the fixture repository (once) and defines an Objective targeting `main`.
    pub fn create_objective(&mut self, title: &str) -> (Id, ObjectiveId) {
        let workspace_id = match self.service.workspaces.values().next() {
            Some(ws) => super::wire_id(ws.id.uuid()),
            None => self.open_workspace(),
        };
        let reply = self.call(Request::ObjectiveCreate(ObjectiveCreateCommand {
            workspace_id,
            title: title.into(),
            intent: Intent {
                outcome: format!("{title} is done"),
                constraints: String::new(),
                acceptance: String::new(),
            },
            integration_target: "main".into(),
            start: Start::Target,
        }));
        let Ok(ReplyData::ObjectiveCreate(detail)) = reply else {
            panic!("objective create failed: {reply:?}");
        };
        let oid = self
            .service
            .objective_id(&detail.objective_id)
            .expect("oid");
        (detail.objective_id, oid)
    }

    /// Starts an agent for a new Objective, lets it write `file`, and freezes its work.
    pub fn frozen(&mut self, title: &str, file: &str) -> (Id, ObjectiveId, CandidateSummary) {
        let (id, oid) = self.create_objective(title);
        self.call(Request::ExecutionStart(ExecutionStartCommand {
            objective_id: id.clone(),
            model: Model {
                provider: "anthropic".into(),
                id: "model".into(),
            },
            thinking: None,
        }))
        .expect("start");
        self.worker_event(
            oid,
            WorkerEvent::Body(WorkerBody::Ready {
                pi_session_id: "pi".into(),
                session_file: "/s.jsonl".into(),
            }),
        );
        let worktree = self.service.objectives[&oid]
            .row
            .worktree
            .clone()
            .expect("worktree");
        fs::write(worktree.join(file), format!("{title}\n")).expect("agent work");
        let ctx = Self::ctx(RequestName::ReviewFreeze);
        let request_id = ctx.request_id.clone();
        assert!(matches!(
            self.service.review_freeze(ctx, &id),
            Flow::Deferred
        ));
        assert!(
            self.reply(&request_id).is_none(),
            "the freeze waits for the worker to stop"
        );
        let (_, log) = self.last_worker();
        assert!(matches!(
            log.lock().expect("log").sent.last(),
            Some(WorkerBody::Abort)
        ));
        self.worker_event(
            oid,
            WorkerEvent::Exited {
                code: Some(0),
                signal: None,
            },
        );
        let Some(Ok(ReplyData::ReviewFreeze(candidate))) = self.reply(&request_id) else {
            panic!("freeze");
        };
        (id, oid, candidate)
    }

    pub fn approve(
        &mut self,
        id: &Id,
        candidate: &CandidateSummary,
    ) -> Result<ReplyData, Rejection> {
        let target_commit = self.main_head();
        self.call(Request::ReviewApprove(ReviewApproveCommand {
            objective_id: id.clone(),
            candidate_id: candidate.candidate_id.clone(),
            target_commit,
            override_reason: Some("reviewed by hand".into()),
        }))
    }

    /// The commit `main` points at in the fixture repository.
    pub fn main_head(&self) -> WireGitId {
        WireGitId::from_hex(git(&self.repo, &["rev-parse", "main"]).trim()).expect("head")
    }

    /// The worker launched most recently and what it was sent.
    pub fn last_worker(&self) -> (LaunchRequest, Arc<Mutex<WorkerLog>>) {
        let launches = self
            .launcher
            .launches
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (request, log) = launches.last().expect("a worker was launched");
        (request.clone(), Arc::clone(log))
    }

    /// Delivers `event` as if the current worker of `oid` sent it.
    pub fn worker_event(&mut self, oid: ObjectiveId, event: WorkerEvent) {
        let generation = self.service.objectives[&oid]
            .objective()
            .execution()
            .generation()
            .value();
        self.service.on_supervisor(SupervisorEvent {
            binding: LaunchBinding {
                objective_id: oid.to_string(),
                execution_generation: generation,
            },
            event,
        });
        self.settle();
    }
}

fn boot(
    paths: Paths,
    limits: ResourceLimits,
    launcher: &Arc<FakeLauncher>,
) -> (Service, Receiver<Msg>, Receiver<SupervisorEvent>) {
    let config = ServiceConfig {
        state_dir: paths.state.clone(),
        runtime_dir: paths.run.clone(),
        limits,
        storage_limit_bytes: WRITABLE_STORAGE_BYTES,
        cgroups: None,
        test_fixture_provider: false,
    };
    let (queue_tx, queue) = mpsc::sync_channel(1024);
    let (supervisor_tx, supervisor) = mpsc::sync_channel(1024);
    let store = Store::open(&paths.state.join("lade.db")).expect("store");
    let artifacts =
        ArtifactStore::open(&paths.state.join("artifacts"), &paths.tmp).expect("artifacts");
    let mut service = Service::new(
        &config,
        store,
        paths,
        Arc::new(artifacts),
        Arc::clone(launcher) as Arc<dyn WorkerLauncher>,
        supervisor_tx,
        queue_tx,
        Pools::default(),
    )
    .expect("load durable state");
    service.held = Some(Vec::new());
    (service, queue, supervisor)
}

/// Attaches a controlling client to `service` and returns the queue its frames land in.
fn attach(service: &mut Service) -> Outbound {
    let out = Outbound::new();
    service.controller = Some(Controller {
        conn: CONN,
        out: out.clone(),
        request_ids: std::collections::HashMap::new(),
        request_order: VecDeque::new(),
    });
    out
}
