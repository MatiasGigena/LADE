use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use lade_protocol::client::{
    self as wire, ClientBody, ClientEnvelope, Event, EventFrame, NoticeLevel, Reply, ReplyData,
    Request, ServiceNotice,
};
use lade_protocol::types::{CONTROL_CAPABILITY_NAME, CONTROL_SOCKET_NAME, Counter};
use lade_protocol::{FrameReader, FrameWriter, Framing};

use super::connection::{self, Config, ConnectError, Input, random_id, spawn_driver};
use super::requests::{
    self,
    tests::{epoch, snapshot_data},
};
use crate::presentation::controller::intents::{Intent, StartPoint};
use crate::presentation::state::model::{
    Basis, ConnectionState, DeliveryMode, ExecutionState, Intent as ObjectiveIntent, ModelRef,
    ObjectiveId, RepoPath,
};
use crate::presentation::state::store::StoreUpdate;

const TIMEOUT: Duration = Duration::from_secs(60);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("lade-{tag}-{}", &random_id().as_str()[..8]));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn private_dir(path: &Path) {
    fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(path)
        .unwrap();
}

fn start(runtime_dir: &Path, open: Option<PathBuf>) -> (Sender<Input>, Receiver<StoreUpdate>) {
    let (tx, rx) = channel();
    let (updates_tx, updates) = channel();
    let config = Config {
        runtime_dir: runtime_dir.to_path_buf(),
        spawn: None,
        open,
    };
    spawn_driver(config, updates_tx, tx.clone(), rx).unwrap();
    (tx, updates)
}

fn wait_for(
    updates: &Receiver<StoreUpdate>,
    what: &str,
    mut pred: impl FnMut(&StoreUpdate) -> bool,
) -> StoreUpdate {
    let deadline = Instant::now() + TIMEOUT;
    let mut seen = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match updates.recv_timeout(left) {
            Ok(update) if pred(&update) => return update,
            Ok(update) => seen.push(format!("{update:?}").chars().take(300).collect::<String>()),
            Err(_) => panic!("timed out waiting for {what}; saw:\n{}", seen.join("\n")),
        }
    }
}

const CAPABILITY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn fake_runtime(dir: &Path) -> UnixListener {
    private_dir(dir);
    let cap = dir.join(CONTROL_CAPABILITY_NAME);
    fs::write(&cap, CAPABILITY).unwrap();
    fs::set_permissions(&cap, fs::Permissions::from_mode(0o600)).unwrap();
    UnixListener::bind(dir.join(CONTROL_SOCKET_NAME)).unwrap()
}

struct FakeLink {
    reader: FrameReader<std::os::unix::net::UnixStream>,
    writer: FrameWriter<std::os::unix::net::UnixStream>,
}

impl FakeLink {
    fn send(&mut self, body: ClientBody) {
        self.writer.write(&ClientEnvelope::new(body)).unwrap();
    }

    fn recv(&mut self) -> ClientBody {
        self.reader
            .read::<ClientEnvelope>()
            .unwrap()
            .expect("frame")
            .body
    }

    fn request(&mut self) -> wire::RequestFrame {
        loop {
            match self.recv() {
                ClientBody::Request(frame)
                    if matches!(
                        frame.request.name(),
                        wire::RequestName::ModelsList | wire::RequestName::ResourcesList
                    ) => {}
                ClientBody::Request(frame) => return frame,
                other => panic!("expected a request, got {other:?}"),
            }
        }
    }
}

fn accept(listener: &UnixListener) -> FakeLink {
    let (stream, _) = listener.accept().unwrap();
    let mut link = FakeLink {
        reader: FrameReader::new(stream.try_clone().unwrap(), Framing::Client),
        writer: FrameWriter::new(stream, Framing::Client),
    };
    assert_eq!(link.recv(), ClientBody::Hello);
    link.send(ClientBody::Hello);
    match link.recv() {
        ClientBody::Auth { capability } => assert_eq!(capability.as_str(), CAPABILITY),
        other => panic!("expected auth, got {other:?}"),
    }
    link
}

fn event(sequence: u64) -> ClientBody {
    ClientBody::Event(requests::tests::event(sequence))
}

#[test]
fn fake_server_handshake_gap_resync_reconnect_and_lease_refusal() {
    let dir = TempDir::new("fake");
    let runtime = dir.0.join("run");
    let listener = fake_runtime(&runtime);
    let server = thread::spawn(move || {
        let mut link = accept(&listener);
        link.send(ClientBody::Welcome {
            service_epoch: epoch(),
            lease_id: random_id(),
        });
        let first = link.request();
        assert_eq!(first.request.name(), wire::RequestName::Snapshot);
        assert!(first.operation_id.is_none());
        link.send(ClientBody::Reply(Reply::ok(
            first.request_id,
            snapshot_data(5),
        )));
        link.send(event(6));
        link.send(event(8));
        let resync = link.request();
        assert_eq!(resync.request.name(), wire::RequestName::Snapshot);
        link.send(ClientBody::Reply(Reply::ok(
            resync.request_id,
            snapshot_data(8),
        )));
        let command = link.request();
        assert_eq!(command.request.name(), wire::RequestName::ReviewFreeze);
        let operation_id = command.operation_id.clone().expect("operation id");
        drop(link);

        let mut link = accept(&listener);
        link.send(ClientBody::Welcome {
            service_epoch: epoch(),
            lease_id: random_id(),
        });
        let mut saw_query = false;
        for _ in 0..2 {
            let frame = link.request();
            match frame.request {
                Request::Snapshot(_) => {
                    link.send(ClientBody::Reply(Reply::ok(
                        frame.request_id,
                        snapshot_data(9),
                    )));
                }
                Request::OperationGet(query) => {
                    assert_eq!(query.operation_id, operation_id);
                    saw_query = true;
                    link.send(ClientBody::Reply(Reply::ok(
                        frame.request_id,
                        ReplyData::OperationGet(wire::OperationStatus {
                            state: wire::OperationState::Unknown,
                            outcome: None,
                        }),
                    )));
                }
                other => panic!("unexpected request after reconnect: {other:?}"),
            }
        }
        assert!(saw_query);
        drop(link);

        let mut link = accept(&listener);
        link.send(ClientBody::Event(EventFrame {
            service_epoch: epoch(),
            sequence: Counter::new(0),
            event: Event::ServiceNotice(ServiceNotice {
                level: NoticeLevel::Error,
                code: "unauthorized".into(),
                message: "another client holds the controlling lease".into(),
            }),
        }));
    });

    let (tx, updates) = start(&runtime, None);
    wait_for(&updates, "connected", |u| {
        u == &StoreUpdate::Connection(ConnectionState::Connected)
    });
    wait_for(&updates, "snapshot", |u| {
        matches!(u, StoreUpdate::Snapshot { .. })
    });
    wait_for(&updates, "event 6", |u| {
        matches!(u, StoreUpdate::ObjectiveChanged(_))
    });
    wait_for(&updates, "resync snapshot", |u| {
        matches!(u, StoreUpdate::Snapshot { .. })
    });
    tx.send(Input::Intent(Intent::Freeze {
        objective_id: ObjectiveId::new(super::requests::tests::O),
    }))
    .unwrap();
    wait_for(&updates, "reconnecting", |u| {
        matches!(
            u,
            StoreUpdate::Connection(ConnectionState::Reconnecting { .. })
        )
    });
    wait_for(
        &updates,
        "outcome check",
        |u| matches!(u, StoreUpdate::CommandStatus { status: Some(s), .. } if s.message.starts_with("Checking outcome")),
    );
    wait_for(
        &updates,
        "unknown outcome notice",
        |u| matches!(u, StoreUpdate::Notice(n) if n.code == "unknown_operation"),
    );
    wait_for(&updates, "lease held", |u| {
        u == &StoreUpdate::Connection(ConnectionState::LeaseHeld)
    });
    server.join().unwrap();
    let _ = tx.send(Input::Shutdown);
}

#[test]
fn missing_service_without_spawn_reports_unavailable() {
    let dir = TempDir::new("none");
    let (tx, updates) = start(&dir.0.join("run"), None);
    let update = wait_for(&updates, "unavailable", |u| {
        matches!(
            u,
            StoreUpdate::Connection(ConnectionState::Unavailable { .. })
        )
    });
    let StoreUpdate::Connection(ConnectionState::Unavailable { reason }) = update else {
        unreachable!()
    };
    assert!(reason.contains("laded serve"), "{reason}");
    let _ = tx.send(Input::Shutdown);
}

#[test]
fn capability_must_be_private() {
    let dir = TempDir::new("cap");
    let runtime = dir.0.join("run");
    let _listener = fake_runtime(&runtime);
    assert!(connection::read_capability(&runtime).is_ok());
    let cap = runtime.join(CONTROL_CAPABILITY_NAME);
    fs::set_permissions(&cap, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        connection::read_capability(&runtime),
        Err(ConnectError::Unsafe(_))
    ));
    fs::set_permissions(&cap, fs::Permissions::from_mode(0o600)).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        connection::read_capability(&runtime),
        Err(ConnectError::Unsafe(_))
    ));
}

#[test]
fn runtime_dir_follows_the_service_defaults() {
    let env = |vars: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    };
    let dir = |explicit: Option<&str>, state: Option<&str>, vars| {
        connection::runtime_dir(explicit.map(Path::new), state.map(Path::new), env(vars))
    };
    assert_eq!(
        dir(Some("/r"), None, &[("XDG_RUNTIME_DIR", "/x")]),
        Some(PathBuf::from("/r"))
    );
    assert_eq!(
        dir(
            None,
            None,
            &[("LADE_RUNTIME_DIR", "/l"), ("XDG_RUNTIME_DIR", "/x")]
        ),
        Some(PathBuf::from("/l"))
    );
    assert_eq!(
        dir(None, None, &[("XDG_RUNTIME_DIR", "/x")]),
        Some(PathBuf::from("/x/lade"))
    );
    assert_eq!(
        dir(None, None, &[("XDG_STATE_HOME", "/s"), ("HOME", "/h")]),
        Some(PathBuf::from("/s/lade/run"))
    );
    assert_eq!(
        dir(None, None, &[("HOME", "/h")]),
        Some(PathBuf::from("/h/.local/state/lade/run"))
    );
    assert_eq!(
        dir(None, Some("/st"), &[("HOME", "/h")]),
        Some(PathBuf::from("/st/run"))
    );
    assert_eq!(dir(None, None, &[("XDG_RUNTIME_DIR", "relative")]), None);
}

fn laded() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.parent()?.join("laded");
    candidate.is_file().then_some(candidate)
}

struct Service(Child);

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "LADE Test")
        .env("GIT_AUTHOR_EMAIL", "test@lade.invalid")
        .env("GIT_COMMITTER_NAME", "LADE Test")
        .env("GIT_COMMITTER_EMAIL", "test@lade.invalid")
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

#[test]
fn real_service_workspace_objective_execution_and_change_set() {
    let Some(laded) = laded() else {
        eprintln!("skipped: build laded first (cargo build -p lade-system)");
        return;
    };
    let dir = TempDir::new("live");
    let repo = dir.0.join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("README.md"), "hello\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    let runtime = dir.0.join("run");
    let _service = Service(
        Command::new(&laded)
            .arg("serve")
            .arg("--state-dir")
            .arg(dir.0.join("state"))
            .arg("--runtime-dir")
            .arg(&runtime)
            .env("LADE_TEST_FIXTURE_PROVIDER", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while !runtime.join(CONTROL_SOCKET_NAME).exists() {
        assert!(Instant::now() < deadline, "laded did not start");
        thread::sleep(Duration::from_millis(50));
    }

    let (tx, updates) = start(&runtime, Some(repo.clone()));
    let send = |intent| tx.send(Input::Intent(intent)).unwrap();
    wait_for(&updates, "connected", |u| {
        u == &StoreUpdate::Connection(ConnectionState::Connected)
    });
    let StoreUpdate::WorkspaceOpened(workspace) = wait_for(&updates, "workspace", |u| {
        matches!(u, StoreUpdate::WorkspaceOpened(_))
    }) else {
        unreachable!()
    };
    assert!(workspace.branches.contains(&"main".to_owned()));

    let (tx2, updates2) = start(&runtime, None);
    wait_for(&updates2, "lease held", |u| {
        u == &StoreUpdate::Connection(ConnectionState::LeaseHeld)
    });
    let _ = tx2.send(Input::Shutdown);

    send(Intent::CreateObjective {
        workspace_id: workspace.workspace_id.clone(),
        title: "Say hello".into(),
        intent: ObjectiveIntent {
            outcome: "hello.txt contains hi".into(),
            constraints: String::new(),
            acceptance: String::new(),
        },
        integration_target: "main".into(),
        start: StartPoint::Target,
    });
    let StoreUpdate::Select(objective_id) = wait_for(&updates, "created objective selected", |u| {
        matches!(u, StoreUpdate::Select(_))
    }) else {
        unreachable!()
    };
    send(Intent::LoadObjective {
        objective_id: objective_id.clone(),
    });
    send(Intent::StartExecution {
        objective_id: objective_id.clone(),
        model: ModelRef::new("lade-fixture", "scripted"),
        thinking: None,
    });
    wait_for(&updates, "execution started", |u| {
        matches!(u, StoreUpdate::ObjectiveDetail(d)
            if d.summary.objective_id == objective_id
            && matches!(d.summary.execution.state, ExecutionState::Starting | ExecutionState::Running | ExecutionState::Idle))
    });
    send(Intent::SendMessage {
        objective_id: objective_id.clone(),
        text: "lade-fixture: write hello.txt hi".into(),
        mode: DeliveryMode::Prompt,
    });
    wait_for(&updates, "input accepted", |u| {
        matches!(u, StoreUpdate::InputChanged { .. })
    });
    wait_for(
        &updates,
        "activity from the agent",
        |u| matches!(u, StoreUpdate::ActivityAppended { objective_id: o, .. } if o == &objective_id),
    );
    wait_for(
        &updates,
        "worktree changed",
        |u| matches!(u, StoreUpdate::WorktreeChanged { objective_id: o } if o == &objective_id),
    );
    send(Intent::LoadChanges {
        objective_id: objective_id.clone(),
        basis: Basis::Working,
    });
    let StoreUpdate::Changes(list) = wait_for(
        &updates,
        "change set",
        |u| matches!(u, StoreUpdate::Changes(l) if !l.files.is_empty()),
    ) else {
        unreachable!()
    };
    let path = RepoPath::utf8("hello.txt");
    assert!(list.files.iter().any(|f| f.path == path), "{list:?}");
    assert!(list.capture_id.is_some());
    send(Intent::LoadDiff {
        objective_id: objective_id.clone(),
        basis: Basis::Working,
        path: path.clone(),
    });
    let StoreUpdate::Diff(diff) = wait_for(&updates, "diff", |u| matches!(u, StoreUpdate::Diff(_)))
    else {
        unreachable!()
    };
    assert!(
        diff.hunks
            .iter()
            .flat_map(|h| &h.lines)
            .any(|l| l.text.contains("hi"))
    );
    send(Intent::OpenFile {
        objective_id: objective_id.clone(),
        basis: Basis::Working,
        path,
        side: crate::presentation::state::model::Side::New,
        offset: 0,
    });
    wait_for(
        &updates,
        "file page",
        |u| matches!(u, StoreUpdate::FilePage(p) if p.eof && p.text.as_deref().is_some_and(|t| t.contains("hi"))),
    );

    send(Intent::OpenTerminal {
        objective_id: objective_id.clone(),
        cols: 80,
        rows: 24,
    });
    let opened = wait_for(&updates, "terminal opened or refused", |u| {
        matches!(
            u,
            StoreUpdate::TerminalOpened { .. } | StoreUpdate::TerminalProblem { .. }
        )
    });
    if let StoreUpdate::TerminalOpened { terminal_id, .. } = opened {
        send(Intent::TerminalInput {
            objective_id: objective_id.clone(),
            terminal_id: terminal_id.clone(),
            data: "cat hello.txt; echo T_$((2+2))\r".into(),
        });
        let mut seen = String::new();
        while !seen.contains("T_4") {
            if let StoreUpdate::TerminalOutput { data, .. } = wait_for(
                &updates,
                "terminal output",
                |u| matches!(u, StoreUpdate::TerminalOutput { terminal_id: t, .. } if *t == terminal_id),
            ) {
                seen.push_str(&data);
            }
        }
        assert!(seen.contains("hi"), "{seen:?}");
    } else {
        eprintln!("note: terminal refused (no sandbox on this host): {opened:?}");
    }

    send(Intent::StopService {
        reason: crate::presentation::controller::intents::StopReason::Exit,
    });
    wait_for(&updates, "service stopped", |u| {
        u == &StoreUpdate::ServiceStopped
    });
    wait_for(&updates, "no reconnect after stop", |u| {
        matches!(
            u,
            StoreUpdate::Connection(ConnectionState::Disconnected { .. })
        )
    });
    let mut service = _service;
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = service.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "laded kept running after service.stop"
        );
        thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "{status:?}");
    assert!(!runtime.join(CONTROL_SOCKET_NAME).exists());
    thread::sleep(Duration::from_millis(500));
    while let Ok(update) = updates.try_recv() {
        assert!(
            !matches!(
                update,
                StoreUpdate::Connection(ConnectionState::Reconnecting { .. })
            ),
            "reconnected after Exit"
        );
    }
    let _ = tx.send(Input::Shutdown);
}
