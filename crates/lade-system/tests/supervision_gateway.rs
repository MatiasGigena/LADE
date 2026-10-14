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

const TLS_CLIENT: &str = r#"
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import https from "node:https";
import { createInterface } from "node:readline";

const [gatewayModule, key, cert] = process.argv.slice(2);
const { installGateway } = await import(gatewayModule);
const options = { key: readFileSync(key), cert: readFileSync(cert) };
const listen = () => new Promise(resolve => {
  const server = https.createServer(options, (request, response) => {
    response.end(`tls origin saw ${request.url} over ${request.socket.getProtocol()}`);
  });
  server.on("upgrade", (request, socket) => {
    const accept = createHash("sha1").update(`${request.headers["sec-websocket-key"]}258EAFA5-E914-47DA-95CA-C5AB0DC85B11`).digest("base64");
    socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
    socket.once("data", frame => {
      const mask = frame.subarray(2, 6);
      const payload = Buffer.from(frame.subarray(6, 6 + (frame[1] & 0x7f)).map((byte, i) => byte ^ mask[i % 4]));
      const reply = Buffer.from(`${payload} over ${socket.getProtocol()}`);
      socket.write(Buffer.concat([Buffer.from([0x81, reply.length]), reply]));
    });
  });
  server.listen(0, "127.0.0.1", () => resolve(server));
});
const echo = url => new Promise((resolve, reject) => {
  const socket = new WebSocket(url);
  socket.addEventListener("open", () => socket.send("ping"));
  socket.addEventListener("message", event => { resolve(String(event.data)); socket.close(); });
  socket.addEventListener("error", () => reject(Object.assign(new Error("websocket"), { code: "websocket_failed" })));
});
const allowed = await listen();
const denied = await listen();
console.log(`PORTS ${allowed.address().port} ${denied.address().port}`);
const socket = await new Promise(resolve => createInterface({ input: process.stdin }).once("line", resolve));
installGateway(socket);
const get = url => new Promise((resolve, reject) => {
  https.get(url, response => {
    let body = "";
    response.setEncoding("utf8");
    response.on("data", chunk => { body += chunk; });
    response.once("end", () => resolve(body));
  }).once("error", reject);
});
const outcome = async work => {
  try { return { ok: await work() }; } catch (error) { return { error: String(error.code ?? error.cause?.code ?? error) }; }
};
const base = port => `https://127.0.0.1:${port}`;
const report = {
  fetch: await outcome(async () => (await fetch(`${base(allowed.address().port)}/v1/models`)).text()),
  agent: await outcome(() => get(`${base(allowed.address().port)}/v1/agent`)),
  fetchDenied: await outcome(async () => (await fetch(`${base(denied.address().port)}/x`)).text()),
  agentDenied: await outcome(() => get(`${base(denied.address().port)}/x`)),
  websocket: await outcome(() => echo(`wss://127.0.0.1:${allowed.address().port}/v1/responses`)),
  websocketDenied: await outcome(() => echo(`wss://127.0.0.1:${denied.address().port}/x`)),
};
console.log(`REPORT ${JSON.stringify(report)}`);
process.exit(0);
"#;

fn on_path(program: &str) -> Option<std::path::PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

#[test]
fn worker_https_and_wss_reach_a_local_tls_origin_through_the_gateway() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    let (Some(node), Some(openssl)) = (on_path("node"), on_path("openssl")) else {
        eprintln!("skipping TLS gateway test: node or openssl not installed");
        return;
    };
    let tmp = TempDir::new("gw-tls");
    let (key, cert) = (tmp.path().join("key.pem"), tmp.path().join("cert.pem"));
    let generated = Command::new(&openssl)
        .args(["req", "-x509", "-newkey", "ec"])
        .args([
            "-pkeyopt",
            "ec_paramgen_curve:prime256v1",
            "-nodes",
            "-days",
            "1",
        ])
        .args(["-subj", "/CN=lade-test-origin"])
        .args(["-addext", "subjectAltName=IP:127.0.0.1"])
        .arg("-keyout")
        .arg(&key)
        .arg("-out")
        .arg(&cert)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(
        generated.success(),
        "openssl could not generate a certificate"
    );
    let script = tmp.path().join("client.mjs");
    std::fs::write(&script, TLS_CLIENT).unwrap();
    let module = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/pi-worker/src/worker/gateway.ts")
        .canonicalize()
        .unwrap();
    let mut child = Command::new(node)
        .arg("--experimental-strip-types")
        .arg("--no-warnings")
        .arg(&script)
        .arg(format!("file://{}", module.display()))
        .arg(&key)
        .arg(&cert)
        .env("NODE_EXTRA_CA_CERTS", &cert)
        .env_remove("HTTPS_PROXY")
        .env_remove("https_proxy")
        .env_remove("HTTP_PROXY")
        .env_remove("http_proxy")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let ports = lines.next().unwrap().unwrap();
    let ports: Vec<u16> = ports
        .strip_prefix("PORTS ")
        .unwrap()
        .split(' ')
        .map(|p| p.parse().unwrap())
        .collect();
    let socket = tmp.path().join("run/gw.sock");
    let gateway = Gateway::start(
        &socket,
        EgressPolicy::none().with_extra([local_rule(ports[0])]),
        None,
    )
    .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "{}", socket.display()).unwrap();
    let report = lines
        .map_while(Result::ok)
        .find_map(|line| line.strip_prefix("REPORT ").map(str::to_owned))
        .expect("client report");
    assert!(child.wait().unwrap().success());
    drop(gateway);
    let value: serde_json::Value = serde_json::from_str(&report).unwrap();
    for (name, path) in [("fetch", "/v1/models"), ("agent", "/v1/agent")] {
        let body = value[name]["ok"]
            .as_str()
            .unwrap_or_else(|| panic!("{report}"));
        assert!(
            body.starts_with(&format!("tls origin saw {path} over TLSv1.")),
            "{report}"
        );
    }
    for name in ["fetchDenied", "agentDenied"] {
        assert_eq!(value[name]["error"], "gateway_refused", "{report}");
    }
    let echoed = value["websocket"]["ok"]
        .as_str()
        .unwrap_or_else(|| panic!("{report}"));
    assert!(echoed.starts_with("ping over TLSv1."), "{report}");
    assert_eq!(
        value["websocketDenied"]["error"], "websocket_failed",
        "{report}"
    );
}
