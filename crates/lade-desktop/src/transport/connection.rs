//! The service connection: finding and admitting to the control socket, starting `laded` when
//! allowed, and the driver thread that reconnects and feeds the protocol core.

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

use lade_protocol::client::{
    ClientBody, ClientEnvelope, ErrorCode, Event, NoticeCode, Rejection, SafeAction,
};
use lade_protocol::types::{CONTROL_CAPABILITY_NAME, CONTROL_SOCKET_NAME, Capability, Id};
use lade_protocol::{FrameReader, FrameWriter, Framing, TransportError, WireError};

use super::core::{Core, Step};
use crate::presentation::controller::intents::Intent;
use crate::presentation::state::model::ConnectionState;
use crate::presentation::state::store::StoreUpdate;

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const START_WINDOW: Duration = Duration::from_secs(5);
const RECONNECT_WINDOW: Duration = Duration::from_secs(60);
const TICK: Duration = Duration::from_secs(1);

pub trait UpdateSink: Send + 'static {
    fn deliver(&self, update: StoreUpdate) -> bool;
}

impl UpdateSink for Sender<StoreUpdate> {
    fn deliver(&self, update: StoreUpdate) -> bool {
        self.send(update).is_ok()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnService {
    pub program: PathBuf,
    pub state_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub runtime_dir: PathBuf,
    pub spawn: Option<SpawnService>,
    pub open: Option<PathBuf>,
}

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

pub fn sibling_service() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join("laded");
    candidate.is_file().then_some(candidate)
}

static FALLBACK: AtomicU64 = AtomicU64::new(0);

/// A fresh id for a request or operation, or for a test's scratch name. Those ids only
/// correlate replies and recognise resends, so they must be unique but need not be secret:
/// when `/dev/urandom` cannot be read, a mix of the time, process id and a counter stands in.
/// That fallback is unique within the process but not random.
pub fn unique_id() -> Id {
    let mut bytes = [0u8; 16];
    let read = File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes));
    if read.is_err() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let count = FALLBACK.fetch_add(1, Ordering::Relaxed);
        // Only the low bits vary between calls; keeping them is the intent.
        bytes[..8].copy_from_slice(&nanos.to_le_bytes()[..8]);
        bytes[8..12].copy_from_slice(&std::process::id().to_le_bytes());
        bytes[12..].copy_from_slice(&count.to_le_bytes()[..4]);
    }
    Id::from_random_bytes(bytes)
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConnectError {
    NotRunning,
    LeaseHeld,
    Refused,
    Unsafe(String),
    Protocol(String),
}

fn own_uid() -> Option<u32> {
    fs::metadata("/proc/self").ok().map(|m| m.uid())
}

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
    let path = dir.join(CONTROL_CAPABILITY_NAME);
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

pub struct Link {
    pub service_epoch: Id,
    pub reader: FrameReader<UnixStream>,
    pub writer: FrameWriter<UnixStream>,
    pub stream: UnixStream,
}

pub fn connect(dir: &Path) -> Result<Link, ConnectError> {
    let socket = dir.join(CONTROL_SOCKET_NAME);
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
        Err(TransportError::Wire(WireError::VersionMismatch)) => {
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
                Event::ServiceNotice(n) if n.code == NoticeCode::LeaseHeld => {
                    ConnectError::LeaseHeld
                }
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
    thread::Builder::new()
        .name("lade-reap".into())
        .spawn(move || {
            let _ = child.wait();
        })?;
    Ok(())
}

pub enum Input {
    Intent(Intent),
    Frame(u64, ClientEnvelope),
    Closed(u64),
    Shutdown,
}

/// Whether this connection attempt has started `laded`, when the window may start it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spawn {
    NotTried,
    Started,
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    Connecting {
        started: Instant,
        attempt: u32,
        next: Instant,
        spawn: Spawn,
        after_loss: bool,
    },
    Connected,
    Waiting,
}

impl Phase {
    fn connecting(after_loss: bool, delay: Duration) -> Self {
        let now = Instant::now();
        Self::Connecting {
            started: now,
            attempt: 0,
            next: now + delay,
            spawn: Spawn::NotTried,
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
    stopping: bool,
}

pub fn spawn_driver<S: UpdateSink>(
    config: Config,
    sink: S,
    tx: Sender<Input>,
    rx: Receiver<Input>,
) -> std::io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("lade-transport".into())
        .spawn(move || {
            let core = Core::new(unique_id, config.open.clone());
            let mut driver = Driver {
                config,
                sink,
                tx,
                core,
                link: None,
                generation: 0,
                phase: Phase::connecting(false, Duration::ZERO),
                stopping: false,
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
                    if matches!(intent, Intent::StopService { .. }) && self.link.is_some() {
                        self.stopping = true;
                    }
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

    fn maybe_connect(&mut self) -> bool {
        let Phase::Connecting {
            started,
            attempt,
            next,
            spawn,
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
        let mut spawn = spawn;
        let failure: Option<String> = match connect(&self.config.runtime_dir) {
            Ok(link) => return self.connected(link),
            Err(ConnectError::NotRunning) => match (&self.config.spawn, spawn) {
                // Waiting for it again would not help, so a failed start is reported at once.
                (Some(service), Spawn::NotTried) => {
                    match spawn_service(service, &self.config.runtime_dir) {
                        Ok(()) => {
                            spawn = Spawn::Started;
                            None
                        }
                        Err(e) => Some(format!(
                            "LADE could not start the local service ({}): {e}.",
                            service.program.display()
                        )),
                    }
                }
                (Some(_), Spawn::Started) => (!in_window)
                    .then(|| "LADE started the local service but could not reach it.".to_owned()),
                (None, _) => (!in_window).then(|| {
                    "LADE could not reach the local service. Start it with “laded serve”, then retry."
                        .to_owned()
                }),
            },
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
                    spawn,
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
        if self.stopping {
            self.phase = Phase::Waiting;
            return self.status(ConnectionState::Disconnected {
                reason: "The service was stopped.".into(),
            });
        }
        self.phase = Phase::connecting(true, backoff(0, true));
        self.status(ConnectionState::Reconnecting { attempt: 0 })
    }

    fn deliver(&mut self, step: Step) -> bool {
        let mut followups = Step::default();
        for frame in step.frames {
            let Some(link) = self.link.as_mut().filter(|l| !l.failed) else {
                continue;
            };
            let request_id = frame.request_id.clone();
            match link
                .writer
                .write(&ClientEnvelope::new(ClientBody::Request(frame)))
            {
                Ok(()) => {}
                Err(TransportError::Io(_)) => self.fail_link(),
                Err(TransportError::Wire(e)) => {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_backoff_doubles_up_to_its_cap() {
        let first: Vec<u64> = (0..6)
            .map(|attempt| u64::try_from(backoff(attempt, false).as_millis()).unwrap())
            .collect();
        assert_eq!(first, [100, 200, 400, 800, 1_000, 1_000]);
        assert_eq!(backoff(0, true), Duration::from_millis(250));
        assert_eq!(backoff(4, true), Duration::from_millis(4_000));
        assert_eq!(backoff(u32::MAX, true), Duration::from_millis(5_000));
    }

    #[test]
    fn unique_ids_are_distinct_version_4_uuids() {
        let a = unique_id();
        let b = unique_id();
        assert_ne!(a, b);
        assert!(Id::parse(a.as_str()).is_ok());
        assert_eq!(a.as_str().as_bytes()[14], b'4');
    }
}
