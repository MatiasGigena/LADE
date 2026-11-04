//! The egress gateway of one worker: a Unix socket in the sandbox that accepts `CONNECT`
//! requests and tunnels the ones its policy allows to the provider over TCP.

mod policy;
mod proxy;
mod request;

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use crate::adapters::providers::is_non_public;
use crate::support::fs::ensure_private_dir;

pub use policy::{EgressPolicy, EgressRule, default_policies};
pub use proxy::UpstreamProxy;
use proxy::connect_via_proxy;
use request::{Refusal, Target, parse_request, read_head, write_refusal};

pub const MAX_HEADER_BYTES: usize = 8 << 10;
pub const MAX_TUNNELS: usize = 32;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const COPY_BUFFER: usize = 16 << 10;

struct Connections {
    next: AtomicU64,
    open: Mutex<HashMap<u64, Vec<Box<dyn ShutdownBoth + Send>>>>,
}

trait ShutdownBoth {
    fn shutdown_both(&self);
}
impl ShutdownBoth for UnixStream {
    fn shutdown_both(&self) {
        let _ = self.shutdown(Shutdown::Both);
    }
}
impl ShutdownBoth for TcpStream {
    fn shutdown_both(&self) {
        let _ = self.shutdown(Shutdown::Both);
    }
}

struct Shared {
    policy: EgressPolicy,
    upstream: Option<UpstreamProxy>,
    max_tunnels: usize,
    active: AtomicUsize,
    stopped: AtomicBool,
    connections: Connections,
    label: String,
}

impl Shared {
    fn register(&self, id: u64, stream: Box<dyn ShutdownBoth + Send>) -> bool {
        let mut open = self
            .connections
            .open
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if self.stopped.load(Ordering::SeqCst) {
            stream.shutdown_both();
            return false;
        }
        open.entry(id).or_default().push(stream);
        true
    }

    fn unregister(&self, id: u64) {
        self.connections
            .open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
    }

    fn log(&self, target: &str, code: &str) {
        eprintln!("lade-gateway[{}]: {target} {code}", self.label);
    }
}

pub struct Gateway {
    socket: PathBuf,
    shared: Arc<Shared>,
    accept: Option<thread::JoinHandle<()>>,
}

impl fmt::Debug for Gateway {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Gateway")
            .field("socket", &self.socket)
            .finish_non_exhaustive()
    }
}

impl Gateway {
    /// Starts a gateway on `socket` that tunnels HTTPS for the hosts `policy` allows, through
    /// `upstream` when set, with the default tunnel limit.
    ///
    /// # Errors
    ///
    /// Fails as [`Gateway::start_with`] does.
    pub fn start(
        socket: &Path,
        policy: EgressPolicy,
        upstream: Option<UpstreamProxy>,
    ) -> io::Result<Self> {
        Self::start_with(socket, policy, upstream, MAX_TUNNELS)
    }

    /// [`Gateway::start`] with at most `max_tunnels` open tunnels.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` when `socket` has no parent directory, or the operating system's error
    /// when that directory is unsafe, the socket cannot be replaced, bound or restricted, or the accept
    /// thread cannot be spawned.
    pub fn start_with(
        socket: &Path,
        policy: EgressPolicy,
        upstream: Option<UpstreamProxy>,
        max_tunnels: usize,
    ) -> io::Result<Self> {
        let dir = socket
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "gateway socket path"))?;
        ensure_private_dir(dir)?;
        match fs::remove_file(socket) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let listener = UnixListener::bind(socket)?;
        fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
        let label = socket
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let shared = Arc::new(Shared {
            policy,
            upstream,
            max_tunnels,
            active: AtomicUsize::new(0),
            stopped: AtomicBool::new(false),
            connections: Connections {
                next: AtomicU64::new(0),
                open: Mutex::new(HashMap::new()),
            },
            label,
        });
        let accept_shared = Arc::clone(&shared);
        let accept = thread::Builder::new()
            .name("lade-gateway".into())
            .spawn(move || accept_loop(&listener, &accept_shared))?;
        Ok(Self {
            socket: socket.to_path_buf(),
            shared,
            accept: Some(accept),
        })
    }

    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    #[must_use]
    pub fn active_connections(&self) -> usize {
        self.shared.active.load(Ordering::SeqCst)
    }

    pub fn stop(&mut self) {
        let Some(accept) = self.accept.take() else {
            return;
        };
        self.shared.stopped.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&self.socket);
        let _ = accept.join();
        let _ = fs::remove_file(&self.socket);
        let open = std::mem::take(
            &mut *self
                .shared
                .connections
                .open
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for stream in open.values().flatten() {
            stream.shutdown_both();
        }
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        self.stop();
    }
}

fn accept_loop(listener: &UnixListener, shared: &Arc<Shared>) {
    for stream in listener.incoming() {
        if shared.stopped.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = stream else { continue };
        if shared.active.fetch_add(1, Ordering::SeqCst) >= shared.max_tunnels {
            shared.active.fetch_sub(1, Ordering::SeqCst);
            let mut stream = stream;
            let refusal = Refusal {
                status: 503,
                code: "too_many_tunnels",
            };
            shared.log("-", refusal.code);
            let _ = write_refusal(&mut stream, refusal);
            continue;
        }
        let worker_shared = Arc::clone(shared);
        let spawned = thread::Builder::new()
            .name("lade-gateway-conn".into())
            .spawn(move || {
                let id = worker_shared
                    .connections
                    .next
                    .fetch_add(1, Ordering::SeqCst);
                serve(stream, id, &worker_shared);
                worker_shared.unregister(id);
                worker_shared.active.fetch_sub(1, Ordering::SeqCst);
            });
        if spawned.is_err() {
            shared.active.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

fn serve(mut client: UnixStream, id: u64, shared: &Shared) {
    let Ok(registered) = client.try_clone() else {
        return;
    };
    if !shared.register(id, Box::new(registered)) {
        return;
    }
    let _ = client.set_read_timeout(Some(HANDSHAKE_TIMEOUT));
    let _ = client.set_write_timeout(Some(HANDSHAKE_TIMEOUT));
    let decided = read_head(&mut client).and_then(|(head, leftover)| {
        let target = parse_request(&head)?;
        let label = format!("{}:{}", target.host, target.port);
        let opened = open_upstream(&target, shared).inspect_err(|r| shared.log(&label, r.code))?;
        Ok((label, opened, leftover))
    });
    let (label, upstream, leftover) = match decided {
        Ok(ok) => ok,
        Err(refusal) => {
            if matches!(refusal.status, 400 | 405 | 431) {
                shared.log("-", refusal.code);
            }
            let _ = write_refusal(&mut client, refusal);
            return;
        }
    };
    let Ok(registered) = upstream.try_clone() else {
        return;
    };
    if !shared.register(id, Box::new(registered)) {
        return;
    }
    if client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .is_err()
    {
        return;
    }
    shared.log(&label, "tunnel_open");
    let _ = client.set_read_timeout(None);
    let _ = client.set_write_timeout(None);
    let mut upstream = upstream;
    if !leftover.is_empty() && upstream.write_all(&leftover).is_err() {
        return;
    }
    tunnel(client, upstream);
    shared.log(&label, "tunnel_closed");
}

fn open_upstream(target: &Target, shared: &Shared) -> Result<TcpStream, Refusal> {
    let rule = shared
        .policy
        .allows(&target.host, target.port)
        .ok_or(Refusal {
            status: 403,
            code: "destination_denied",
        })?;
    let private = Refusal {
        status: 403,
        code: "private_address",
    };
    let literal = target.host.parse::<IpAddr>().ok();
    if let Some(ip) = literal
        && is_non_public(ip)
        && !rule.allow_private
    {
        return Err(private);
    }
    let unreachable = Refusal {
        status: 502,
        code: "upstream_unreachable",
    };
    if let Some(proxy) = shared.upstream.as_ref().filter(|p| !p.bypass(&target.host)) {
        return connect_via_proxy(proxy, target).map_err(|_| Refusal {
            status: 502,
            code: "proxy_failed",
        });
    }
    let addrs: Vec<SocketAddr> = match literal {
        Some(ip) => vec![SocketAddr::new(ip, target.port)],
        None => (target.host.as_str(), target.port)
            .to_socket_addrs()
            .map_err(|_| Refusal {
                status: 502,
                code: "resolution_failed",
            })?
            .collect(),
    };
    if addrs.is_empty() {
        return Err(unreachable);
    }
    if !rule.allow_private && addrs.iter().any(|a| is_non_public(a.ip())) {
        return Err(private);
    }
    addrs
        .iter()
        .find_map(|addr| TcpStream::connect_timeout(addr, CONNECT_TIMEOUT).ok())
        .ok_or(unreachable)
}

fn tunnel(client: UnixStream, upstream: TcpStream) {
    let (Ok(client_rx), Ok(upstream_tx)) = (client.try_clone(), upstream.try_clone()) else {
        return;
    };
    let forward = thread::Builder::new()
        .name("lade-gateway-copy".into())
        .spawn(move || {
            copy(client_rx, upstream_tx);
        });
    copy(upstream, client);
    if let Ok(forward) = forward {
        let _ = forward.join();
    }
}

fn copy<R: Read + ShutdownBoth, W: Write + ShutdownBoth>(mut from: R, mut to: W) {
    let mut buffer = vec![0u8; COPY_BUFFER];
    loop {
        match from.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                if to.write_all(&buffer[..n]).is_err() {
                    break;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    from.shutdown_both();
    to.shutdown_both();
}
