//! The upstream HTTPS proxy a gateway tunnels through when the environment names one, and
//! the hosts that bypass it.

use std::fmt;
use std::io::{self, Write};
use std::net::{Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs};

use super::policy::normalize_host;
use super::request::{Target, read_head};
use super::{CONNECT_TIMEOUT, HANDSHAKE_TIMEOUT};

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

pub(super) fn split_authority(text: &str, default_port: u16) -> Option<(String, u16)> {
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
            && let Ok(byte) = u8::try_from(h * 16 + l)
        {
            out.push(byte);
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

pub(super) fn connect_via_proxy(proxy: &UpstreamProxy, target: &Target) -> io::Result<TcpStream> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
