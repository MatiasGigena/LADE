//! Egress gateway integration tests over a real Unix socket and loopback TCP servers. No
//! external network is used.

mod adapters_support;

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use adapters_support::TempDir;
use lade_system::supervision::gateway::{
    EgressPolicy, EgressRule, Gateway, MAX_HEADER_BYTES, UpstreamProxy,
};

fn local_rule(port: u16) -> EgressRule {
    EgressRule {
        host: "127.0.0.1".into(),
        port,
        allow_private: true,
    }
}

/// A loopback server that echoes everything back on each accepted connection.
fn echo_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            thread::spawn(move || {
                let mut stream = stream;
                let mut buf = [0u8; 1024];
                while let Ok(n) = stream.read(&mut buf) {
                    if n == 0 || stream.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            });
        }
    });
    port
}

fn connect(socket: &Path, request: &[u8]) -> (UnixStream, String) {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    // The gateway may refuse (and close) before reading everything.
    let _ = stream.write_all(request);
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => break,
        }
    }
    (stream, String::from_utf8_lossy(&head).into_owned())
}

fn status_line(response: &str) -> &str {
    response.split("\r\n").next().unwrap_or_default()
}

#[test]
fn allowed_tunnel_round_trips_and_socket_is_private() {
    let tmp = TempDir::new("gw-allow");
    let port = echo_server();
    let socket = tmp.path().join("run/gw.sock");
    let gateway = Gateway::start(
        &socket,
        EgressPolicy::none().with_extra([local_rule(port)]),
        None,
    )
    .unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&socket), 0o600);
    assert_eq!(mode(&tmp.path().join("run")), 0o700);
    let (mut stream, head) = connect(
        &socket,
        format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes(),
    );
    assert_eq!(status_line(&head), "HTTP/1.1 200 Connection Established");
    stream.write_all(b"ping").unwrap();
    let mut buf = [0u8; 4];
    stream.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"ping");
    assert_eq!(gateway.active_connections(), 1);
    drop(stream);
    let start = Instant::now();
    while gateway.active_connections() > 0 && start.elapsed() < Duration::from_secs(5) {
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(gateway.active_connections(), 0);
}

#[test]
fn refusals() {
    let tmp = TempDir::new("gw-refuse");
    let port = echo_server();
    let socket = tmp.path().join("run/gw.sock");
    let policy = EgressPolicy::https_hosts(&["api.anthropic.com"]).with_extra([
        EgressRule {
            host: "localhost".into(),
            port,
            allow_private: false,
        },
        EgressRule {
            host: "127.0.0.1".into(),
            port,
            allow_private: false,
        },
    ]);
    let _gateway = Gateway::start(&socket, policy, None).unwrap();
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (
            b"CONNECT denied.example:443 HTTP/1.1\r\n\r\n".to_vec(),
            "HTTP/1.1 403 Forbidden",
        ),
        (
            b"CONNECT api.anthropic.com:80 HTTP/1.1\r\n\r\n".to_vec(),
            "HTTP/1.1 403 Forbidden",
        ),
        // Allowlisted names that resolve to private addresses are blocked by default.
        (
            format!("CONNECT localhost:{port} HTTP/1.1\r\n\r\n").into_bytes(),
            "HTTP/1.1 403 Forbidden",
        ),
        (
            format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n").into_bytes(),
            "HTTP/1.1 403 Forbidden",
        ),
        (
            b"GET http://api.anthropic.com/ HTTP/1.1\r\nHost: api.anthropic.com\r\n\r\n".to_vec(),
            "HTTP/1.1 405 Method Not Allowed",
        ),
        (
            b"CONNECT api.anthropic.com HTTP/1.1\r\n\r\n".to_vec(),
            "HTTP/1.1 400 Bad Request",
        ),
        (
            {
                let mut big = b"CONNECT api.anthropic.com:443 HTTP/1.1\r\nX-Pad: ".to_vec();
                big.extend(std::iter::repeat_n(b'a', MAX_HEADER_BYTES));
                big.extend_from_slice(b"\r\n\r\n");
                big
            },
            "HTTP/1.1 431 Request Header Fields Too Large",
        ),
    ];
    for (request, expected) in cases {
        let (_, head) = connect(&socket, &request);
        assert_eq!(
            status_line(&head),
            expected,
            "{}",
            String::from_utf8_lossy(&request[..request.len().min(60)])
        );
        assert!(head.contains("X-Lade-Decision: "), "{head}");
    }
}

#[test]
fn stop_closes_open_tunnels_and_removes_socket() {
    let tmp = TempDir::new("gw-stop");
    let port = echo_server();
    let socket = tmp.path().join("run/gw.sock");
    let mut gateway = Gateway::start(
        &socket,
        EgressPolicy::none().with_extra([local_rule(port)]),
        None,
    )
    .unwrap();
    let (mut stream, head) = connect(
        &socket,
        format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n").as_bytes(),
    );
    assert_eq!(status_line(&head), "HTTP/1.1 200 Connection Established");
    // A second connection still in its handshake is closed too.
    let mut pending = UnixStream::connect(&socket).unwrap();
    pending.write_all(b"CONNECT 127.0.0.1").unwrap();
    gateway.stop();
    assert!(!socket.exists());
    let mut buf = [0u8; 16];
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    assert_eq!(stream.read(&mut buf).unwrap(), 0, "tunnel closed");
    pending
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    assert!(matches!(pending.read(&mut buf), Ok(0) | Err(_)));
    assert!(UnixStream::connect(&socket).is_err());
}

#[test]
fn concurrent_tunnels_are_bounded() {
    let tmp = TempDir::new("gw-max");
    let port = echo_server();
    let socket = tmp.path().join("run/gw.sock");
    let _gateway = Gateway::start_with(
        &socket,
        EgressPolicy::none().with_extra([local_rule(port)]),
        None,
        2,
    )
    .unwrap();
    let request = format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n");
    let open: Vec<_> = (0..2)
        .map(|_| {
            let (stream, head) = connect(&socket, request.as_bytes());
            assert_eq!(status_line(&head), "HTTP/1.1 200 Connection Established");
            stream
        })
        .collect();
    let (_, head) = connect(&socket, request.as_bytes());
    assert_eq!(status_line(&head), "HTTP/1.1 503 Service Unavailable");
    drop(open);
}

#[test]
fn tunnels_chain_through_upstream_proxy_after_the_allowlist() {
    let tmp = TempDir::new("gw-proxy");
    let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_port = proxy.local_addr().unwrap().port();
    let seen = thread::spawn(move || {
        let (mut stream, _) = proxy.accept().unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        stream
            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .unwrap();
        let mut ping = [0u8; 4];
        stream.read_exact(&mut ping).unwrap();
        stream.write_all(b"pong").unwrap();
        stream.shutdown(Shutdown::Both).unwrap();
        String::from_utf8(head).unwrap()
    });
    let upstream = UpstreamProxy::from_vars(
        Some(&format!("http://u:p@127.0.0.1:{proxy_port}")),
        Some("localhost"),
    );
    let socket = tmp.path().join("run/gw.sock");
    let _gateway = Gateway::start(
        &socket,
        EgressPolicy::https_hosts(&["api.example.test"]),
        upstream,
    )
    .unwrap();
    // Denied hosts never reach the proxy.
    let (_, head) = connect(&socket, b"CONNECT other.example.test:443 HTTP/1.1\r\n\r\n");
    assert_eq!(status_line(&head), "HTTP/1.1 403 Forbidden");
    let (mut stream, head) = connect(&socket, b"CONNECT api.example.test:443 HTTP/1.1\r\n\r\n");
    assert_eq!(status_line(&head), "HTTP/1.1 200 Connection Established");
    stream.write_all(b"ping").unwrap();
    let mut pong = [0u8; 4];
    stream.read_exact(&mut pong).unwrap();
    assert_eq!(&pong, b"pong");
    let head = seen.join().unwrap();
    assert!(
        head.starts_with("CONNECT api.example.test:443 HTTP/1.1\r\n"),
        "{head}"
    );
    assert!(
        head.contains("Proxy-Authorization: Basic dTpw\r\n"),
        "{head}"
    );
}

#[test]
fn unreachable_destination_is_bad_gateway() {
    let tmp = TempDir::new("gw-unreach");
    // Bind and drop to obtain a closed loopback port.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let socket = tmp.path().join("run/gw.sock");
    let _gateway = Gateway::start(
        &socket,
        EgressPolicy::none().with_extra([local_rule(port)]),
        None,
    )
    .unwrap();
    let (_, head) = connect(
        &socket,
        format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n").as_bytes(),
    );
    assert_eq!(status_line(&head), "HTTP/1.1 502 Bad Gateway");
}
