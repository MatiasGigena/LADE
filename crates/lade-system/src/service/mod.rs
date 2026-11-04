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
mod link;
mod models;
mod objectives;
mod project;
mod recovery;
mod resources;
mod review;
mod startup;
mod state;
mod terminal;
#[cfg(test)]
mod test_harness;

/// The repository fixtures shared with the adapter integration tests, for unit tests here.
#[cfg(test)]
#[path = "../../tests/adapters_support/mod.rs"]
mod adapters_support;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use lade_core::domain::ids::{CandidateId, ObjectiveId, Uuid, VerificationId, WorkspaceId};
use lade_core::domain::review::IntegrationPlan;
use lade_protocol::client::{ErrorCode, Outcome, Rejection, ReplyData, RequestName, Settings};
use lade_protocol::types::Id;
use lade_protocol::worker::WorkerResources;
use rusqlite::Transaction;

use crate::adapters::artifacts::ArtifactStore;
use crate::adapters::git::{GitError, SquashOutcome};
use crate::adapters::profile::ImportReport;
use crate::adapters::sandbox::{STORAGE_CHECK_INTERVAL, SandboxCgroups, SandboxTools};
use crate::storage::{self, OperationKey, OperationState, Store};
use crate::supervision::{SupervisorEvent, Terminal, WorkerLauncher};
use crate::transport::Inbound;

pub use limits::ResourceLimits;
pub use startup::{RunningService, ServiceConfig, StartError, start};

use crate::support::{new_uuid, now_ms};
use convert::{durability, wire_id};
use link::Controller;
use state::{ObjState, Runtime, WsState};

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

/// How often the ticker wakes the service thread to flush deltas and check timeouts.
pub(crate) const TICK: Duration = Duration::from_millis(100);
/// Ticks between storage-usage checks.
const STORAGE_CHECK_TICKS: u128 = STORAGE_CHECK_INTERVAL.as_millis() / TICK.as_millis();

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
