//! The one request a gateway accepts, `CONNECT host:port`, and the refusals it answers
//! everything else with.

use std::io::{self, Read, Write};
use std::net::IpAddr;
use std::os::unix::net::UnixStream;

use super::MAX_HEADER_BYTES;
use super::policy::normalize_host;
use super::proxy::split_authority;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Target {
    pub(super) host: String,
    pub(super) port: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Refusal {
    pub(super) status: u16,
    pub(super) code: &'static str,
}

pub(super) const BAD_REQUEST: Refusal = Refusal {
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

pub(super) fn parse_request(head: &[u8]) -> Result<Target, Refusal> {
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

pub(super) fn valid_dns_name(host: &str) -> bool {
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

pub(super) fn write_refusal(stream: &mut UnixStream, refusal: Refusal) -> io::Result<()> {
    let response = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: 0\r\nConnection: close\r\nX-Lade-Decision: {}\r\n\r\n",
        refusal.status,
        refusal.reason(),
        refusal.code
    );
    stream.write_all(response.as_bytes())
}

pub(super) fn read_head(stream: &mut impl Read) -> Result<(Vec<u8>, Vec<u8>), Refusal> {
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
}
