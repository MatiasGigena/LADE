use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{ChildStderr, ChildStdin, ChildStdout, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use lade_protocol::worker::WorkerBody;
use lade_protocol::{FrameReader, Framing, TransportError, WorkerEnvelope, decode_worker, encode};

use super::child::{ChildStdio, SandboxedChild, lock};
use super::credentials::{
    CredentialError, CredentialSource, FIXTURE_PROVIDER, FIXTURE_REFUSED, FIXTURE_SWITCH,
    ReleasedCredentials,
};
use super::gateway::{EgressPolicy, EgressRule, Gateway, UpstreamProxy, default_policies};
use super::providers::{ProviderAccess, ProviderFacts, classify};
use super::sweep::sandbox_marker;
use super::{
    LaunchBinding, LaunchError, LaunchRequest, SupervisorEvent, WorkerControl, WorkerEvent,
    WorkerLauncher,
};
use crate::adapters::profile::{self, CustomProvider};
pub use crate::adapters::sandbox::WORKER_LIMITS;
use crate::adapters::sandbox::{
    Limits, SandboxCgroups, SandboxSpec, SandboxTools, WorkerLaunch, find_program, run_bounded,
    sandbox_available, worker_spec,
};
use crate::support::fs::ensure_private_dir;

pub const NODE_ARGS: &[&str] = &["--disable-wasm-trap-handler"];
/// The pi switches every worker spawn (session worker and model catalogue) runs with.
pub const PI_FLAGS: [(&str, &str); 3] = [
    ("PI_OFFLINE", "1"),
    ("PI_SKIP_VERSION_CHECK", "1"),
    ("PI_TELEMETRY", "0"),
];

pub const WRITE_QUEUE_FRAMES: usize = 256;
pub const WRITE_QUEUE_BYTES: usize = 8 << 20;
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);
pub const STDERR_LINE_BYTES: usize = 512;
pub const STDERR_LINES_PER_SECOND: u32 = 100;
pub const CATALOG_TIMEOUT: Duration = Duration::from_secs(60);
pub const MAX_CATALOG_BYTES: u64 = 4 << 20;
pub const MAX_SOCKET_PATH: usize = 107;
pub const GATEWAY_SOCKET_DIR: &str = "g";
pub const RESERVED_GENERATION: u64 = 9_999_999;

pub fn gateway_socket_path(dir: &Path, objective_id: &str, generation: u64) -> PathBuf {
    let short: String = objective_id
        .chars()
        .filter(char::is_ascii_hexdigit)
        .take(12)
        .collect();
    dir.join(format!("{short}-{generation}.sock"))
}

pub fn socket_path_fits(path: &Path) -> bool {
    path.as_os_str().len() <= MAX_SOCKET_PATH
}

#[derive(Clone, Debug)]
pub struct LauncherConfig {
    pub node: PathBuf,
    pub worker_dir: PathBuf,
    pub entry: PathBuf,
    pub node_args: Vec<OsString>,
    pub run_dir: PathBuf,
    pub socket_dir: PathBuf,
    pub limits: Limits,
    pub policies: BTreeMap<String, EgressPolicy>,
    pub credentials: CredentialSource,
    pub upstream_proxy: Option<UpstreamProxy>,
    pub extra_ro: Vec<PathBuf>,
    pub cgroups: Option<Arc<SandboxCgroups>>,
    pub test_fixture_provider: bool,
}

impl LauncherConfig {
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
            test_fixture_provider: false,
        })
    }
}

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

pub fn default_worker_dir() -> Result<PathBuf, LaunchError> {
    let exe = std::env::current_exe().ok();
    worker_dir_from(
        std::env::var_os("LADE_WORKER_DIR").map(PathBuf::from),
        exe.as_deref(),
    )
}

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

pub fn default_launcher(config: LauncherConfig) -> Result<Box<dyn WorkerLauncher>, LaunchError> {
    Ok(Box::new(SandboxLauncher::new(config)?))
}

#[derive(Debug)]
pub struct SandboxLauncher {
    config: LauncherConfig,
    tools: SandboxTools,
    sandbox_ok: AtomicBool,
}

impl SandboxLauncher {
    pub fn new(mut config: LauncherConfig) -> Result<Self, LaunchError> {
        let tools = SandboxTools::locate()?;
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
    fn worker_launch(
        &self,
        worktree: PathBuf,
        repo_common_dir: PathBuf,
        session_dir: PathBuf,
        gateway_socket: PathBuf,
        credentials: &ReleasedCredentials,
    ) -> WorkerLaunch {
        let config = &self.config;
        let pi_env = [
            (
                "PI_CODING_AGENT_DIR".into(),
                session_dir.join("agent").into(),
            ),
            (
                "PI_CODING_AGENT_SESSION_DIR".into(),
                session_dir.join("sessions").into(),
            ),
        ]
        .into_iter()
        .chain(PI_FLAGS.map(|(name, value)| (name.into(), value.into())))
        .collect();
        WorkerLaunch {
            node: config.node.clone(),
            worker_dir: config.worker_dir.clone(),
            entry: config.entry.clone(),
            node_args: config.node_args.clone(),
            args: Vec::new(),
            worktree,
            repo_common_dir,
            session_dir,
            gateway_socket,
            pi_env,
            credentials: credentials
                .vars
                .iter()
                .map(|(k, v)| (k.clone(), v.expose().to_os_string()))
                .collect(),
            extra_ro: config.extra_ro.clone(),
            limits: config.limits,
            test_fixture_provider: config.test_fixture_provider,
        }
    }

    fn profile_dir(&self) -> Option<PathBuf> {
        self.config
            .run_dir
            .parent()
            .map(|state| state.join("profile"))
    }

    fn custom_providers(&self) -> Vec<CustomProvider> {
        self.profile_dir()
            .and_then(|dir| profile::profile_providers(&dir).ok())
            .unwrap_or_default()
    }

    fn resolve_provider(
        &self,
        provider: &str,
        release: bool,
    ) -> Result<(EgressPolicy, ReleasedCredentials), LaunchError> {
        let unavailable = |e: CredentialError| LaunchError::ProviderUnavailable(e.to_string());
        if provider == FIXTURE_PROVIDER && !self.config.test_fixture_provider {
            return Err(LaunchError::ProviderUnavailable(FIXTURE_REFUSED.into()));
        }
        let credentials = &self.config.credentials;
        if let Some(policy) = self.config.policies.get(provider) {
            let released = if release {
                credentials.resolve(provider).map_err(unavailable)?
            } else {
                credentials.check(provider).map_err(unavailable)?;
                ReleasedCredentials::default()
            };
            return Ok((policy.clone(), released));
        }
        let custom = self.custom_providers();
        let Some(found) = custom.iter().find(|p| p.id == provider) else {
            return Err(LaunchError::ProviderUnavailable(format!(
                "no egress policy for provider {provider}"
            )));
        };
        let released = if release {
            credentials
                .resolve_custom(provider, &found.vars)
                .map_err(unavailable)?
        } else {
            credentials
                .check_custom(provider, &found.vars)
                .map_err(unavailable)?;
            ReleasedCredentials::default()
        };
        let rule = EgressRule {
            host: found.host.clone(),
            port: found.port,
            allow_private: found.local,
        };
        Ok((EgressPolicy::none().with_extra([rule]), released))
    }

    fn provider_ready(&self, provider: &str) -> Result<(), LaunchError> {
        self.resolve_provider(provider, false).map(drop)
    }
}

impl WorkerLauncher for SandboxLauncher {
    fn preflight(&self, provider: &str) -> Result<(), LaunchError> {
        if !self.sandbox_ok.load(Ordering::SeqCst) {
            sandbox_available(&self.tools)?;
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
        sandbox_available(&self.tools)?;
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
        let mut spec = SandboxSpec::new(argv, scratch, config.limits)?;
        spec.ro_bind(&prefix)?
            .ro_bind(&config.worker_dir)?
            .rw_bind(scratch)?
            .env("HOME", scratch)?
            .env("LANG", "C.UTF-8")?
            .env("PI_CODING_AGENT_DIR", scratch)?;
        for (name, value) in PI_FLAGS {
            spec.env(name, value)?;
        }
        if let Some(models) = models {
            spec.ro_bind(models)?;
        }
        if config.test_fixture_provider {
            spec.env(FIXTURE_SWITCH, "1")?;
        }
        spec.marker(&sandbox_marker(&config.run_dir))?;
        let command = spec.command(&self.tools)?;
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
        sandbox_available(&self.tools)?;
        let config = &self.config;
        let (policy, credentials) = self.resolve_provider(&provider, true)?;

        ensure_private_dir(&config.run_dir).map_err(LaunchError::Io)?;
        let generation = binding.execution_generation;
        let socket = gateway_socket_path(&config.socket_dir, &binding.objective_id, generation);
        if !socket_path_fits(&socket) {
            return Err(invalid("gateway socket path too long"));
        }
        let gateway = Gateway::start(&socket, policy, config.upstream_proxy.clone())
            .map_err(LaunchError::Io)?;
        let mut spec = worker_spec(&self.worker_launch(
            worktree,
            repo_common_dir,
            session_dir,
            socket,
            &credentials,
        ))?;
        drop(credentials);
        spec.marker(&sandbox_marker(&config.run_dir))?;
        let leaf = match &config.cgroups {
            Some(cgroups) => Some(
                cgroups
                    .leaf(&binding.objective_id, &format!("worker-{generation}"))
                    .map_err(LaunchError::Io)?,
            ),
            None => None,
        };
        let mut command = spec.command(&self.tools)?;
        drop(spec);
        command
            .command_mut()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let pidfile = config
            .run_dir
            .join(format!("worker-{}-{generation}.pid", binding.objective_id));
        let (child, stdio) =
            SandboxedChild::spawn(command, pidfile, leaf).map_err(LaunchError::Io)?;
        let shared = Arc::new(Shared {
            child,
            gateway: Mutex::new(Some(gateway)),
        });
        let mut worker = SandboxWorker::start(shared, stdio, binding, events)?;
        worker.send(start).map_err(LaunchError::Io)?;
        Ok(Box::new(worker))
    }
}

pub fn read_bounded(path: &Path) -> Result<Vec<u8>, LaunchError> {
    let meta = std::fs::symlink_metadata(path).map_err(LaunchError::Io)?;
    if !meta.is_file() || meta.len() > MAX_CATALOG_BYTES {
        return Err(invalid("model catalogue output"));
    }
    std::fs::read(path).map_err(LaunchError::Io)
}

struct Shared {
    child: SandboxedChild,
    gateway: Mutex<Option<Gateway>>,
}

impl Shared {
    /// Tears the sandbox down (see [`SandboxedChild::terminate`]) and stops its gateway.
    fn finish(&self) -> Option<ExitStatus> {
        let status = self.child.terminate();
        if let Some(mut gateway) = lock(&self.gateway).take() {
            gateway.stop();
        }
        status
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
            shared.child.kill();
            let _ = events.send(event(WorkerEvent::ProtocolViolation(code)));
        }
        Ending::ReceiverGone => shared.child.kill(),
        Ending::Eof => drop(shared.child.wait_exit(SHUTDOWN_GRACE)),
    }
    let status = shared.finish();
    let _ = events.send(event(WorkerEvent::Exited {
        code: status.and_then(|s| s.code()),
        signal: status.and_then(|s| s.signal()),
    }));
}

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

pub struct SandboxWorker {
    shared: Arc<Shared>,
    tx: Option<SyncSender<Vec<u8>>>,
    queued: Arc<AtomicUsize>,
    link_failed: Arc<AtomicBool>,
    terminated: bool,
}

impl SandboxWorker {
    /// Starts the writer, stderr and monitor threads of a spawned worker sandbox. On failure the
    /// worker is dropped, which terminates the sandbox.
    fn start(
        shared: Arc<Shared>,
        stdio: ChildStdio,
        binding: LaunchBinding,
        events: SyncSender<SupervisorEvent>,
    ) -> Result<Self, LaunchError> {
        let (tx, rx) = sync_channel::<Vec<u8>>(WRITE_QUEUE_FRAMES);
        let worker = Self {
            shared: Arc::clone(&shared),
            tx: Some(tx),
            queued: Arc::new(AtomicUsize::new(0)),
            link_failed: Arc::new(AtomicBool::new(false)),
            terminated: false,
        };
        let ChildStdio {
            stdin: Some(stdin),
            stdout: Some(stdout),
            stderr: Some(stderr),
        } = stdio
        else {
            return Err(invalid("missing worker pipes"));
        };
        let label = format!("{}/{}", binding.objective_id, binding.execution_generation);
        spawn_threads(
            shared,
            binding,
            label,
            events,
            Pipes {
                stdin,
                stdout,
                stderr,
                rx,
            },
            Arc::clone(&worker.queued),
            Arc::clone(&worker.link_failed),
        )
        .map_err(LaunchError::Io)?;
        Ok(worker)
    }
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
            || self.shared.child.poll_exit().is_some()
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
        if self.shared.child.poll_exit().is_none() {
            let _ = self.send(WorkerBody::Shutdown);
        }
        self.terminated = true;
        self.tx = None;
        self.shared.child.wait_exit(SHUTDOWN_GRACE);
        self.shared.finish();
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
