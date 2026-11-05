use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{self, ErrorKind};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lade_protocol::worker::WorkerBody;

use super::child::SandboxedChild;
use super::credentials::{CredentialError, CredentialSource, FIXTURE_REFUSED, ReleasedCredentials};
use super::gateway::{EgressPolicy, EgressRule, Gateway, UpstreamProxy, default_policies};
use super::provider_access::{ProviderAccess, ProviderFacts, classify};
use super::safe_id;
use super::sweep::sandbox_marker;
use super::{LaunchError, LaunchRequest, SupervisorEvent, WorkerControl, WorkerLauncher};
use crate::adapters::profile::{self, CustomProvider};
use crate::adapters::providers::FIXTURE_PROVIDER;
use crate::adapters::sandbox::{
    FIXTURE_SWITCH, Limits, SandboxCgroups, SandboxSpec, SandboxTools, WORKER_LIMITS, WorkerLaunch,
    find_program, run_bounded, sandbox_available, worker_spec,
};
use crate::support::fs::ensure_private_dir;

mod worker;

pub use worker::SandboxWorker;
use worker::Shared;

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

#[must_use]
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
    /// The default launcher configuration: Node and the worker package found as
    /// [`resolve_node`] and [`worker_dir_from`] find them from the configured `node` and
    /// `worker_dir` (the composition root reads `LADE_NODE` and `LADE_WORKER_DIR`), built-in
    /// provider policies, and credentials and proxy settings from the process environment.
    ///
    /// # Errors
    ///
    /// Fails as [`resolve_node`] and [`worker_dir_from`] do.
    pub fn new(
        run_dir: PathBuf,
        socket_dir: PathBuf,
        node: Option<PathBuf>,
        worker_dir: Option<PathBuf>,
    ) -> Result<Self, LaunchError> {
        let worker_dir = worker_dir_from(worker_dir, std::env::current_exe().ok().as_deref())?;
        Ok(Self {
            socket_dir,
            node: resolve_node(node)?,
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

/// The canonical path of the Node runtime: `configured` (from `LADE_NODE` or the launcher
/// configuration), else `node` on `PATH`.
///
/// # Errors
///
/// Returns [`LaunchError::SandboxUnavailable`] when `configured` is relative, no runtime is
/// found, or its path cannot be resolved.
pub fn resolve_node(configured: Option<PathBuf>) -> Result<PathBuf, LaunchError> {
    if configured.as_ref().is_some_and(|path| !path.is_absolute()) {
        return Err(LaunchError::SandboxUnavailable(
            "LADE_NODE must be an absolute path to the node runtime".into(),
        ));
    }
    let candidate = match configured {
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

/// The worker package directory: `env_dir` when set, else the bundle next to
/// `current_exe`, else (in debug builds) the repository's `packages/pi-worker`.
///
/// # Errors
///
/// Returns [`LaunchError::SandboxUnavailable`] when `env_dir` does not exist or no worker package
/// is found.
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

/// The sandboxed launcher for `config`, boxed as the service takes it.
///
/// # Errors
///
/// Fails as [`SandboxLauncher::new`] does.
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
    /// A launcher that runs workers in sandboxes as `config` describes, with its paths
    /// canonicalized and its private directories created.
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError::SandboxUnavailable`] when `bwrap`, `prlimit` or Node is missing, or
    /// [`LaunchError::Io`] when the worker directory cannot be resolved, the entry is outside it, the
    /// run or socket directory is relative, or a private directory cannot be created.
    pub fn new(mut config: LauncherConfig) -> Result<Self, LaunchError> {
        let tools = SandboxTools::locate()?;
        config.node = resolve_node(Some(config.node))?;
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

/// Reads the model catalogue a worker wrote, refusing anything but a regular file of at
/// most the catalogue bound.
///
/// # Errors
///
/// Returns [`LaunchError::Io`] when the file cannot be read, is not a regular file, or is too
/// large.
pub fn read_bounded(path: &Path) -> Result<Vec<u8>, LaunchError> {
    let meta = std::fs::symlink_metadata(path).map_err(LaunchError::Io)?;
    if !meta.is_file() || meta.len() > MAX_CATALOG_BYTES {
        return Err(invalid("model catalogue output"));
    }
    std::fs::read(path).map_err(LaunchError::Io)
}
