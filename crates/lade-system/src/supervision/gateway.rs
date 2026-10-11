//! Per-launch egress gateway: an HTTP `CONNECT` proxy on a Unix socket, the worker sandbox's
//! only network path.
//!
//! Policy is an allowlist of exact `(host, port)` pairs (port 443 by default). Only
//! `CONNECT host:port HTTP/1.x` with a bounded (8 KiB) header is accepted; there is no plaintext
//! request forwarding. The host is resolved once, every resolved address must be public unless
//! the rule explicitly allows private addresses, and the connection goes to one of those checked
//! addresses (never re-resolved). When the service runs with `HTTPS_PROXY`, tunnels are chained
//! through that proxy (minimal `NO_PROXY` semantics), after the allowlist check. Stopping the
//! gateway removes the socket and shuts down every open tunnel, so revocation also closes access
//! that was already granted. Diagnostics carry only `host:port` and a decision code.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

/// Bound on the `CONNECT` request head (and on an upstream proxy's response head).
pub const MAX_HEADER_BYTES: usize = 8 << 10;
/// Default bound on concurrently open connections per gateway.
pub const MAX_TUNNELS: usize = 32;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const COPY_BUFFER: usize = 16 << 10;

/// One allowed destination. `host` is an exact lowercase DNS name or an IP literal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EgressRule {
    pub host: String,
    pub port: u16,
    /// Permit loopback/private/link-local resolution results (custom local providers only).
    pub allow_private: bool,
}

impl EgressRule {
    /// A public HTTPS destination (`host:443`).
    pub fn https(host: &str) -> Self {
        Self {
            host: normalize_host(host),
            port: 443,
            allow_private: false,
        }
    }
}

/// The destinations a worker may reach.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EgressPolicy {
    rules: Vec<EgressRule>,
}

impl EgressPolicy {
    /// No network at all.
    pub fn none() -> Self {
        Self::default()
    }

    /// `host:443` for each host, public addresses only.
    pub fn https_hosts(hosts: &[&str]) -> Self {
        Self {
            rules: hosts.iter().map(|h| EgressRule::https(h)).collect(),
        }
    }

    /// This policy plus further rules (for example hosts from settings).
    pub fn with_extra(mut self, rules: impl IntoIterator<Item = EgressRule>) -> Self {
        for mut rule in rules {
            rule.host = normalize_host(&rule.host);
            if !self.rules.contains(&rule) {
                self.rules.push(rule);
            }
        }
        self
    }

    pub fn rules(&self) -> &[EgressRule] {
        &self.rules
    }

    /// The rule allowing `host:port`, if any (exact match after normalization).
    pub fn allows(&self, host: &str, port: u16) -> Option<&EgressRule> {
        let host = normalize_host(host);
        self.rules.iter().find(|r| r.host == host && r.port == port)
    }
}

/// Default provider egress policies, keyed by pi provider id. The hosts are the base URLs of
/// pi-ai 0.85.1's built-in provider catalogues (`dist/providers/data/*.json`).
pub fn default_policies() -> BTreeMap<String, EgressPolicy> {
    [
        ("lade-fixture", &[][..]),
        ("anthropic", &["api.anthropic.com"][..]),
        ("openai", &["api.openai.com"][..]),
        ("google", &["generativelanguage.googleapis.com"][..]),
        ("openrouter", &["openrouter.ai"][..]),
        ("mistral", &["api.mistral.ai"][..]),
        ("groq", &["api.groq.com"][..]),
        ("xai", &["api.x.ai"][..]),
        ("deepseek", &["api.deepseek.com"][..]),
        ("cerebras", &["api.cerebras.ai"][..]),
        ("together", &["api.together.ai"][..]),
        ("fireworks", &["api.fireworks.ai"][..]),
        // IMP-011: further plain-variable providers (base URLs from the pinned catalogue).
        ("moonshotai", &["api.moonshot.ai"][..]),
        ("huggingface", &["router.huggingface.co"][..]),
        ("nvidia", &["integrate.api.nvidia.com"][..]),
        ("zai", &["api.z.ai"][..]),
        ("vercel-ai-gateway", &["ai-gateway.vercel.sh"][..]),
        ("kimi-coding", &["api.kimi.com"][..]),
    ]
    .into_iter()
    .map(|(id, hosts)| (id.to_owned(), EgressPolicy::https_hosts(hosts)))
    .collect()
}

fn normalize_host(host: &str) -> String {
    let host = host.strip_suffix('.').unwrap_or(host);
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    host.to_ascii_lowercase()
}

/// True when `addr` must not be reached without an explicit `allow_private` rule.
pub fn is_non_public(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => is_non_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_non_public_v4(v4);
            }
            let seg = v6.segments();
            // NAT64 well-known prefix 64:ff9b::/96 embeds an IPv4 destination.
            if seg[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                let [a, b] = seg[6].to_be_bytes();
                let [c, d] = seg[7].to_be_bytes();
                return is_non_public_v4(Ipv4Addr::new(a, b, c, d));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (seg[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (seg[0] & 0xffc0) == 0xfec0 // site-local fec0::/10
                || (seg[0] == 0x2001 && seg[1] == 0x0db8) // documentation
                || seg[..6] == [0, 0, 0, 0, 0, 0] // IPv4-compatible (deprecated)
        }
    }
}

fn is_non_public_v4(v4: Ipv4Addr) -> bool {
    let [a, b, c, _] = v4.octets();
    v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_multicast()
        || v4.is_broadcast()
        || v4.is_documentation()
        || a == 0 // "this network" 0.0.0.0/8
        || (a == 100 && (b & 0xc0) == 64) // shared address space 100.64.0.0/10
        || (a == 192 && b == 0 && c == 0) // IETF protocol assignments
        || (a == 198 && (b & 0xfe) == 18) // benchmarking 198.18.0.0/15
        || a >= 240 // reserved
}

/// An upstream HTTP proxy that tunnels are chained through (from `HTTPS_PROXY`).
#[derive(Clone)]
pub struct UpstreamProxy {
    host: String,
    port: u16,
    authorization: Option<String>,
    no_proxy: Vec<String>,
}

impl fmt::Debug for UpstreamProxy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UpstreamProxy")
            .field("host", &self.host)
            .field("port", &self.port)
            .field(
                "authorization",
                &self.authorization.as_ref().map(|_| "<redacted>"),
            )
            .field("no_proxy", &self.no_proxy)
            .finish()
    }
}

impl UpstreamProxy {
    /// `HTTPS_PROXY`/`https_proxy` and `NO_PROXY`/`no_proxy` of the service's environment.
    pub fn from_env() -> Option<Self> {
        let var = |upper: &str, lower: &str| {
            std::env::var(upper)
                .ok()
                .filter(|v| !v.is_empty())
                .or_else(|| std::env::var(lower).ok().filter(|v| !v.is_empty()))
        };
        Self::from_vars(
            var("HTTPS_PROXY", "https_proxy").as_deref(),
            var("NO_PROXY", "no_proxy").as_deref(),
        )
    }

    /// Parse `http://[user:pass@]host[:port][/]` (a missing scheme means `http`). Other schemes
    /// are not supported and yield `None`.
    pub fn from_vars(https_proxy: Option<&str>, no_proxy: Option<&str>) -> Option<Self> {
        let url = https_proxy?.trim();
        let rest = match url.split_once("://") {
            Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") => rest,
            Some(_) => return None,
            None => url,
        };
        let authority = rest.split('/').next()?;
        let (userinfo, hostport) = match authority.rsplit_once('@') {
            Some((u, h)) => (Some(u), h),
            None => (None, authority),
        };
        let (host, port) = split_authority(hostport, 80)?;
        let authorization = userinfo.map(|u| format!("Basic {}", base64(&percent_decode(u))));
        let no_proxy = no_proxy
            .unwrap_or_default()
            .split(',')
            .map(|e| e.trim().to_ascii_lowercase())
            .filter(|e| !e.is_empty())
            .collect();
        Some(Self {
            host,
            port,
            authorization,
            no_proxy,
        })
    }

    /// Minimal `NO_PROXY`: `*`, exact hosts/IP literals, and domain suffixes (`example.com`,
    /// `.example.com` and `*.example.com` all match `example.com` subdomains). Entries with a
    /// port or CIDR ranges match only literally.
    pub fn bypass(&self, host: &str) -> bool {
        let host = normalize_host(host);
        self.no_proxy.iter().any(|entry| {
            let entry = entry.trim_start_matches("*.").trim_start_matches('.');
            entry == "*"
                || host == entry
                || (host.len() > entry.len()
                    && host.ends_with(entry)
                    && host.as_bytes()[host.len() - entry.len() - 1] == b'.')
        })
    }
}

/// Split `host:port` / `[v6]:port` / `host` (default port).
fn split_authority(text: &str, default_port: u16) -> Option<(String, u16)> {
    if let Some(rest) = text.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None if after.is_empty() => default_port,
            None => return None,
        };
        host.parse::<Ipv6Addr>().ok()?;
        return Some((host.to_ascii_lowercase(), port));
    }
    let (host, port) = match text.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (text, default_port),
    };
    (!host.is_empty()).then(|| (host.to_ascii_lowercase(), port))
}

fn percent_decode(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The parsed, syntactically valid target of a `CONNECT` request.
#[derive(Debug, PartialEq, Eq)]
struct Target {
    host: String,
    port: u16,
}

/// Why a request was refused: HTTP status plus a stable decision code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Refusal {
    status: u16,
    code: &'static str,
}

const BAD_REQUEST: Refusal = Refusal {
    status: 400,
    code: "bad_request",
};

impl Refusal {
    fn reason(self) -> &'static str {
        match self.status {
            400 => "Bad Request",
            403 => "Forbidden",
            405 => "Method Not Allowed",
            431 => "Request Header Fields Too Large",
            502 => "Bad Gateway",
            503 => "Service Unavailable",
            _ => "Error",
        }
    }
}

/// Parse a complete request head (without the final blank line).
fn parse_request(head: &[u8]) -> Result<Target, Refusal> {
    let head = std::str::from_utf8(head).map_err(|_| BAD_REQUEST)?;
    let request_line = head.split("\r\n").next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let (Some(method), Some(authority), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(BAD_REQUEST);
    };
    if method != "CONNECT" {
        return Err(Refusal {
            status: 405,
            code: "method_not_allowed",
        });
    }
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(BAD_REQUEST);
    }
    let valid_chars = authority
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-.:[]".contains(&b));
    if !valid_chars || authority.len() > 261 {
        return Err(BAD_REQUEST);
    }
    // CONNECT requires an explicit port.
    let has_port = authority
        .rsplit_once(':')
        .is_some_and(|(h, p)| !p.is_empty() && (!h.starts_with('[') || h.ends_with(']')));
    let (host, port) = split_authority(authority, 0)
        .filter(|(_, port)| has_port && *port != 0)
        .ok_or(BAD_REQUEST)?;
    if !authority.starts_with('[') && host.contains(':') {
        return Err(BAD_REQUEST);
    }
    let host = normalize_host(&host);
    if host.parse::<IpAddr>().is_err() && !valid_dns_name(&host) {
        return Err(BAD_REQUEST);
    }
    Ok(Target { host, port })
}

fn valid_dns_name(host: &str) -> bool {
    host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

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

/// A running gateway. Dropping it stops it.
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
    /// Bind `socket` (mode 0600; its directory is created 0700 when missing and must not be
    /// group/other accessible) and serve `policy`. An existing file at `socket` is replaced.
    pub fn start(
        socket: &Path,
        policy: EgressPolicy,
        upstream: Option<UpstreamProxy>,
    ) -> io::Result<Self> {
        Self::start_with(socket, policy, upstream, MAX_TUNNELS)
    }

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
            .spawn(move || accept_loop(listener, accept_shared))?;
        Ok(Self {
            socket: socket.to_path_buf(),
            shared,
            accept: Some(accept),
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// Open connections (handshaking or tunnelling).
    pub fn active_connections(&self) -> usize {
        self.shared.active.load(Ordering::SeqCst)
    }

    /// Stop accepting, remove the socket and shut down every open connection. Idempotent.
    pub fn stop(&mut self) {
        let Some(accept) = self.accept.take() else {
            return;
        };
        self.shared.stopped.store(true, Ordering::SeqCst);
        // Wake the blocking accept.
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

/// Create `dir` 0700 if missing; refuse an existing directory with group/other access.
pub(crate) fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    match fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
    {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "run directory must be a 0700 directory",
        ));
    }
    Ok(())
}

fn accept_loop(listener: UnixListener, shared: Arc<Shared>) {
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
        let worker_shared = Arc::clone(&shared);
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

fn write_refusal(stream: &mut UnixStream, refusal: Refusal) -> io::Result<()> {
    let response = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: 0\r\nConnection: close\r\nX-Lade-Decision: {}\r\n\r\n",
        refusal.status,
        refusal.reason(),
        refusal.code
    );
    stream.write_all(response.as_bytes())
}

/// Read a request/response head up to the blank line. Returns `(head, leftover)`.
fn read_head(stream: &mut impl Read) -> Result<(Vec<u8>, Vec<u8>), Refusal> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            let leftover = buffer.split_off(end + 4);
            buffer.truncate(end);
            return Ok((buffer, leftover));
        }
        if buffer.len() >= MAX_HEADER_BYTES {
            return Err(Refusal {
                status: 431,
                code: "header_too_large",
            });
        }
        let room = (MAX_HEADER_BYTES + 3 - buffer.len()).min(chunk.len());
        match stream.read(&mut chunk[..room]) {
            Ok(0) => return Err(BAD_REQUEST),
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => {
                return Err(Refusal {
                    status: 400,
                    code: "handshake_failed",
                });
            }
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

/// Apply the policy and open the upstream connection (direct or through the proxy).
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

fn connect_via_proxy(proxy: &UpstreamProxy, target: &Target) -> io::Result<TcpStream> {
    let addrs: Vec<SocketAddr> = (proxy.host.as_str(), proxy.port)
        .to_socket_addrs()?
        .collect();
    let mut stream = addrs
        .iter()
        .find_map(|addr| TcpStream::connect_timeout(addr, CONNECT_TIMEOUT).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::ConnectionRefused, "proxy"))?;
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
    let authority = if target.host.contains(':') {
        format!("[{}]:{}", target.host, target.port)
    } else {
        format!("{}:{}", target.host, target.port)
    };
    let mut request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some(auth) = &proxy.authorization {
        request.push_str("Proxy-Authorization: ");
        request.push_str(auth);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes())?;
    let (head, leftover) =
        read_head(&mut stream).map_err(|_| io::Error::other("proxy response"))?;
    let status_ok = head
        .strip_prefix(b"HTTP/1.")
        .and_then(|rest| rest.get(1..5))
        .is_some_and(|code| code.first() == Some(&b' ') && code.get(1) == Some(&b'2'));
    if !status_ok || !leftover.is_empty() {
        return Err(io::Error::other("proxy refused"));
    }
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(None)?;
    Ok(stream)
}

/// Copy bytes both ways with fixed buffers until either side closes.
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
    // Either direction ending ends the tunnel.
    from.shutdown_both();
    to.shutdown_both();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_connect() {
        assert_eq!(
            parse_request(b"CONNECT API.Anthropic.com.:443 HTTP/1.1\r\nHost: x"),
            Ok(Target {
                host: "api.anthropic.com".into(),
                port: 443
            })
        );
        assert_eq!(
            parse_request(b"CONNECT [::1]:8443 HTTP/1.1"),
            Ok(Target {
                host: "::1".into(),
                port: 8443
            })
        );
        assert_eq!(
            parse_request(b"GET http://x/ HTTP/1.1").unwrap_err().status,
            405
        );
        for bad in [
            &b"CONNECT host HTTP/1.1"[..],
            b"CONNECT host:0 HTTP/1.1",
            b"CONNECT host:99999 HTTP/1.1",
            b"CONNECT -bad-:443 HTTP/1.1",
            b"CONNECT a..b:443 HTTP/1.1",
            b"CONNECT a:443 HTTP/2",
            b"CONNECT a:443 HTTP/1.1 extra",
            b"CONNECT a@b:443 HTTP/1.1",
            b"CONNECT ::1:443 HTTP/1.1",
        ] {
            assert_eq!(parse_request(bad).unwrap_err().status, 400, "{bad:?}");
        }
    }

    #[test]
    fn classifies_addresses() {
        for addr in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "ff02::1",
            "::ffff:10.0.0.1",
            "::ffff:127.0.0.1",
            "64:ff9b::a00:1",
        ] {
            assert!(is_non_public(addr.parse().unwrap()), "{addr}");
        }
        for addr in [
            "1.1.1.1",
            "160.79.104.10",
            "2606:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            assert!(!is_non_public(addr.parse().unwrap()), "{addr}");
        }
    }

    #[test]
    fn proxy_env_and_no_proxy() {
        let proxy = UpstreamProxy::from_vars(
            Some("http://user:p%40ss@proxy.local:3128/"),
            Some("localhost, .internal,*.corp.example,exact.example.org"),
        )
        .unwrap();
        assert_eq!((proxy.host.as_str(), proxy.port), ("proxy.local", 3128));
        assert_eq!(
            proxy.authorization.as_deref(),
            Some(format!("Basic {}", base64(b"user:p@ss")).as_str())
        );
        assert!(!format!("{proxy:?}").contains("Basic"));
        assert!(proxy.bypass("localhost"));
        assert!(proxy.bypass("a.internal"));
        assert!(proxy.bypass("x.corp.example"));
        assert!(proxy.bypass("exact.example.org"));
        assert!(!proxy.bypass("sub.exact.example.org.evil"));
        assert!(!proxy.bypass("api.anthropic.com"));
        assert!(UpstreamProxy::from_vars(Some("socks5://x:1"), None).is_none());
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"abc"), "YWJj");
        assert_eq!(base64(b"a"), "YQ==");
    }

    #[test]
    fn policy_matching() {
        let policy = default_policies()["anthropic"]
            .clone()
            .with_extra([EgressRule::https("Extra.Example.")]);
        assert!(policy.allows("api.anthropic.com", 443).is_some());
        assert!(policy.allows("API.ANTHROPIC.COM.", 443).is_some());
        assert!(policy.allows("api.anthropic.com", 80).is_none());
        assert!(policy.allows("evil.api.anthropic.com", 443).is_none());
        assert!(policy.allows("extra.example", 443).is_some());
        assert!(default_policies()["lade-fixture"].rules().is_empty());
    }
}
