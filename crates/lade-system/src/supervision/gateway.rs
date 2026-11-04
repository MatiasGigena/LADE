use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv6Addr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use crate::adapters::providers::{BUILT_IN_PROVIDERS, is_non_public};
use crate::support::fs::ensure_private_dir;

pub const MAX_HEADER_BYTES: usize = 8 << 10;
pub const MAX_TUNNELS: usize = 32;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const COPY_BUFFER: usize = 16 << 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EgressRule {
    pub host: String,
    pub port: u16,
    pub allow_private: bool,
}

impl EgressRule {
    #[must_use]
    pub fn https(host: &str) -> Self {
        Self {
            host: normalize_host(host),
            port: 443,
            allow_private: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EgressPolicy {
    rules: Vec<EgressRule>,
}

impl EgressPolicy {
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn https_hosts(hosts: &[&str]) -> Self {
        Self {
            rules: hosts.iter().map(|h| EgressRule::https(h)).collect(),
        }
    }

    #[must_use]
    pub fn with_extra(mut self, rules: impl IntoIterator<Item = EgressRule>) -> Self {
        for mut rule in rules {
            rule.host = normalize_host(&rule.host);
            if !self.rules.contains(&rule) {
                self.rules.push(rule);
            }
        }
        self
    }

    #[must_use]
    pub fn rules(&self) -> &[EgressRule] {
        &self.rules
    }

    #[must_use]
    pub fn allows(&self, host: &str, port: u16) -> Option<&EgressRule> {
        let host = normalize_host(host);
        self.rules.iter().find(|r| r.host == host && r.port == port)
    }
}

/// The reviewed endpoint policy of every built-in provider: HTTPS to its documented API hosts.
#[must_use]
pub fn default_policies() -> BTreeMap<String, EgressPolicy> {
    BUILT_IN_PROVIDERS
        .iter()
        .map(|p| (p.id.to_owned(), EgressPolicy::https_hosts(p.hosts)))
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
    #[must_use]
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

    #[must_use]
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

    #[must_use]
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

#[derive(Debug, PartialEq, Eq)]
struct Target {
    host: String,
    port: u16,
}

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
            .spawn(move || accept_loop(listener, accept_shared))?;
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
