//! The network side: runtime-directory discovery, capability check, handshake, optional service
//! spawn, and the driver thread that owns the link. Nothing here runs on the GTK main context;
//! results reach the store only through the [`UpdateSink`].

use std::fs::{self, File};
use std::io::{ErrorKind, Read};
use std::net::Shutdown;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lade_protocol::client::{ClientBody, ClientEnvelope, ErrorCode, Event, Rejection, SafeAction};
use lade_protocol::types::{Capability, Id};
use lade_protocol::{FrameReader, FrameWriter, Framing, TransportError};

use super::core::{Core, Step};
use crate::presentation::intents::Intent;
use crate::presentation::model::ConnectionState;
use crate::presentation::store::StoreUpdate;

pub const SOCKET_NAME: &str = "control.sock";
pub const CAPABILITY_NAME: &str = "control.cap";

/// Handshake and write timeout.
const IO_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a first connection (including a spawned service's start-up) is retried.
const START_WINDOW: Duration = Duration::from_secs(5);
/// How long reconnection after a lost link is retried before showing "Disconnected — Retry".
const RECONNECT_WINDOW: Duration = Duration::from_secs(60);
const TICK: Duration = Duration::from_secs(1);

/// Receives store updates on behalf of the window; `false` when the window is gone.
pub trait UpdateSink: Send + 'static {
    fn deliver(&self, update: StoreUpdate) -> bool;
}

impl UpdateSink for Sender<StoreUpdate> {
    fn deliver(&self, update: StoreUpdate) -> bool {
        self.send(update).is_ok()
    }
}

/// How to start `laded serve` when no service answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnService {
    pub program: PathBuf,
    pub state_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub runtime_dir: PathBuf,
    /// `None` with `--no-spawn` or when no `laded` sits next to this executable.
    pub spawn: Option<SpawnService>,
    /// Folder to open as a Workspace once connected (`--open PATH`).
    pub open: Option<PathBuf>,
}

/// Resolves the control runtime directory the way `laded` does: an explicit directory, then
/// `LADE_RUNTIME_DIR`, `$XDG_RUNTIME_DIR/lade`, and finally `$STATE/run` with
/// `$STATE = ${XDG_STATE_HOME:-$HOME/.local/state}/lade` (or the explicit `--state-dir`).
pub fn runtime_dir(
    explicit: Option<&Path>,
    state_dir: Option<&Path>,
    env: impl Fn(&str) -> Option<String>,
) -> Option<PathBuf> {
    let absolute = |name: &str| env(name).map(PathBuf::from).filter(|p| p.is_absolute());
    if let Some(dir) = explicit {
        return Some(dir.to_path_buf());
    }
    if let Some(dir) = absolute("LADE_RUNTIME_DIR") {
        return Some(dir);
    }
    if let Some(dir) = absolute("XDG_RUNTIME_DIR") {
        return Some(dir.join("lade"));
    }
    let state = state_dir.map(Path::to_path_buf).or_else(|| {
        absolute("XDG_STATE_HOME")
            .or_else(|| absolute("HOME").map(|h| h.join(".local/state")))
            .map(|d| d.join("lade"))
    })?;
    Some(state.join("run"))
}

/// `laded` next to the running executable, if present.
pub fn sibling_service() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join("laded");
    candidate.is_file().then_some(candidate)
}

// ---------------------------------------------------------------------------------------------
// Identifiers.

static FALLBACK: AtomicU64 = AtomicU64::new(0);

/// A fresh UUIDv4 from `/dev/urandom`. Ids are public correlation values, never secrets; if the
/// device is unreadable the id is derived from time, process id and a counter (still unique).
pub fn random_id() -> Id {
    let mut bytes = [0u8; 16];
    let read = File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes));
    if read.is_err() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let count = FALLBACK.fetch_add(1, Ordering::Relaxed);
        bytes[..8].copy_from_slice(&(nanos as u64).to_le_bytes());
        bytes[8..12].copy_from_slice(&std::process::id().to_le_bytes());
        bytes[12..].copy_from_slice(&(count as u32).to_le_bytes());
    }
    Id::from_random_bytes(bytes)
}

// ---------------------------------------------------------------------------------------------
// Connecting.

#[derive(Debug, PartialEq, Eq)]
pub enum ConnectError {
    /// No socket, refused, or no capability yet: the service is not running.
    NotRunning,
    /// Another client holds the single controlling lease.
    LeaseHeld,
    /// The service closed the link after `auth` without a welcome (capability mismatch).
    Refused,
    /// The runtime directory or capability file is not private to this user.
    Unsafe(String),
    Protocol(String),
}

fn own_uid() -> Option<u32> {
    fs::metadata("/proc/self").ok().map(|m| m.uid())
}

/// Reads `control.cap`, refusing unless the directory is 0700-private and the file is a regular
/// 0600 file, both owned by this user.
pub fn read_capability(dir: &Path) -> Result<Capability, ConnectError> {
    let uid =
        own_uid().ok_or_else(|| ConnectError::Unsafe("cannot determine the user id".into()))?;
    let unsafe_dir = || {
        ConnectError::Unsafe(format!(
            "the runtime directory {} is not private to you (expected 0700, owned by you)",
            dir.display()
        ))
    };
    let meta = fs::symlink_metadata(dir).map_err(|e| match e.kind() {
        ErrorKind::NotFound => ConnectError::NotRunning,
        _ => unsafe_dir(),
    })?;
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(unsafe_dir());
    }
    let path = dir.join(CAPABILITY_NAME);
    let meta = fs::symlink_metadata(&path).map_err(|e| match e.kind() {
        ErrorKind::NotFound => ConnectError::NotRunning,
        _ => ConnectError::Unsafe("the capability file cannot be inspected".into()),
    })?;
    if !meta.file_type().is_file() || meta.uid() != uid || meta.mode() & 0o777 != 0o600 {
        return Err(ConnectError::Unsafe(
            "the capability file is not a 0600 file owned by you; refusing to use it".into(),
        ));
    }
    let mut text = String::new();
    File::open(&path)
        .and_then(|f| f.take(130).read_to_string(&mut text))
        .map_err(|_| ConnectError::Unsafe("the capability file cannot be read".into()))?;
    Capability::parse(text.strip_suffix('\n').unwrap_or(&text))
        .map_err(|_| ConnectError::Unsafe("the capability file is malformed".into()))
}

/// An authenticated link.
pub struct Link {
    pub service_epoch: Id,
    pub reader: FrameReader<UnixStream>,
    pub writer: FrameWriter<UnixStream>,
    pub stream: UnixStream,
}

/// Connects and authenticates: `hello` ⇄ `hello`, `auth` → `welcome`.
pub fn connect(dir: &Path) -> Result<Link, ConnectError> {
    let socket = dir.join(SOCKET_NAME);
    match fs::symlink_metadata(&socket) {
        Ok(meta) if meta.file_type().is_socket() => {}
        Ok(_) => {
            return Err(ConnectError::Unsafe(
                "the control socket path is not a socket".into(),
            ));
        }
        Err(_) => return Err(ConnectError::NotRunning),
    }
    let stream = UnixStream::connect(&socket).map_err(|e| match e.kind() {
        ErrorKind::NotFound | ErrorKind::ConnectionRefused => ConnectError::NotRunning,
        _ => ConnectError::Protocol(format!("connect failed ({})", e.kind())),
    })?;
    let capability = read_capability(dir)?;
    let io =
        |e: std::io::Error| ConnectError::Protocol(format!("socket setup failed ({})", e.kind()));
    stream.set_read_timeout(Some(IO_TIMEOUT)).map_err(io)?;
    stream.set_write_timeout(Some(IO_TIMEOUT)).map_err(io)?;
    let mut reader = FrameReader::new(stream.try_clone().map_err(io)?, Framing::Client);
    let mut writer = FrameWriter::new(stream.try_clone().map_err(io)?, Framing::Client);
    let lost = |_| ConnectError::Protocol("the service closed the connection during hello".into());
    writer
        .write(&ClientEnvelope::new(ClientBody::Hello))
        .map_err(lost)?;
    match reader.read::<ClientEnvelope>() {
        Ok(Some(ClientEnvelope {
            body: ClientBody::Hello,
            ..
        })) => {}
        Err(TransportError::Wire(e)) if e.0 == "version_mismatch" => {
            return Err(ConnectError::Protocol(
                "the service speaks a different LADE version".into(),
            ));
        }
        _ => {
            return Err(ConnectError::Protocol(
                "the service did not answer hello".into(),
            ));
        }
    }
    writer
        .write(&ClientEnvelope::new(ClientBody::Auth { capability }))
        .map_err(|_| ConnectError::Refused)?;
    let welcome = match reader.read::<ClientEnvelope>() {
        Ok(Some(envelope)) => envelope.body,
        Ok(None) | Err(TransportError::Io(_)) => return Err(ConnectError::Refused),
        Err(TransportError::Wire(e)) => return Err(ConnectError::Protocol(e.to_string())),
    };
    let service_epoch = match welcome {
        ClientBody::Welcome { service_epoch, .. } => service_epoch,
        ClientBody::Event(frame) => {
            return Err(match frame.event {
                Event::ServiceNotice(n) if n.code == "unauthorized" => ConnectError::LeaseHeld,
                _ => ConnectError::Protocol("unexpected event before welcome".into()),
            });
        }
        _ => return Err(ConnectError::Protocol("unexpected reply to auth".into())),
    };
    stream.set_read_timeout(None).map_err(io)?;
    Ok(Link {
        service_epoch,
        reader,
        writer,
        stream,
    })
}

/// Starts `laded serve` detached in its own process group with null stdio.
pub fn spawn_service(spawn: &SpawnService, runtime_dir: &Path) -> std::io::Result<()> {
    let mut command = Command::new(&spawn.program);
    command.arg("serve").arg("--runtime-dir").arg(runtime_dir);
    if let Some(state) = &spawn.state_dir {
        command.arg("--state-dir").arg(state);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    // Reap it if it exits while this window runs; closing the window never stops the service.
    thread::Builder::new()
        .name("lade-reap".into())
        .spawn(move || {
            let _ = child.wait();
        })?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Driver.

/// Everything the driver thread reacts to.
pub enum Input {
    Intent(Intent),
    Frame(u64, ClientEnvelope),
    Closed(u64),
    Shutdown,
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    Connecting {
        started: Instant,
        attempt: u32,
        next: Instant,
        spawned: bool,
        after_loss: bool,
    },
    Connected,
    /// Waiting for an explicit Retry (unavailable, refused or another window holds the lease).
    Waiting,
}

impl Phase {
    fn connecting(after_loss: bool, delay: Duration) -> Self {
        let now = Instant::now();
        Self::Connecting {
            started: now,
            attempt: 0,
            next: now + delay,
            spawned: false,
            after_loss,
        }
    }
}

fn backoff(attempt: u32, after_loss: bool) -> Duration {
    let (base, cap) = if after_loss {
        (250, 5_000)
    } else {
        (100, 1_000)
    };
    Duration::from_millis((base << attempt.min(6)).min(cap))
}

struct ActiveLink {
    generation: u64,
    writer: FrameWriter<UnixStream>,
    stream: UnixStream,
    failed: bool,
}

struct Driver<S: UpdateSink> {
    config: Config,
    sink: S,
    tx: Sender<Input>,
    core: Core,
    link: Option<ActiveLink>,
    generation: u64,
    phase: Phase,
}

/// Runs the driver on a dedicated thread. `rx` receives intents from the main context.
pub fn spawn_driver<S: UpdateSink>(
    config: Config,
    sink: S,
    tx: Sender<Input>,
    rx: Receiver<Input>,
) -> std::io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("lade-transport".into())
        .spawn(move || {
            let core = Core::new(random_id, config.open.clone());
            let mut driver = Driver {
                config,
                sink,
                tx,
                core,
                link: None,
                generation: 0,
                phase: Phase::connecting(false, Duration::ZERO),
            };
            driver.run(&rx);
        })
}

impl<S: UpdateSink> Driver<S> {
    fn run(&mut self, rx: &Receiver<Input>) {
        let mut last_tick = Instant::now();
        loop {
            if !self.maybe_connect() {
                break;
            }
            let wait = match &self.phase {
                Phase::Connecting { next, .. } => next.saturating_duration_since(Instant::now()),
                _ => TICK,
            };
            let keep = match rx.recv_timeout(wait) {
                Ok(Input::Shutdown) | Err(RecvTimeoutError::Disconnected) => false,
                Ok(Input::Intent(Intent::RetryConnection)) => {
                    if !matches!(self.phase, Phase::Connected) {
                        self.phase = Phase::connecting(false, Duration::ZERO);
                    }
                    true
                }
                Ok(Input::Intent(intent)) => {
                    let step = self.core.submit(intent);
                    self.deliver(step)
                }
                Ok(Input::Frame(generation, envelope)) => self.on_frame(generation, envelope),
                Ok(Input::Closed(generation)) => self.on_closed(generation),
                Err(RecvTimeoutError::Timeout) => true,
            };
            if !keep {
                break;
            }
            if last_tick.elapsed() >= TICK {
                last_tick = Instant::now();
                let step = self.core.tick();
                if !self.deliver(step) {
                    break;
                }
            }
        }
        if let Some(link) = self.link.take() {
            let _ = link.stream.shutdown(Shutdown::Both);
        }
    }

    fn status(&self, state: ConnectionState) -> bool {
        self.sink.deliver(StoreUpdate::Connection(state))
    }

    /// Attempts a connection when one is due. Returns `false` when the window is gone.
    fn maybe_connect(&mut self) -> bool {
        let Phase::Connecting {
            started,
            attempt,
            next,
            spawned,
            after_loss,
        } = self.phase
        else {
            return true;
        };
        if Instant::now() < next {
            return true;
        }
        let attempt = attempt + 1;
        let state = if after_loss {
            ConnectionState::Reconnecting { attempt }
        } else {
            ConnectionState::Connecting
        };
        if (after_loss || attempt == 1) && !self.status(state) {
            return false;
        }
        let window = if after_loss {
            RECONNECT_WINDOW
        } else {
            START_WINDOW
        };
        let in_window = started.elapsed() < window;
        let mut spawned_now = spawned;
        let failure: Option<String> = match connect(&self.config.runtime_dir) {
            Ok(link) => return self.connected(link),
            Err(ConnectError::NotRunning) => {
                let mut why = "LADE could not reach the local service.".to_owned();
                match &self.config.spawn {
                    Some(spawn) if !spawned => {
                        spawned_now = true;
                        if spawn_service(spawn, &self.config.runtime_dir).is_err() {
                            why = "LADE could not start the local service (laded).".into();
                        }
                    }
                    Some(_) => {}
                    None => why.push_str(" Start it with “laded serve”, then retry."),
                }
                (!in_window).then_some(why)
            }
            // The capability may have rotated between reading and proving it: retry briefly.
            Err(ConnectError::Refused) if attempt < 3 => None,
            Err(ConnectError::Refused) => Some(
                "The service refused this window's credentials. If it was just restarted, retry."
                    .to_owned(),
            ),
            Err(ConnectError::LeaseHeld) => {
                self.phase = Phase::Waiting;
                return self.status(ConnectionState::LeaseHeld);
            }
            Err(ConnectError::Unsafe(why) | ConnectError::Protocol(why)) => Some(why),
        };
        match failure {
            None => {
                self.phase = Phase::Connecting {
                    started,
                    attempt,
                    next: Instant::now() + backoff(attempt, after_loss),
                    spawned: spawned_now,
                    after_loss,
                };
                true
            }
            Some(reason) => {
                self.phase = Phase::Waiting;
                self.status(if after_loss {
                    ConnectionState::Disconnected { reason }
                } else {
                    ConnectionState::Unavailable { reason }
                })
            }
        }
    }

    fn connected(&mut self, link: Link) -> bool {
        self.generation += 1;
        let generation = self.generation;
        let Link {
            service_epoch,
            mut reader,
            writer,
            stream,
        } = link;
        let tx = self.tx.clone();
        let spawned = thread::Builder::new()
            .name("lade-transport-read".into())
            .spawn(move || {
                while let Ok(Some(envelope)) = reader.read::<ClientEnvelope>() {
                    if tx.send(Input::Frame(generation, envelope)).is_err() {
                        return;
                    }
                }
                let _ = tx.send(Input::Closed(generation));
            });
        if spawned.is_err() {
            let _ = stream.shutdown(Shutdown::Both);
            self.phase = Phase::Waiting;
            return self.status(ConnectionState::Unavailable {
                reason: "could not start the connection reader".into(),
            });
        }
        self.link = Some(ActiveLink {
            generation,
            writer,
            stream,
            failed: false,
        });
        self.phase = Phase::Connected;
        let step = self.core.on_welcome(service_epoch);
        self.deliver(step)
    }

    fn on_frame(&mut self, generation: u64, envelope: ClientEnvelope) -> bool {
        if self
            .link
            .as_ref()
            .is_none_or(|l| l.generation != generation)
        {
            return true;
        }
        let step = match envelope.body {
            ClientBody::Reply(reply) => self.core.on_reply(reply),
            ClientBody::Event(event) => self.core.on_event(event),
            // Anything else from the service after welcome is a protocol violation.
            _ => {
                self.fail_link();
                return true;
            }
        };
        self.deliver(step)
    }

    fn fail_link(&mut self) {
        if let Some(link) = self.link.as_mut() {
            link.failed = true;
            let _ = link.stream.shutdown(Shutdown::Both);
        }
    }

    fn on_closed(&mut self, generation: u64) -> bool {
        if self
            .link
            .as_ref()
            .is_none_or(|l| l.generation != generation)
        {
            return true;
        }
        if let Some(link) = self.link.take() {
            let _ = link.stream.shutdown(Shutdown::Both);
        }
        self.core.on_link_lost();
        self.phase = Phase::connecting(true, backoff(0, true));
        self.status(ConnectionState::Reconnecting { attempt: 0 })
    }

    /// Applies updates and writes frames. Returns `false` when the window is gone.
    fn deliver(&mut self, step: Step) -> bool {
        let mut followups = Step::default();
        for frame in step.frames {
            let Some(link) = self.link.as_mut().filter(|l| !l.failed) else {
                continue; // stays pending; settled by on_link_lost
            };
            let request_id = frame.request_id.clone();
            match link
                .writer
                .write(&ClientEnvelope::new(ClientBody::Request(frame)))
            {
                Ok(()) => {}
                Err(TransportError::Io(_)) => self.fail_link(),
                Err(TransportError::Wire(e)) => {
                    // Encoding refused (e.g. over the frame cap): nothing left this process.
                    let rejection = Rejection::new(
                        ErrorCode::LimitReached,
                        SafeAction::None,
                        &format!("the request could not be encoded ({e})"),
                    );
                    let s = self.core.unsendable(&request_id, &rejection);
                    followups.updates.extend(s.updates);
                }
            }
        }
        step.updates
            .into_iter()
            .chain(followups.updates)
            .all(|update| self.sink.deliver(update))
    }
}
