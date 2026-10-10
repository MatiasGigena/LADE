//! Client admission over the private control socket (`$RUNTIME/control.sock`).
//!
//! Bootstrap order: validate/create the 0700 runtime directory, write a fresh 256-bit capability
//! atomically to `control.cap` (0600), then bind `control.sock` (0600) — the capability exists before
//! anyone can connect. A stale socket is removed only after a failed connect probe.
//!
//! Admission per connection: `SO_PEERCRED` uid must equal the service uid, then exactly `hello`
//! (answered with `hello`), then `auth` with the capability (constant-time comparison; a mismatch
//! closes the link without a reply). Only then is the link handed to the service, which grants or
//! refuses the single controlling lease. Each admitted link has a reader thread and a writer thread
//! with a bounded outbound queue; the service never blocks on a slow client.

use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::net::Shutdown;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use lade_protocol::client::{ClientBody, ClientEnvelope, RequestFrame};
use lade_protocol::types::Id;
use lade_protocol::{FrameReader, Framing, decode_client_frame, encode};

use crate::service::convert::{constant_time_eq, hex, random_fill};

pub const SOCKET_NAME: &str = "control.sock";
pub const CAPABILITY_NAME: &str = "control.cap";
/// Outbound budget per link (frames and bytes) before the link is dropped.
pub const MAX_OUTBOUND_FRAMES: usize = 256;
pub const MAX_OUTBOUND_BYTES: usize = 8 * 1024 * 1024;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PENDING_HANDSHAKES: usize = 8;

#[derive(Debug)]
pub enum TransportError {
    UnsafeRuntimeDir(&'static str),
    AlreadyRunning,
    Io(&'static str, io::ErrorKind),
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsafeRuntimeDir(why) => write!(f, "unsafe runtime directory: {why}"),
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

/// The current process's real uid.
pub fn service_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Creates `dir` (0700) or validates an existing one: a real directory (not a symlink), owned by
/// the service uid, with no group/other permission bits.
pub fn prepare_private_dir(dir: &Path) -> Result<(), TransportError> {
    match fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = dir.parent() {
                fs::create_dir_all(parent).map_err(io_err("create runtime parent"))?;
            }
            fs::DirBuilder::new()
                .mode(0o700)
                .create(dir)
                .map_err(io_err("create runtime dir"))?;
        }
        Err(e) => return Err(TransportError::Io("inspect runtime dir", e.kind())),
        Ok(_) => {}
    }
    let meta = fs::symlink_metadata(dir).map_err(io_err("inspect runtime dir"))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(TransportError::UnsafeRuntimeDir("not a directory"));
    }
    if meta.uid() != service_uid() {
        return Err(TransportError::UnsafeRuntimeDir(
            "not owned by the service user",
        ));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(TransportError::UnsafeRuntimeDir(
            "group or other permissions are set",
        ));
    }
    Ok(())
}

/// Writes a fresh capability (64 lowercase hex) atomically as `dir/control.cap` (0600) and returns it.
pub fn write_capability(dir: &Path) -> Result<String, TransportError> {
    let mut secret = [0u8; 32];
    random_fill(&mut secret).map_err(io_err("random"))?;
    let capability = hex(&secret);
    let tmp = dir.join(format!("{CAPABILITY_NAME}.tmp"));
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
    fs::rename(&tmp, dir.join(CAPABILITY_NAME)).map_err(io_err("publish capability"))?;
    fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(io_err("sync runtime dir"))?;
    Ok(capability)
}

/// Binds `dir/control.sock` (0600). An existing socket is removed only when nothing answers it.
pub fn bind_socket(dir: &Path) -> Result<UnixListener, TransportError> {
    let path = dir.join(SOCKET_NAME);
    if let Ok(meta) = fs::symlink_metadata(&path) {
        if !meta.file_type().is_socket() {
            return Err(TransportError::UnsafeRuntimeDir(
                "control socket path is not a socket",
            ));
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

// ---------------------------------------------------------------------------------------------
// Links.

/// What an admitted connection reports to the service, in order.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Inbound {
    /// Authenticated; the service decides about the lease and answers through `out`.
    Attach {
        conn: u64,
        out: Outbound,
    },
    Request {
        conn: u64,
        frame: RequestFrame,
    },
    /// A well-formed request that failed the schema: answer `invalid_request`, keep the link.
    Invalid {
        conn: u64,
        request_id: Id,
    },
    Detach {
        conn: u64,
    },
}

#[derive(Default)]
struct OutState {
    frames: VecDeque<Vec<u8>>,
    bytes: usize,
    closing: bool,
    dead: bool,
}

/// Bounded outbound queue of one link, drained by its writer thread.
#[derive(Clone)]
pub struct Outbound {
    shared: Arc<(Mutex<OutState>, Condvar)>,
}

impl std::fmt::Debug for Outbound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Outbound")
    }
}

/// The outbound budget was exceeded; the caller should close the link with a notice.
#[derive(Debug)]
pub struct Overflow;

impl Outbound {
    fn new() -> Self {
        Self {
            shared: Arc::new((Mutex::new(OutState::default()), Condvar::new())),
        }
    }

    /// Queues one encoded frame. Never blocks.
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

    /// Discards queued frames, sends `last` (if any) and closes the link.
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

    /// Flushes queued frames, then closes.
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

/// Handle to the running listener.
pub struct Listener {
    stop: Arc<AtomicBool>,
    socket: PathBuf,
    thread: Option<JoinHandle<()>>,
}

impl Listener {
    /// Stops accepting (existing links are closed by the service) and joins the accept thread.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&self.socket);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Starts the accept loop. Admitted links are reported to `tx` as [`Inbound`] values.
pub fn serve<T: From<Inbound> + Send + 'static>(
    listener: UnixListener,
    socket: PathBuf,
    capability: String,
    tx: SyncSender<T>,
) -> Listener {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&stop);
    let thread = thread::Builder::new()
        .name("laded-accept".into())
        .spawn(move || accept_loop(&listener, &capability, &stop_flag, &tx))
        .expect("spawn accept thread");
    Listener {
        stop,
        socket,
        thread: Some(thread),
    }
}

fn accept_loop<T: From<Inbound> + Send + 'static>(
    listener: &UnixListener,
    capability: &str,
    stop: &AtomicBool,
    tx: &SyncSender<T>,
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
        let spawned = thread::Builder::new()
            .name(format!("laded-conn-{conn}"))
            .spawn(move || run_link(conn, stream, &capability, &tx, &pending));
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

/// Handshake, then forward frames until the link ends.
fn run_link<T: From<Inbound>>(
    conn: u64,
    stream: UnixStream,
    capability: &str,
    tx: &SyncSender<T>,
    pending: &AtomicUsize,
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
    let writer_out = out.clone();
    let writer = thread::Builder::new()
        .name(format!("laded-write-{conn}"))
        .spawn(move || write_loop(write_half, &writer_out));
    if writer.is_err()
        || tx
            .send(
                Inbound::Attach {
                    conn,
                    out: out.clone(),
                }
                .into(),
            )
            .is_err()
    {
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
