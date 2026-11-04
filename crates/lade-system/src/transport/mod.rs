use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::net::Shutdown;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use lade_protocol::client::{ClientBody, ClientEnvelope, RequestFrame};
use lade_protocol::types::{CONTROL_CAPABILITY_NAME, CONTROL_SOCKET_NAME, Id};
use lade_protocol::{FrameReader, Framing, decode_client_frame, encode};

use crate::support::fs::service_uid;
use crate::support::{constant_time_eq, hex, random_fill};

pub const MAX_OUTBOUND_FRAMES: usize = 256;
pub const MAX_OUTBOUND_BYTES: usize = 8 * 1024 * 1024;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
pub const WRITE_STALL_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PENDING_HANDSHAKES: usize = 8;

#[derive(Debug)]
pub enum TransportError {
    UnsafeSocketPath,
    AlreadyRunning,
    Io(&'static str, io::ErrorKind),
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsafeSocketPath => f.write_str("control socket path is not a socket"),
            Self::AlreadyRunning => {
                f.write_str("another service is listening on the control socket")
            }
            Self::Io(stage, kind) => write!(f, "control socket {stage} failed: {kind}"),
        }
    }
}

impl std::error::Error for TransportError {}

fn io_err(stage: &'static str) -> impl Fn(io::Error) -> TransportError {
    move |e| TransportError::Io(stage, e.kind())
}

pub fn write_capability(dir: &Path) -> Result<String, TransportError> {
    let mut secret = [0u8; 32];
    random_fill(&mut secret).map_err(io_err("random"))?;
    let capability = hex(&secret);
    let tmp = dir.join(format!("{CONTROL_CAPABILITY_NAME}.tmp"));
    let _ = fs::remove_file(&tmp);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(io_err("create capability"))?;
    file.write_all(capability.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(io_err("write capability"))?;
    drop(file);
    fs::rename(&tmp, dir.join(CONTROL_CAPABILITY_NAME)).map_err(io_err("publish capability"))?;
    fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(io_err("sync runtime dir"))?;
    Ok(capability)
}

pub fn ensure_not_running(dir: &Path) -> Result<(), TransportError> {
    let path = dir.join(CONTROL_SOCKET_NAME);
    let socket = fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_socket());
    if socket && UnixStream::connect(&path).is_ok() {
        return Err(TransportError::AlreadyRunning);
    }
    Ok(())
}

pub fn bind_socket(dir: &Path) -> Result<UnixListener, TransportError> {
    let path = dir.join(CONTROL_SOCKET_NAME);
    if let Ok(meta) = fs::symlink_metadata(&path) {
        if !meta.file_type().is_socket() {
            return Err(TransportError::UnsafeSocketPath);
        }
        if UnixStream::connect(&path).is_ok() {
            return Err(TransportError::AlreadyRunning);
        }
        fs::remove_file(&path).map_err(io_err("remove stale socket"))?;
    }
    let listener = UnixListener::bind(&path).map_err(io_err("bind"))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(io_err("chmod"))?;
    Ok(listener)
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Inbound {
    Attach { conn: u64, out: Outbound },
    Request { conn: u64, frame: RequestFrame },
    Invalid { conn: u64, request_id: Id },
    Detach { conn: u64 },
}

#[derive(Default)]
struct OutState {
    frames: VecDeque<Vec<u8>>,
    bytes: usize,
    closing: bool,
    dead: bool,
}

#[derive(Clone)]
pub struct Outbound {
    shared: Arc<(Mutex<OutState>, Condvar)>,
}

impl std::fmt::Debug for Outbound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Outbound")
    }
}

#[derive(Debug)]
pub struct Overflow;

impl Outbound {
    fn new() -> Self {
        Self {
            shared: Arc::new((Mutex::new(OutState::default()), Condvar::new())),
        }
    }

    pub fn push(&self, frame: Vec<u8>) -> Result<(), Overflow> {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().unwrap_or_else(PoisonError::into_inner);
        if state.closing || state.dead {
            return Ok(());
        }
        if state.frames.len() >= MAX_OUTBOUND_FRAMES
            || state.bytes + frame.len() > MAX_OUTBOUND_BYTES
        {
            return Err(Overflow);
        }
        state.bytes += frame.len();
        state.frames.push_back(frame);
        wake.notify_one();
        Ok(())
    }

    pub fn close_with(&self, last: Option<Vec<u8>>) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(frame) = last {
            state.frames.clear();
            state.bytes = frame.len();
            state.frames.push_back(frame);
        }
        state.closing = true;
        wake.notify_one();
    }

    pub fn close(&self) {
        let (lock, wake) = &*self.shared;
        lock.lock().unwrap_or_else(PoisonError::into_inner).closing = true;
        wake.notify_one();
    }

    fn next(&self) -> Option<Vec<u8>> {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(frame) = state.frames.pop_front() {
                state.bytes -= frame.len();
                return Some(frame);
            }
            if state.closing || state.dead {
                return None;
            }
            state = wake.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn mark_dead(&self) {
        let (lock, _) = &*self.shared;
        let mut state = lock.lock().unwrap_or_else(PoisonError::into_inner);
        state.dead = true;
        state.frames.clear();
        state.bytes = 0;
    }
}

/// Counts link writer threads that may still be flushing frames to a client.
#[derive(Default)]
struct Writers {
    running: Mutex<usize>,
    finished: Condvar,
}

impl Writers {
    fn started(&self) {
        *self.running.lock().unwrap_or_else(PoisonError::into_inner) += 1;
    }

    fn finished(&self) {
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        *running = running.saturating_sub(1);
        self.finished.notify_all();
    }
}

pub struct Listener {
    stop: Arc<AtomicBool>,
    socket: PathBuf,
    thread: Option<JoinHandle<()>>,
    writers: Arc<Writers>,
}

impl Listener {
    /// Stops accepting connections. Links already admitted keep running.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let Some(thread) = self.thread.take() else {
            return;
        };
        let _ = UnixStream::connect(&self.socket);
        let _ = thread.join();
    }

    /// Waits, at most `timeout`, until every link writer has flushed its closed queue, so the
    /// last frames (a stop reply, a final notice) reach clients before the service exits.
    pub fn drain(&self, timeout: Duration) {
        let running = self
            .writers
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let _ = self
            .writers
            .finished
            .wait_timeout_while(running, timeout, |running| *running > 0);
    }
}

#[must_use]
pub fn serve<T: From<Inbound> + Send + 'static>(
    listener: UnixListener,
    socket: PathBuf,
    capability: String,
    tx: SyncSender<T>,
) -> Listener {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&stop);
    let writers = Arc::new(Writers::default());
    let link_writers = Arc::clone(&writers);
    let thread = thread::Builder::new()
        .name("laded-accept".into())
        .spawn(move || accept_loop(&listener, &capability, &stop_flag, &tx, &link_writers))
        .expect("spawn accept thread");
    Listener {
        stop,
        socket,
        thread: Some(thread),
        writers,
    }
}

fn accept_loop<T: From<Inbound> + Send + 'static>(
    listener: &UnixListener,
    capability: &str,
    stop: &AtomicBool,
    tx: &SyncSender<T>,
    writers: &Arc<Writers>,
) {
    static NEXT_CONN: AtomicU64 = AtomicU64::new(1);
    let pending = Arc::new(AtomicUsize::new(0));
    let uid = service_uid();
    for stream in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = stream else { continue };
        let peer_ok = rustix::net::sockopt::socket_peercred(&stream)
            .is_ok_and(|cred| cred.uid.as_raw() == uid);
        if !peer_ok {
            eprintln!("laded: rejected connection: peer_uid_mismatch");
            continue;
        }
        if pending.fetch_add(1, Ordering::SeqCst) >= MAX_PENDING_HANDSHAKES {
            pending.fetch_sub(1, Ordering::SeqCst);
            continue;
        }
        let conn = NEXT_CONN.fetch_add(1, Ordering::Relaxed);
        let capability = capability.to_owned();
        let tx = tx.clone();
        let pending = Arc::clone(&pending);
        let writers = Arc::clone(writers);
        let spawned = thread::Builder::new()
            .name(format!("laded-conn-{conn}"))
            .spawn(move || run_link(conn, stream, &capability, &tx, &pending, writers));
        if spawned.is_err() {
            eprintln!("laded: could not start link thread");
        }
    }
}

fn write_now(stream: &mut UnixStream, body: ClientBody) -> bool {
    encode(&ClientEnvelope::new(body), Framing::Client)
        .ok()
        .is_some_and(|bytes| stream.write_all(&bytes).is_ok())
}

fn run_link<T: From<Inbound>>(
    conn: u64,
    stream: UnixStream,
    capability: &str,
    tx: &SyncSender<T>,
    pending: &AtomicUsize,
    writers: Arc<Writers>,
) {
    let admitted = handshake(&stream, capability);
    pending.fetch_sub(1, Ordering::SeqCst);
    let Some(mut reader) = admitted else {
        let _ = stream.shutdown(Shutdown::Both);
        return;
    };
    let _ = stream.set_read_timeout(None);
    let out = Outbound::new();
    let Ok(write_half) = stream.try_clone() else {
        return;
    };
    let _ = write_half.set_write_timeout(Some(WRITE_STALL_TIMEOUT));
    let writer_out = out.clone();
    writers.started();
    let writer_done = Arc::clone(&writers);
    let writer = thread::Builder::new()
        .name(format!("laded-write-{conn}"))
        .spawn(move || {
            write_loop(write_half, &writer_out);
            writer_done.finished();
        });
    if writer.is_err() {
        writers.finished();
        let _ = stream.shutdown(Shutdown::Both);
        return;
    }
    if tx
        .send(
            Inbound::Attach {
                conn,
                out: out.clone(),
            }
            .into(),
        )
        .is_err()
    {
        out.close();
        let _ = stream.shutdown(Shutdown::Both);
        return;
    }
    while let Ok(Some(frame)) = reader.read_frame() {
        let message = match decode_client_frame(&frame) {
            Ok(ClientEnvelope {
                body: ClientBody::Request(frame),
                ..
            }) => Inbound::Request { conn, frame },
            Ok(_) => break,
            Err(rejection) => match rejection.request_id {
                Some(request_id) => Inbound::Invalid { conn, request_id },
                None => break,
            },
        };
        if tx.send(message.into()).is_err() {
            break;
        }
    }
    let _ = tx.send(Inbound::Detach { conn }.into());
    out.close();
    let _ = stream.shutdown(Shutdown::Read);
}

fn handshake(stream: &UnixStream, capability: &str) -> Option<FrameReader<UnixStream>> {
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT)).ok()?;
    let mut reader = FrameReader::new(stream.try_clone().ok()?, Framing::Client);
    let mut write = stream.try_clone().ok()?;
    let first = reader.read_frame().ok()??;
    if !matches!(decode_client_frame(&first).ok()?.body, ClientBody::Hello) {
        return None;
    }
    if !write_now(&mut write, ClientBody::Hello) {
        return None;
    }
    let second = reader.read_frame().ok()??;
    let ClientBody::Auth { capability: proof } = decode_client_frame(&second).ok()?.body else {
        return None;
    };
    if !constant_time_eq(proof.as_str().as_bytes(), capability.as_bytes()) {
        eprintln!("laded: rejected connection: capability_mismatch");
        return None;
    }
    Some(reader)
}

fn write_loop(mut stream: UnixStream, out: &Outbound) {
    while let Some(frame) = out.next() {
        if stream.write_all(&frame).is_err() {
            out.mark_dead();
            break;
        }
    }
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Both);
}
