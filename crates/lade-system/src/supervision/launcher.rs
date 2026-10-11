//! The real worker launcher: one bwrap sandbox, one gateway and one set of pipes per launch.
//!
//! `launch` checks the sandbox, resolves the provider's egress policy and credentials, starts
//! the per-launch gateway, spawns `prlimit … bwrap … node <entry>` with piped stdio, records a
//! pidfile for the startup sweep, and sends `worker.start`. Threads per launch: a writer (bounded
//! queue of service→worker frames), a monitor (reads and validates worker frames, observes the
//! exit, then stops the gateway and removes the pidfile) and a stderr forwarder (bounded,
//! rate-limited diagnostic lines).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use lade_protocol::worker::WorkerBody;
use lade_protocol::{FrameReader, Framing, TransportError, WorkerEnvelope, decode_worker, encode};

use super::credentials::{CredentialSource, ReleasedCredentials};
use super::gateway::{
    EgressPolicy, EgressRule, Gateway, UpstreamProxy, default_policies, ensure_private_dir,
};
use super::providers::{ProviderAccess, ProviderFacts, classify};
use super::sweep::{PidRecord, kill_record, record_sandbox, sandbox_marker};
use super::{
    LaunchBinding, LaunchError, LaunchRequest, SupervisorEvent, WorkerControl, WorkerEvent,
    WorkerLauncher,
};
use crate::adapters::profile::{self, CustomProvider};
pub use crate::adapters::sandbox::WORKER_LIMITS;
use crate::adapters::sandbox::{
    CgroupLeaf, Limits, SandboxCgroups, SandboxError, SandboxSpec, SandboxTools, WorkerLaunch,
    find_program, run_bounded, sandbox_available, worker_spec,
};

/// Node options for the worker. `--disable-wasm-trap-handler` makes V8 bounds-check
/// WebAssembly memory instead of reserving ~10 GiB guard regions per instance (Node's type
/// stripping and undici's HTTP parser are WebAssembly), which is what lets an address-space
/// limit work at all.
pub const NODE_ARGS: &[&str] = &["--disable-wasm-trap-handler"];

/// Queued service→worker frames per launch.
pub const WRITE_QUEUE_FRAMES: usize = 256;
/// Queued service→worker bytes per launch.
pub const WRITE_QUEUE_BYTES: usize = 8 << 20;
/// Grace period after `worker.shutdown` (or stdout EOF) before `SIGKILL`.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);
/// Forwarded stderr line bound and rate.
pub const STDERR_LINE_BYTES: usize = 512;
pub const STDERR_LINES_PER_SECOND: u32 = 100;
const KILL_WAIT: Duration = Duration::from_secs(10);
/// Wall-clock bound and largest accepted output of one model-catalogue run.
pub const CATALOG_TIMEOUT: Duration = Duration::from_secs(60);
pub const MAX_CATALOG_BYTES: u64 = 4 << 20;
const POLL: Duration = Duration::from_millis(10);
/// `sockaddr_un.sun_path` holds 108 bytes including the terminating NUL.
pub const MAX_SOCKET_PATH: usize = 107;
/// Subdirectory of the runtime directory holding gateway sockets (`$RUNTIME/g`).
pub const GATEWAY_SOCKET_DIR: &str = "g";
/// Largest execution generation the startup path-length check reserves room for.
pub const RESERVED_GENERATION: u64 = 9_999_999;

/// Gateway socket for one launch: `<dir>/<12 hex of the objective id>-<generation>.sock`. The name
/// is short so the path stays inside `sun_path` for any reasonable runtime directory, independent
/// of how long the state directory is.
pub fn gateway_socket_path(dir: &Path, objective_id: &str, generation: u64) -> PathBuf {
    let short: String = objective_id
        .chars()
        .filter(char::is_ascii_hexdigit)
        .take(12)
        .collect();
    dir.join(format!("{short}-{generation}.sock"))
}

/// Whether `path` can be bound as a Unix socket (`sun_path` limit).
pub fn socket_path_fits(path: &Path) -> bool {
    path.as_os_str().len() <= MAX_SOCKET_PATH
}

/// Everything the launcher needs besides the per-launch request.
#[derive(Clone, Debug)]
pub struct LauncherConfig {
    /// Node executable (canonicalized; its installation prefix is bound read-only).
    pub node: PathBuf,
    /// LADE worker package (bound read-only, includes its `node_modules`).
    pub worker_dir: PathBuf,
    /// Worker entry script; `<worker_dir>/src/worker.ts` by default. Tests may point it at
    /// another script inside `worker_dir`.
    pub entry: PathBuf,
    /// Node options before the entry; [`NODE_ARGS`] by default.
    pub node_args: Vec<OsString>,
    /// Private (0700) directory for pidfiles (`$STATE/run`).
    pub run_dir: PathBuf,
    /// Private (0700) directory for gateway sockets (`$RUNTIME/g`); kept separate from
    /// `run_dir` so socket paths stay short whatever the state directory is.
    pub socket_dir: PathBuf,
    pub limits: Limits,
    /// Egress policy per provider id; a provider without an entry cannot launch.
    pub policies: BTreeMap<String, EgressPolicy>,
    pub credentials: CredentialSource,
    /// Upstream proxy that gateway tunnels are chained through (`HTTPS_PROXY`).
    pub upstream_proxy: Option<UpstreamProxy>,
    /// Further read-only binds inside the sandbox.
    pub extra_ro: Vec<PathBuf>,
    /// Delegated cgroup subtree: each sandbox then runs in a leaf of its Objective's limited
    /// cgroup. `None` (no delegation): the `prlimit` limits only.
    pub cgroups: Option<Arc<SandboxCgroups>>,
}

impl LauncherConfig {
    /// Defaults: `node` from `PATH`, the worker package from `LADE_WORKER_DIR` or this
    /// repository's `packages/pi-worker`, [`WORKER_LIMITS`], the default provider policies,
    /// credentials from the process environment and credentials file, and `HTTPS_PROXY`.
    pub fn new(run_dir: PathBuf, socket_dir: PathBuf) -> Result<Self, LaunchError> {
        let worker_dir = default_worker_dir()?;
        Ok(Self {
            socket_dir,
            node: resolve_node(None)?,
            entry: worker_dir.join("src").join("worker.ts"),
            node_args: NODE_ARGS.iter().map(OsString::from).collect(),
            worker_dir,
            run_dir,
            limits: WORKER_LIMITS,
            policies: default_policies(),
            credentials: CredentialSource::from_process(),
            upstream_proxy: UpstreamProxy::from_env(),
            extra_ro: Vec::new(),
            cgroups: None,
        })
    }
}

/// The real path of `explicit`, else of `LADE_NODE` (an absolute path), else of `node` found on
/// `PATH`.
pub fn resolve_node(explicit: Option<&Path>) -> Result<PathBuf, LaunchError> {
    let from_env = std::env::var_os("LADE_NODE").map(PathBuf::from);
    if let Some(path) = &from_env
        && !path.is_absolute()
    {
        return Err(LaunchError::SandboxUnavailable(
            "LADE_NODE must be an absolute path to the node runtime".into(),
        ));
    }
    let candidate = match explicit.map(Path::to_path_buf).or(from_env) {
        Some(path) => path,
        None => find_program("node").ok_or_else(|| {
            LaunchError::SandboxUnavailable(
                "node runtime not found (set LADE_NODE or put node on PATH)".into(),
            )
        })?,
    };
    candidate
        .canonicalize()
        .map_err(|_| LaunchError::SandboxUnavailable("node runtime path unresolvable".into()))
}

/// The worker package, canonical. Resolution order: `LADE_WORKER_DIR`; the bundle layout
/// `<dir of the running executable>/../lib/lade/pi-worker` (see `scripts/bundle`); and only in
/// debug builds (development and tests) this repository's `packages/pi-worker`. Release builds
/// never fall back to the compiled-in source tree.
pub fn default_worker_dir() -> Result<PathBuf, LaunchError> {
    let exe = std::env::current_exe().ok();
    worker_dir_from(
        std::env::var_os("LADE_WORKER_DIR").map(PathBuf::from),
        exe.as_deref(),
    )
}

/// [`default_worker_dir`] with explicit inputs (testable without changing the environment).
pub fn worker_dir_from(
    env_dir: Option<PathBuf>,
    current_exe: Option<&Path>,
) -> Result<PathBuf, LaunchError> {
    if let Some(dir) = env_dir {
        return dir.canonicalize().map_err(|_| {
            LaunchError::SandboxUnavailable(format!(
                "LADE_WORKER_DIR {} does not exist",
                dir.display()
            ))
        });
    }
    let bundled = current_exe
        .and_then(|exe| exe.canonicalize().ok())
        .and_then(|exe| exe.parent().map(|bin| bin.join("../lib/lade/pi-worker")));
    if let Some(dir) = bundled.and_then(|d| d.canonicalize().ok())
        && dir.join("src").join("worker.ts").is_file()
    {
        return Ok(dir);
    }
    if cfg!(debug_assertions)
        && let Ok(dir) = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../packages/pi-worker")
            .canonicalize()
    {
        return Ok(dir);
    }
    Err(LaunchError::SandboxUnavailable(
        "worker package not found: set LADE_WORKER_DIR or install the bundle (lib/lade/pi-worker next to bin/)"
            .into(),
    ))
}

/// Locate the sandbox helpers and return the launcher (the sandbox itself is probed on every
/// launch).
pub fn default_launcher(config: LauncherConfig) -> Result<Box<dyn WorkerLauncher>, LaunchError> {
    Ok(Box::new(SandboxLauncher::new(config)?))
}

/// [`WorkerLauncher`] running every worker in a bwrap sandbox. There is no unsandboxed path.
#[derive(Debug)]
pub struct SandboxLauncher {
    config: LauncherConfig,
    tools: SandboxTools,
    /// A sandbox probe succeeded once; [`WorkerLauncher::preflight`] then skips re-probing
    /// (every launch still probes).
    sandbox_ok: AtomicBool,
}

impl SandboxLauncher {
    pub fn new(mut config: LauncherConfig) -> Result<Self, LaunchError> {
        let tools =
            SandboxTools::locate().map_err(|e| LaunchError::SandboxUnavailable(e.to_string()))?;
        config.node = resolve_node(Some(&config.node))?;
        config.worker_dir = config.worker_dir.canonicalize().map_err(LaunchError::Io)?;
        if !config.entry.is_absolute() || !config.entry.starts_with(&config.worker_dir) {
            return Err(invalid("worker entry must be inside the worker directory"));
        }
        if !config.run_dir.is_absolute() || !config.socket_dir.is_absolute() {
            return Err(invalid("run and socket directories must be absolute"));
        }
        ensure_private_dir(&config.run_dir).map_err(LaunchError::Io)?;
        ensure_private_dir(&config.socket_dir).map_err(LaunchError::Io)?;
        Ok(Self {
            config,
            tools,
            sandbox_ok: AtomicBool::new(false),
        })
    }

    pub fn config(&self) -> &LauncherConfig {
        &self.config
    }
}

fn invalid(what: &'static str) -> LaunchError {
    LaunchError::Io(io::Error::new(ErrorKind::InvalidInput, what))
}

fn safe_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

impl SandboxLauncher {
    /// The LADE profile next to the run directory (`$STATE/profile`).
    fn profile_dir(&self) -> Option<PathBuf> {
        self.config
            .run_dir
            .parent()
            .map(|state| state.join("profile"))
    }

    /// Custom providers of the LADE profile's `models.json` (none when it is absent or invalid).
    fn custom_providers(&self) -> Vec<CustomProvider> {
        self.profile_dir()
            .and_then(|dir| profile::profile_providers(&dir).ok())
            .unwrap_or_default()
    }

    /// The reviewed egress policy and the released credentials for `provider`: a built-in policy,
    /// or the exact endpoint of a custom provider defined in the LADE profile.
    fn resolve_provider(
        &self,
        provider: &str,
    ) -> Result<(EgressPolicy, ReleasedCredentials), LaunchError> {
        let unavailable = |e: String| LaunchError::ProviderUnavailable(e);
        if let Some(policy) = self.config.policies.get(provider) {
            let released = self
                .config
                .credentials
                .resolve(provider)
                .map_err(|e| unavailable(e.to_string()))?;
            return Ok((policy.clone(), released));
        }
        let custom = self.custom_providers();
        let Some(found) = custom.iter().find(|p| p.id == provider) else {
            return Err(unavailable(format!(
                "no egress policy for provider {provider}"
            )));
        };
        let released = self
            .config
            .credentials
            .resolve_custom(provider, &found.vars)
            .map_err(|e| unavailable(e.to_string()))?;
        let rule = EgressRule {
            host: found.host.clone(),
            port: found.port,
            allow_private: found.local,
        };
        Ok((EgressPolicy::none().with_extra([rule]), released))
    }

    fn provider_ready(&self, provider: &str) -> Result<(), LaunchError> {
        self.resolve_provider(provider).map(drop)
    }
}

impl WorkerLauncher for SandboxLauncher {
    fn preflight(&self, provider: &str) -> Result<(), LaunchError> {
        if !self.sandbox_ok.load(Ordering::SeqCst) {
            sandbox_available(&self.tools)
                .map_err(|e| LaunchError::SandboxUnavailable(e.to_string()))?;
            self.sandbox_ok.store(true, Ordering::SeqCst);
        }
        self.provider_ready(provider)
    }

    fn provider_access(&self, facts: &ProviderFacts) -> ProviderAccess {
        classify(
            facts,
            &self.config.policies,
            &self.config.credentials,
            &self.custom_providers(),
        )
    }

    fn catalog(&self, scratch: &Path, models: Option<&Path>) -> Result<Vec<u8>, LaunchError> {
        let sandbox = |e: crate::adapters::sandbox::SandboxError| {
            LaunchError::SandboxUnavailable(e.to_string())
        };
        sandbox_available(&self.tools).map_err(sandbox)?;
        let config = &self.config;
        let node = config.node.canonicalize().map_err(LaunchError::Io)?;
        let prefix = node
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| invalid("node prefix"))?
            .to_path_buf();
        let output = scratch.join("catalog.json");
        let mut argv = vec![node.into_os_string()];
        argv.extend(config.node_args.iter().cloned());
        argv.push(
            config
                .worker_dir
                .join("src/worker/catalog.ts")
                .into_os_string(),
        );
        argv.push(output.clone().into_os_string());
        if let Some(models) = models {
            argv.push(models.as_os_str().to_os_string());
        }
        let mut spec = SandboxSpec::new(argv, scratch, config.limits).map_err(sandbox)?;
        spec.ro_bind(&prefix)
            .and_then(|s| s.ro_bind(&config.worker_dir))
            .and_then(|s| s.rw_bind(scratch))
            .and_then(|s| s.env("HOME", scratch))
            .and_then(|s| s.env("LANG", "C.UTF-8"))
            .and_then(|s| s.env("PI_CODING_AGENT_DIR", scratch))
            .map_err(sandbox)?;
        if let Some(models) = models {
            spec.ro_bind(models).map_err(sandbox)?;
        }
        spec.marker(&sandbox_marker(&config.run_dir))
            .map_err(sandbox)?;
        let command = spec.command(&self.tools).map_err(sandbox)?;
        let outcome = run_bounded(command, CATALOG_TIMEOUT, 16 << 10).map_err(LaunchError::Io)?;
        if outcome.exit_code != Some(0) || outcome.timed_out {
            return Err(LaunchError::Io(io::Error::other("model catalogue failed")));
        }
        read_bounded(&output)
    }

    fn launch(
        &self,
        request: LaunchRequest,
        events: SyncSender<SupervisorEvent>,
    ) -> Result<Box<dyn WorkerControl>, LaunchError> {
        let LaunchRequest {
            binding,
            worktree,
            repo_common_dir,
            session_dir,
            provider,
            start,
        } = request;
        match &start {
            WorkerBody::Start {
                session_dir: start_session,
                cwd,
                ..
            } if Path::new(start_session) == session_dir && Path::new(cwd) == worktree => {}
            _ => return Err(invalid("start body must be worker.start for this launch")),
        }
        if !safe_id(&binding.objective_id) {
            return Err(invalid("objective id"));
        }
        sandbox_available(&self.tools)
            .map_err(|e| LaunchError::SandboxUnavailable(e.to_string()))?;
        let config = &self.config;
        let (policy, credentials) = self.resolve_provider(&provider)?;

        ensure_private_dir(&config.run_dir).map_err(LaunchError::Io)?;
        let stem = format!("{}-{}", binding.objective_id, binding.execution_generation);
        let socket = gateway_socket_path(
            &config.socket_dir,
            &binding.objective_id,
            binding.execution_generation,
        );
        let pidfile = config.run_dir.join(format!("worker-{stem}.pid"));
        if !socket_path_fits(&socket) {
            return Err(invalid("gateway socket path too long"));
        }
        let gateway = Gateway::start(&socket, policy, config.upstream_proxy.clone())
            .map_err(LaunchError::Io)?;

        let session = session_dir.clone();
        let launch = WorkerLaunch {
            node: config.node.clone(),
            worker_dir: config.worker_dir.clone(),
            entry: config.entry.clone(),
            node_args: config.node_args.clone(),
            args: Vec::new(),
            worktree,
            repo_common_dir,
            session_dir,
            gateway_socket: socket,
            pi_env: vec![
                ("PI_CODING_AGENT_DIR".into(), session.join("agent").into()),
                (
                    "PI_CODING_AGENT_SESSION_DIR".into(),
                    session.join("sessions").into(),
                ),
                ("PI_OFFLINE".into(), "1".into()),
                ("PI_SKIP_VERSION_CHECK".into(), "1".into()),
                ("PI_TELEMETRY".into(), "0".into()),
            ],
            credentials: credentials
                .vars
                .iter()
                .map(|(k, v)| (k.clone(), v.expose().to_os_string()))
                .collect(),
            extra_ro: config.extra_ro.clone(),
            limits: config.limits,
        };
        let spec_error = |e: SandboxError| {
            LaunchError::Io(io::Error::new(ErrorKind::InvalidInput, e.to_string()))
        };
        let mut spec = worker_spec(&launch).map_err(spec_error)?;
        drop(launch);
        spec.marker(&sandbox_marker(&config.run_dir))
            .map_err(spec_error)?;
        let leaf = match &config.cgroups {
            Some(cgroups) => Some(
                cgroups
                    .leaf(
                        &binding.objective_id,
                        &format!("worker-{}", binding.execution_generation),
                    )
                    .map_err(LaunchError::Io)?,
            ),
            None => None,
        };
        let mut cmd = spec.command(&self.tools).map_err(spec_error)?;
        drop(spec);
        cmd.command_mut()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        // Spawning consumes the command, which holds the credential environment.
        let (mut child, mut gate) = cmd.spawn().map_err(LaunchError::Io)?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(invalid("missing worker pipes"));
        };
        let pid = child.id();
        let label = format!("{}/{}", binding.objective_id, binding.execution_generation);
        let shared = Arc::new(Shared {
            child: Mutex::new(child),
            status: Mutex::new(None),
            exited: Condvar::new(),
            init: Mutex::new(None),
            gateway: Mutex::new(Some(gateway)),
            leaf: Mutex::new(None),
            pidfile,
        });
        // The program starts only once the sandbox is recorded and placed.
        let recorded = record_sandbox(&shared.pidfile, pid, leaf.as_ref(), || {
            shared.poll_exit().is_some()
        })
        .and_then(|init| {
            *lock(&shared.init) = init;
            *lock(&shared.leaf) = leaf;
            gate.open()
        });
        if let Err(e) = recorded {
            shared.kill();
            shared.wait_exit(KILL_WAIT);
            shared.finish();
            return Err(LaunchError::Io(e));
        }

        let (tx, rx) = sync_channel::<Vec<u8>>(WRITE_QUEUE_FRAMES);
        let queued = Arc::new(AtomicUsize::new(0));
        let link_failed = Arc::new(AtomicBool::new(false));
        let threads = spawn_threads(
            Arc::clone(&shared),
            binding,
            label,
            events,
            Pipes {
                stdin,
                stdout,
                stderr,
                rx,
            },
            Arc::clone(&queued),
            Arc::clone(&link_failed),
        );
        let mut worker = SandboxWorker {
            shared,
            tx: Some(tx),
            queued,
            link_failed,
            terminated: false,
        };
        if let Err(e) = threads {
            worker.terminate();
            return Err(LaunchError::Io(e));
        }
        if let Err(e) = worker.send(start) {
            worker.terminate();
            return Err(LaunchError::Io(e));
        }
        Ok(Box::new(worker))
    }
}

/// Reads at most [`MAX_CATALOG_BYTES`] of a catalogue output file.
pub fn read_bounded(path: &Path) -> Result<Vec<u8>, LaunchError> {
    let meta = std::fs::symlink_metadata(path).map_err(LaunchError::Io)?;
    if !meta.is_file() || meta.len() > MAX_CATALOG_BYTES {
        return Err(invalid("model catalogue output"));
    }
    std::fs::read(path).map_err(LaunchError::Io)
}

/// State shared by the control handle and the per-launch threads.
struct Shared {
    child: Mutex<Child>,
    status: Mutex<Option<ExitStatus>>,
    exited: Condvar,
    /// The sandbox's PID-namespace init (child of bwrap), once discovered.
    init: Mutex<Option<PidRecord>>,
    gateway: Mutex<Option<Gateway>>,
    /// The sandbox's cgroup leaf under a delegated subtree (removed after the exit).
    leaf: Mutex<Option<CgroupLeaf>>,
    pidfile: PathBuf,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    fn poll_exit(&self) -> Option<ExitStatus> {
        if let Some(status) = *lock(&self.status) {
            return Some(status);
        }
        let status = lock(&self.child).try_wait().ok().flatten()?;
        *lock(&self.status) = Some(status);
        self.exited.notify_all();
        Some(status)
    }

    fn wait_exit(&self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.poll_exit() {
                return Some(status);
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let guard = lock(&self.status);
            let _ = self
                .exited
                .wait_timeout(guard, POLL.min(deadline - now))
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// `SIGKILL` the namespace init (ending every sandboxed process) and bwrap. The child lock
    /// is held, so the outer pid cannot have been reaped and reused meanwhile.
    fn kill(&self) {
        let mut child = lock(&self.child);
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        if let Some(init) = *lock(&self.init) {
            kill_record(&init);
        }
        let _ = child.kill();
    }

    /// Revoke network access, drop the pidfile and remove the cgroup leaf (killing anything left
    /// in it). Only after the process has exited.
    fn finish(&self) {
        if let Some(mut gateway) = lock(&self.gateway).take() {
            gateway.stop();
        }
        if let Some(leaf) = lock(&self.leaf).take()
            && leaf.oom_kills() > 0
        {
            eprintln!(
                "laded: sandbox memory limit reached ({} OOM kill(s)) cgroup={}",
                leaf.oom_kills(),
                leaf.path().display()
            );
        }
        let _ = std::fs::remove_file(&self.pidfile);
    }
}

struct Pipes {
    stdin: ChildStdin,
    stdout: ChildStdout,
    stderr: ChildStderr,
    rx: Receiver<Vec<u8>>,
}

fn spawn_threads(
    shared: Arc<Shared>,
    binding: LaunchBinding,
    label: String,
    events: SyncSender<SupervisorEvent>,
    pipes: Pipes,
    queued: Arc<AtomicUsize>,
    link_failed: Arc<AtomicBool>,
) -> io::Result<()> {
    let Pipes {
        mut stdin,
        stdout,
        stderr,
        rx,
    } = pipes;
    thread::Builder::new()
        .name("lade-worker-write".into())
        .spawn(move || {
            for frame in rx {
                let written = stdin.write_all(&frame).and_then(|()| stdin.flush());
                queued.fetch_sub(frame.len(), Ordering::SeqCst);
                if written.is_err() {
                    link_failed.store(true, Ordering::SeqCst);
                    break;
                }
            }
        })?;
    let stderr_label = label.clone();
    thread::Builder::new()
        .name("lade-worker-stderr".into())
        .spawn(move || forward_stderr(stderr, &stderr_label, &mut io::stderr()))?;
    thread::Builder::new()
        .name("lade-worker-monitor".into())
        .spawn(move || monitor(&shared, binding, stdout, &events))?;
    Ok(())
}

enum Ending {
    Eof,
    Violation(&'static str),
    ReceiverGone,
}

fn monitor(
    shared: &Shared,
    binding: LaunchBinding,
    stdout: ChildStdout,
    events: &SyncSender<SupervisorEvent>,
) {
    let mut reader = FrameReader::new(stdout, Framing::Worker);
    let event = |event| SupervisorEvent {
        binding: binding.clone(),
        event,
    };
    let ending = loop {
        match reader.read_frame() {
            Ok(Some(frame)) => match decode_worker(&frame) {
                Ok(envelope) if envelope.body.is_from_worker() => {
                    if events
                        .send(event(WorkerEvent::Body(envelope.body)))
                        .is_err()
                    {
                        break Ending::ReceiverGone;
                    }
                }
                Ok(_) => break Ending::Violation("service_body_from_worker"),
                Err(e) => break Ending::Violation(e.0),
            },
            Ok(None) => break Ending::Eof,
            Err(TransportError::Wire(e)) => break Ending::Violation(e.0),
            Err(TransportError::Io(_)) => break Ending::Eof,
        }
    };
    drop(reader);
    match ending {
        Ending::Violation(code) => {
            shared.kill();
            let _ = events.send(event(WorkerEvent::ProtocolViolation(code)));
        }
        Ending::ReceiverGone => shared.kill(),
        Ending::Eof => {
            if shared.wait_exit(SHUTDOWN_GRACE).is_none() {
                shared.kill();
            }
        }
    }
    let status = loop {
        if let Some(status) = shared.wait_exit(KILL_WAIT) {
            break status;
        }
        shared.kill();
    };
    shared.finish();
    let _ = events.send(event(WorkerEvent::Exited {
        code: status.code(),
        signal: status.signal(),
    }));
}

/// Forward worker stderr lines to `out`, prefixed, each cut to [`STDERR_LINE_BYTES`], control
/// characters replaced, at most [`STDERR_LINES_PER_SECOND`] lines per second (excess counted).
pub(crate) fn forward_stderr(mut from: impl Read, label: &str, out: &mut impl Write) {
    let mut line: Vec<u8> = Vec::with_capacity(STDERR_LINE_BYTES);
    let mut cut = false;
    let mut window = Instant::now();
    let mut emitted = 0u32;
    let mut dropped = 0u64;
    let mut emit = |line: &[u8], cut: bool| {
        if window.elapsed() >= Duration::from_secs(1) {
            if dropped > 0 {
                let _ = writeln!(
                    out,
                    "lade-worker[{label}]: ({dropped} stderr lines dropped)"
                );
            }
            window = Instant::now();
            emitted = 0;
            dropped = 0;
        }
        if emitted >= STDERR_LINES_PER_SECOND {
            dropped += 1;
            return;
        }
        emitted += 1;
        let text: String = String::from_utf8_lossy(line)
            .chars()
            .map(|c| if c.is_control() && c != '\t' { '?' } else { c })
            .collect();
        let _ = writeln!(
            out,
            "lade-worker[{label}]: {text}{}",
            if cut { " …" } else { "" }
        );
    };
    let mut buffer = [0u8; 4096];
    loop {
        let n = match from.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        for &byte in &buffer[..n] {
            if byte == b'\n' {
                emit(&line, cut);
                line.clear();
                cut = false;
            } else if line.len() < STDERR_LINE_BYTES {
                line.push(byte);
            } else {
                cut = true;
            }
        }
    }
    if !line.is_empty() {
        emit(&line, cut);
    }
}

/// Control handle for one launched sandbox. Dropping it terminates the sandbox.
pub struct SandboxWorker {
    shared: Arc<Shared>,
    tx: Option<SyncSender<Vec<u8>>>,
    queued: Arc<AtomicUsize>,
    link_failed: Arc<AtomicBool>,
    terminated: bool,
}

impl fmt::Debug for SandboxWorker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SandboxWorker")
            .field("terminated", &self.terminated)
            .finish_non_exhaustive()
    }
}

impl WorkerControl for SandboxWorker {
    fn send(&mut self, body: WorkerBody) -> io::Result<()> {
        if body.is_from_worker() {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "not a service-to-worker body",
            ));
        }
        let gone = || io::Error::new(ErrorKind::BrokenPipe, "worker_link_closed");
        let Some(tx) = self.tx.as_ref() else {
            return Err(gone());
        };
        if self.terminated
            || self.link_failed.load(Ordering::SeqCst)
            || self.shared.poll_exit().is_some()
        {
            return Err(gone());
        }
        let frame = encode(&WorkerEnvelope::new(body), Framing::Worker)
            .map_err(|e| io::Error::new(ErrorKind::InvalidData, e.0))?;
        let len = frame.len();
        let full = || io::Error::new(ErrorKind::WouldBlock, "worker_queue_full");
        if self.queued.fetch_add(len, Ordering::SeqCst) + len > WRITE_QUEUE_BYTES {
            self.queued.fetch_sub(len, Ordering::SeqCst);
            return Err(full());
        }
        match tx.try_send(frame) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.queued.fetch_sub(len, Ordering::SeqCst);
                Err(full())
            }
            Err(TrySendError::Disconnected(_)) => {
                self.queued.fetch_sub(len, Ordering::SeqCst);
                Err(gone())
            }
        }
    }

    fn terminate(&mut self) {
        if self.terminated {
            return;
        }
        if self.shared.poll_exit().is_none() {
            let _ = self.send(WorkerBody::Shutdown);
        }
        self.terminated = true;
        // Closing the queue closes stdin after the shutdown frame: the worker also exits on EOF.
        self.tx = None;
        if self.shared.wait_exit(SHUTDOWN_GRACE).is_none() {
            self.shared.kill();
        }
        if self.shared.wait_exit(KILL_WAIT).is_some() {
            self.shared.finish();
        }
    }
}

impl Drop for SandboxWorker {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_is_bounded_and_sanitized() {
        let mut input = Vec::new();
        input.extend_from_slice(b"hello\x1b[31m\n");
        input.extend(std::iter::repeat_n(b'a', 2000));
        input.push(b'\n');
        for i in 0..300 {
            input.extend_from_slice(format!("line {i}\n").as_bytes());
        }
        let mut out = Vec::new();
        forward_stderr(&input[..], "o/1", &mut out);
        let out = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "lade-worker[o/1]: hello?[31m");
        assert!(lines[1].ends_with(" …"));
        assert!(lines[1].len() < STDERR_LINE_BYTES + 40);
        assert_eq!(lines.len(), STDERR_LINES_PER_SECOND as usize);
    }

    #[test]
    fn ids_are_file_safe() {
        assert!(safe_id("0b8a4c1e-7a1f-4f7e-9d7a-0d6c8f2a9b11"));
        assert!(!safe_id("../x"));
        assert!(!safe_id(""));
    }
}
