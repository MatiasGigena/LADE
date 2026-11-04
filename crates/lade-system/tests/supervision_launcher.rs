mod adapters_support;

use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, sync_channel};
use std::thread;
use std::time::{Duration, Instant};

use adapters_support::{TempDir, git, init_repo, skip_without_sandbox};
use lade_protocol::types::{DeliveryMode, Id, Model};
use lade_protocol::worker::{TurnState, WorkerBody};
use lade_system::adapters::sandbox::{SandboxTools, WORKER_LIMITS};
use lade_system::supervision::credentials::{CredentialSource, FIXTURE_REFUSED};
use lade_system::supervision::gateway::{EgressPolicy, EgressRule, default_policies};
use lade_system::supervision::launcher::{
    NODE_ARGS, default_worker_dir, resolve_node, worker_dir_from,
};
use lade_system::supervision::sweep::read_pidfile;
use lade_system::supervision::{
    LaunchBinding, LaunchError, LaunchRequest, LauncherConfig, SupervisorEvent, WorkerEvent,
    default_launcher, gateway_socket_path,
};

const OBJECTIVE: &str = "0b8a4c1e-7a1f-4f7e-9d7a-0d6c8f2a9b11";
const WAIT: Duration = Duration::from_secs(90);

fn prerequisites() -> Option<PathBuf> {
    if let Err(e) = SandboxTools::locate() {
        skip_without_sandbox(e);
        return None;
    }
    match resolve_node(None) {
        Ok(node) => Some(node),
        Err(e) => {
            skip_without_sandbox(e.detail());
            None
        }
    }
}

struct Fixture {
    tmp: TempDir,
    worktree: PathBuf,
    common_dir: PathBuf,
    session: PathBuf,
    run: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let tmp = TempDir::new(tag);
        let repo = init_repo(tmp.path(), "sha1");
        let worktree = tmp.path().join("wt");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "lade/0b8a4c1e",
                worktree.to_str().unwrap(),
            ],
        );
        let session = tmp.path().join("session");
        fs::create_dir(&session).unwrap();
        Self {
            common_dir: repo.join(".git"),
            worktree,
            session,
            run: tmp.path().join("run"),
            tmp,
        }
    }

    fn config(&self, node: PathBuf) -> LauncherConfig {
        let worker_dir = default_worker_dir().unwrap();
        LauncherConfig {
            node,
            entry: worker_dir.join("src/worker.ts"),
            node_args: NODE_ARGS.iter().map(OsString::from).collect(),
            worker_dir,
            run_dir: self.run.clone(),
            socket_dir: self.run.join("g"),
            limits: WORKER_LIMITS,
            policies: default_policies(),
            credentials: CredentialSource::empty(),
            upstream_proxy: None,
            extra_ro: Vec::new(),
            cgroups: None,
            test_fixture_provider: true,
        }
    }

    fn probe_config(&self, node: PathBuf, script: &str) -> LauncherConfig {
        let dir = self.tmp.path().join("probe");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("probe.cjs"), script).unwrap();
        LauncherConfig {
            entry: dir.join("probe.cjs"),
            worker_dir: dir,
            ..self.config(node)
        }
    }

    fn request(&self, provider: &str, model: &str, generation: u64) -> LaunchRequest {
        LaunchRequest {
            binding: LaunchBinding {
                objective_id: OBJECTIVE.into(),
                execution_generation: generation,
            },
            worktree: self.worktree.clone(),
            repo_common_dir: self.common_dir.clone(),
            session_dir: self.session.clone(),
            provider: provider.into(),
            start: WorkerBody::Start {
                objective_id: Id::parse(OBJECTIVE).unwrap(),
                session_dir: self.session.display().to_string(),
                cwd: self.worktree.display().to_string(),
                model: Model {
                    provider: provider.into(),
                    id: model.into(),
                },
                resume: false,
                system_note: String::new(),
                thinking: None,
                resources: None,
                session_file: None,
            },
        }
    }

    fn pidfile(&self, generation: u64) -> PathBuf {
        self.run
            .join(format!("worker-{OBJECTIVE}-{generation}.pid"))
    }

    fn socket(&self, generation: u64) -> PathBuf {
        gateway_socket_path(&self.run.join("g"), OBJECTIVE, generation)
    }
}

fn next(rx: &Receiver<SupervisorEvent>) -> WorkerEvent {
    let event = rx.recv_timeout(WAIT).expect("supervisor event");
    assert_eq!(event.binding.objective_id, OBJECTIVE);
    event.event
}

fn until(rx: &Receiver<SupervisorEvent>, stop: impl Fn(&WorkerEvent) -> bool) -> Vec<WorkerEvent> {
    let mut seen = Vec::new();
    loop {
        let event = next(rx);
        let done = stop(&event);
        seen.push(event);
        if done {
            return seen;
        }
        if matches!(seen.last(), Some(WorkerEvent::Exited { .. })) {
            panic!("worker exited early: {seen:?}");
        }
    }
}

fn alive(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
        s.rsplit(") ")
            .next()
            .is_some_and(|rest| !rest.starts_with('Z'))
    })
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    condition()
}

#[test]
fn real_worker_writes_file_with_fixture_model_and_terminates_cleanly() {
    let Some(node) = prerequisites() else { return };
    let fx = Fixture::new("launch");
    let launcher = default_launcher(fx.config(node)).unwrap();
    let (tx, rx) = sync_channel(64);
    let mut control = launcher
        .launch(fx.request("lade-fixture", "scripted", 1), tx)
        .unwrap();
    let records = read_pidfile(&fx.pidfile(1)).unwrap();
    assert_eq!(records.len(), 2, "outer bwrap and namespace init recorded");
    assert!(fx.socket(1).exists());

    let ready = until(&rx, |e| {
        matches!(e, WorkerEvent::Body(WorkerBody::Ready { .. }))
    });
    assert_eq!(ready.len(), 1, "{ready:?}");

    let input_id = Id::parse("6f1c2b3a-4d5e-4f60-8a7b-9c0d1e2f3a4b").unwrap();
    control
        .send(WorkerBody::Deliver {
            input_id: input_id.clone(),
            mode: DeliveryMode::Prompt,
            text: "please\nlade-fixture: write a.txt hello".into(),
        })
        .unwrap();
    let mut running = false;
    let seen = until(&rx, |e| {
        matches!(
            e,
            WorkerEvent::Body(WorkerBody::TurnState {
                state: TurnState::Idle
            })
        )
    });
    let mut received = false;
    let mut tool_items = 0;
    for event in &seen {
        match event {
            WorkerEvent::Body(WorkerBody::Received { input_id: got }) => {
                received = *got == input_id
            }
            WorkerEvent::Body(WorkerBody::TurnState {
                state: TurnState::Running,
            }) => running = true,
            WorkerEvent::Body(WorkerBody::ActivityItem {
                tool_name: Some(name),
                ..
            }) if name == "write" => tool_items += 1,
            WorkerEvent::Body(_) => {}
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert!(received && running && tool_items >= 1, "{seen:?}");
    assert_eq!(
        fs::read_to_string(fx.worktree.join("a.txt")).unwrap(),
        "hello"
    );

    control.terminate();
    let exited = until(&rx, |e| matches!(e, WorkerEvent::Exited { .. }));
    assert!(
        matches!(
            exited.last(),
            Some(WorkerEvent::Exited {
                code: Some(0),
                signal: None
            })
        ),
        "{exited:?}"
    );
    for record in records {
        assert!(
            wait_until(Duration::from_secs(5), || !alive(record.pid)),
            "pid {} survived",
            record.pid
        );
    }
    assert!(!fx.pidfile(1).exists());
    assert!(!fx.socket(1).exists());
    assert!(control.send(WorkerBody::Abort).is_err());
}

#[test]
fn long_state_directory_still_launches() {
    let Some(node) = prerequisites() else { return };
    let fx = Fixture::new("longstate");
    let long_run = fx
        .tmp
        .path()
        .join("s".repeat(120))
        .join("t".repeat(60))
        .join("run");
    assert!(long_run.as_os_str().len() > 108);
    let config = LauncherConfig {
        run_dir: long_run.clone(),
        ..fx.config(node)
    };
    let launcher = default_launcher(config).unwrap();
    let (tx, rx) = sync_channel(64);
    let mut control = launcher
        .launch(fx.request("lade-fixture", "scripted", 1), tx)
        .unwrap();
    assert!(long_run.join(format!("worker-{OBJECTIVE}-1.pid")).exists());
    assert!(fx.socket(1).exists());
    until(&rx, |e| {
        matches!(e, WorkerEvent::Body(WorkerBody::Ready { .. }))
    });
    control.terminate();
    until(&rx, |e| matches!(e, WorkerEvent::Exited { .. }));
    assert!(!fx.socket(1).exists());
}

#[test]
fn worker_package_resolves_from_env_then_bundle_layout() {
    let tmp = TempDir::new("workerdir");
    let bundle = tmp.path().join("bundle");
    let worker = bundle.join("lib/lade/pi-worker");
    fs::create_dir_all(worker.join("src")).unwrap();
    fs::write(worker.join("src/worker.ts"), "").unwrap();
    fs::create_dir_all(bundle.join("bin")).unwrap();
    let exe = bundle.join("bin/laded");
    fs::write(&exe, "").unwrap();
    let canonical = worker.canonicalize().unwrap();
    assert_eq!(worker_dir_from(None, Some(&exe)).unwrap(), canonical);
    let other = tmp.path().join("other");
    fs::create_dir_all(&other).unwrap();
    assert_eq!(
        worker_dir_from(Some(other.clone()), Some(&exe)).unwrap(),
        other.canonicalize().unwrap()
    );
    let missing = worker_dir_from(Some(tmp.path().join("missing")), Some(&exe)).unwrap_err();
    assert!(matches!(missing, LaunchError::SandboxUnavailable(_)));
    let loose = tmp.path().join("laded");
    fs::write(&loose, "").unwrap();
    let fallback = worker_dir_from(None, Some(&loose));
    assert_eq!(fallback.is_ok(), cfg!(debug_assertions), "{fallback:?}");
}

#[test]
fn gateway_socket_names_are_short() {
    let path = gateway_socket_path(Path::new("/run/user/1000/lade/g"), OBJECTIVE, 42);
    assert_eq!(
        path,
        Path::new("/run/user/1000/lade/g/0b8a4c1e7a1f-42.sock")
    );
}

const PROBE: &str = r#"
const fs = require('node:fs');
const net = require('node:net');
const crypto = require('node:crypto');
const out = (body) => process.stdout.write(JSON.stringify({ protocol: 1, bundle: '0.2.0', body }) + '\n');
let buffered = '';
let started = false;
process.stdin.on('data', (chunk) => {
  buffered += chunk;
  let nl;
  while ((nl = buffered.indexOf('\n')) >= 0) {
    const line = buffered.slice(0, nl); buffered = buffered.slice(nl + 1);
    const kind = JSON.parse(line).body.kind;
    if (kind === 'worker.start' && !started) { started = true; start(); }
    if (kind === 'worker.shutdown') process.exit(0);
  }
});
process.stdin.on('end', () => process.exit(0));
function connectVia(authority) {
  return new Promise((resolve) => {
    const s = net.connect(process.env.LADE_GATEWAY_SOCKET);
    let data = '';
    s.on('connect', () => s.write(`CONNECT ${authority} HTTP/1.1\r\nHost: ${authority}\r\n\r\n`));
    s.on('data', (d) => {
      data += d;
      if (data.startsWith('HTTP/1.1 200') && data.includes('\r\n\r\n') && !s.sent) { s.sent = true; s.write('ping'); }
      if (data.endsWith('pong')) { s.end(); }
    });
    s.on('error', (e) => resolve('error ' + e.code));
    s.on('close', () => resolve(data.split('\r\n')[0] + (data.endsWith('pong') ? ' pong' : '')));
  });
}
function direct() {
  return new Promise((resolve) => {
    const s = net.connect({ host: '1.1.1.1', port: 443 });
    s.on('connect', () => { s.destroy(); resolve('connected'); });
    s.on('error', (e) => resolve(e.code));
  });
}
// Node's own WebSocket without the LADE gateway installed: a direct connection attempt.
function websocket() {
  return new Promise((resolve) => {
    const ws = new WebSocket('wss://1.1.1.1/');
    ws.addEventListener('open', () => { ws.close(); resolve('connected'); });
    ws.addEventListener('error', () => resolve('failed'));
  });
}
async function start() {
  out({ kind: 'worker.ready', pi_session_id: 'probe', session_file: '/probe' });
  const report = {
    envKeys: Object.keys(process.env).sort(),
    openai: process.env.OPENAI_API_KEY,
    hostFile: fs.existsSync(process.env.LADE_TEST_OUTSIDE || '__OUTSIDE__'),
    home: fs.existsSync('/root'),
    direct: await direct(),
    websocket: await websocket(),
    allowed: await connectVia('__ALLOWED__'),
    denied: await connectVia('denied.example:443'),
  };
  out({ kind: 'activity.item', item_id: crypto.randomUUID(), role: 'system', title: 'probe', text: JSON.stringify(report), is_error: false, truncated: false });
}
"#;

fn echo_server() -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4];
        stream.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"ping");
        stream.write_all(b"pong").unwrap();
    });
    (port, handle)
}

#[test]
fn probe_sees_only_allowlisted_env_files_and_network() {
    let Some(node) = prerequisites() else { return };
    let fx = Fixture::new("probe");
    let outside = fx.tmp.path().join("outside-secret.txt");
    fs::write(&outside, "host secret").unwrap();
    let (port, server) = echo_server();
    let script = PROBE
        .replace("__OUTSIDE__", outside.to_str().unwrap())
        .replace("__ALLOWED__", &format!("127.0.0.1:{port}"));
    let mut config = fx.probe_config(node, &script);
    config.test_fixture_provider = false;
    config.policies.insert(
        "openai".into(),
        EgressPolicy::none().with_extra([EgressRule {
            host: "127.0.0.1".into(),
            port,
            allow_private: true,
        }]),
    );
    config.credentials = CredentialSource::new(
        [
            (
                OsString::from("OPENAI_API_KEY"),
                OsString::from("sk-test-value"),
            ),
            (OsString::from("GROQ_API_KEY"), OsString::from("gsk-other")),
            (OsString::from("HOME"), OsString::from("/root")),
        ],
        None,
    );
    let launcher = default_launcher(config).unwrap();
    let (tx, rx) = sync_channel(64);
    let mut control = launcher
        .launch(fx.request("openai", "gpt-test", 2), tx)
        .unwrap();
    for record in read_pidfile(&fx.pidfile(2)).unwrap() {
        let cmdline = fs::read(format!("/proc/{}/cmdline", record.pid)).unwrap_or_default();
        assert!(
            !String::from_utf8_lossy(&cmdline).contains("sk-test-value"),
            "credential on command line"
        );
    }
    let seen = until(&rx, |e| {
        matches!(e, WorkerEvent::Body(WorkerBody::ActivityItem { .. }))
    });
    let Some(WorkerEvent::Body(WorkerBody::ActivityItem { text, .. })) = seen.last() else {
        unreachable!()
    };
    server.join().unwrap();
    assert!(
        text.contains(
            r#""envKeys":["HOME","LADE_GATEWAY_SOCKET","LANG","OPENAI_API_KEY","PATH","PI_CODING_AGENT_DIR","PI_CODING_AGENT_SESSION_DIR","PI_OFFLINE","PI_SKIP_VERSION_CHECK","PI_TELEMETRY","TERM"]"#
        ),
        "{text}"
    );
    assert!(text.contains(r#""openai":"sk-test-value""#), "{text}");
    assert!(text.contains(r#""hostFile":false"#), "{text}");
    assert!(text.contains(r#""home":false"#), "{text}");
    assert!(text.contains(r#""direct":"ENETUNREACH""#), "{text}");
    assert!(text.contains(r#""websocket":"failed""#), "{text}");
    assert!(
        text.contains(r#""allowed":"HTTP/1.1 200 Connection Established pong""#),
        "{text}"
    );
    assert!(
        text.contains(r#""denied":"HTTP/1.1 403 Forbidden""#),
        "{text}"
    );
    control.terminate();
    let exited = until(&rx, |e| matches!(e, WorkerEvent::Exited { .. }));
    assert!(
        matches!(
            exited.last(),
            Some(WorkerEvent::Exited { code: Some(0), .. })
        ),
        "{exited:?}"
    );
}

const VIOLATOR: &str = r#"
process.stdin.once('data', () => {
  process.stdout.write(JSON.stringify({ protocol: 1, bundle: '0.2.0', body: { kind: 'worker.ready', pi_session_id: 'p', session_file: '/p' } }) + '\n');
  process.stdout.write(__FRAME__ + '\n');
  setInterval(() => {}, 1000);
});
"#;

fn violation(frame: &str) -> (Vec<WorkerEvent>, Vec<u32>, Fixture) {
    let node = prerequisites().expect("checked by caller");
    let fx = Fixture::new("violation");
    let launcher =
        default_launcher(fx.probe_config(node, &VIOLATOR.replace("__FRAME__", frame))).unwrap();
    let (tx, rx) = sync_channel(64);
    let control = launcher
        .launch(fx.request("lade-fixture", "scripted", 3), tx)
        .unwrap();
    let pids = read_pidfile(&fx.pidfile(3))
        .unwrap()
        .iter()
        .map(|r| r.pid)
        .collect();
    let seen = until(&rx, |e| matches!(e, WorkerEvent::Exited { .. }));
    drop(control);
    (seen, pids, fx)
}

#[test]
fn service_body_from_worker_is_a_violation_and_kills_the_sandbox() {
    if prerequisites().is_none() {
        return;
    }
    let frame =
        r#"JSON.stringify({ protocol: 1, bundle: '0.2.0', body: { kind: 'worker.shutdown' } })"#;
    let (seen, pids, fx) = violation(frame);
    assert!(
        matches!(seen[0], WorkerEvent::Body(WorkerBody::Ready { .. })),
        "{seen:?}"
    );
    assert!(
        matches!(
            seen[1],
            WorkerEvent::ProtocolViolation("service_body_from_worker")
        ),
        "{seen:?}"
    );
    assert!(
        matches!(
            seen[2],
            WorkerEvent::Exited {
                signal: Some(9),
                ..
            }
        ),
        "{seen:?}"
    );
    assert!(wait_until(Duration::from_secs(5), || pids
        .iter()
        .all(|p| !alive(*p))));
    assert!(!fx.pidfile(3).exists() && !fx.socket(3).exists());
}

#[test]
fn malformed_frame_is_a_violation() {
    if prerequisites().is_none() {
        return;
    }
    let (seen, _, _) = violation(
        r#"'{"protocol":1,"bundle":"0.2.0","body":{"kind":"turn.state","state":"busy"}}'"#,
    );
    assert!(
        matches!(seen[1], WorkerEvent::ProtocolViolation(code) if code != "service_body_from_worker"),
        "{seen:?}"
    );
    assert!(matches!(seen[2], WorkerEvent::Exited { .. }), "{seen:?}");
}

#[test]
fn unresponsive_worker_is_killed_on_terminate() {
    let Some(node) = prerequisites() else { return };
    let fx = Fixture::new("stuck");
    let script = "process.on('SIGTERM', () => {}); process.stdin.on('data', () => {}); process.stdin.on('end', () => {}); setInterval(() => {}, 1000);";
    let launcher = default_launcher(fx.probe_config(node, script)).unwrap();
    let (tx, rx) = sync_channel(64);
    let mut control = launcher
        .launch(fx.request("lade-fixture", "scripted", 4), tx)
        .unwrap();
    let pids: Vec<u32> = read_pidfile(&fx.pidfile(4))
        .unwrap()
        .iter()
        .map(|r| r.pid)
        .collect();
    let start = Instant::now();
    control.terminate();
    assert!(start.elapsed() < Duration::from_secs(8));
    let exited = until(&rx, |e| matches!(e, WorkerEvent::Exited { .. }));
    assert!(
        matches!(
            exited.last(),
            Some(WorkerEvent::Exited {
                signal: Some(9),
                ..
            })
        ),
        "{exited:?}"
    );
    assert!(wait_until(Duration::from_secs(5), || pids
        .iter()
        .all(|p| !alive(*p))));
    assert!(!fx.pidfile(4).exists());
}

#[test]
fn providers_without_credentials_or_policy_are_unavailable() {
    let Some(node) = prerequisites() else { return };
    let fx = Fixture::new("provider");
    let mut config = fx.config(node);
    config.credentials = CredentialSource::new(
        [(
            OsString::from("GROQ_API_KEY"),
            OsString::from("gsk-not-openai"),
        )],
        None,
    );
    let launcher = default_launcher(config).unwrap();
    let (tx, _rx) = sync_channel(4);
    let Err(LaunchError::ProviderUnavailable(message)) =
        launcher.launch(fx.request("openai", "gpt", 5), tx.clone())
    else {
        panic!("expected provider_unavailable");
    };
    assert!(message.contains("OPENAI_API_KEY"), "{message}");
    assert!(!message.contains("gsk-not-openai"));
    assert!(matches!(
        launcher.launch(fx.request("no-such-provider", "m", 5), tx.clone()),
        Err(LaunchError::ProviderUnavailable(_))
    ));
    let mut request = fx.request("lade-fixture", "scripted", 5);
    request.worktree = fx.tmp.path().to_path_buf();
    assert!(matches!(
        launcher.launch(request, tx),
        Err(LaunchError::Io(_))
    ));
    assert!(!Path::new(&fx.socket(5)).exists());
}

#[test]
fn the_fixture_provider_is_refused_without_the_test_switch() {
    let Some(node) = prerequisites() else { return };
    let fx = Fixture::new("nofixture");
    let config = LauncherConfig {
        test_fixture_provider: false,
        ..fx.config(node)
    };
    let launcher = default_launcher(config).unwrap();
    let Err(LaunchError::ProviderUnavailable(refused)) = launcher.preflight("lade-fixture") else {
        panic!("expected provider_unavailable at preflight");
    };
    assert_eq!(refused, FIXTURE_REFUSED);
    let (tx, _rx) = sync_channel(4);
    let Err(LaunchError::ProviderUnavailable(refused)) =
        launcher.launch(fx.request("lade-fixture", "scripted", 6), tx)
    else {
        panic!("expected provider_unavailable at launch");
    };
    assert_eq!(refused, FIXTURE_REFUSED);
    assert!(!Path::new(&fx.socket(6)).exists());
}
