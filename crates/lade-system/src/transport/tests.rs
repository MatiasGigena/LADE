use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::sync::mpsc::{self, Receiver};

use lade_protocol::client::{Empty, Request};
use lade_protocol::types::Capability;
use lade_protocol::{FrameReader, FrameWriter};

use super::*;
use crate::support::new_uuid;

const WAIT: Duration = Duration::from_secs(10);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "lade-transport-{tag}-{}-{}",
            std::process::id(),
            new_uuid()
        ));
        fs::create_dir_all(&path).expect("temp dir");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn queued(out: &Outbound) -> (usize, usize) {
    let state = out.shared.0.lock().expect("state");
    (state.frames.len(), state.bytes)
}

#[test]
fn outbound_bounds_frames_and_bytes_and_drops_pushes_after_close() {
    let out = Outbound::new();
    for _ in 0..MAX_OUTBOUND_FRAMES {
        out.push(vec![0; 1]).expect("within the frame bound");
    }
    assert!(out.push(vec![0; 1]).is_err(), "one frame too many");
    assert_eq!(queued(&out), (MAX_OUTBOUND_FRAMES, MAX_OUTBOUND_FRAMES));

    let out = Outbound::new();
    out.push(vec![0; MAX_OUTBOUND_BYTES - 1])
        .expect("under the byte bound");
    assert!(out.push(vec![0; 2]).is_err(), "over the byte bound");
    out.push(vec![0; 1]).expect("exactly at the byte bound");
    assert_eq!(out.next().map(|f| f.len()), Some(MAX_OUTBOUND_BYTES - 1));
    assert_eq!(queued(&out), (1, 1), "popping releases its bytes");

    out.close();
    out.push(vec![1; 4])
        .expect("a closed queue accepts and drops");
    assert_eq!(
        out.next(),
        Some(vec![0]),
        "frames queued before close still drain"
    );
    assert_eq!(out.next(), None);
}

#[test]
fn close_with_replaces_the_backlog_with_one_final_frame() {
    let out = Outbound::new();
    out.push(b"first".to_vec()).expect("push");
    out.push(b"second".to_vec()).expect("push");
    out.close_with(Some(b"bye".to_vec()));
    assert_eq!(queued(&out), (1, 3));
    assert_eq!(out.next(), Some(b"bye".to_vec()));
    assert_eq!(out.next(), None);

    let out = Outbound::new();
    out.push(b"kept".to_vec()).expect("push");
    out.close_with(None);
    assert_eq!(out.next(), Some(b"kept".to_vec()));
    assert_eq!(out.next(), None);
}

#[test]
fn a_dead_link_discards_its_queue_and_wakes_the_writer() {
    let out = Outbound::new();
    out.push(b"lost".to_vec()).expect("push");
    out.mark_dead();
    assert_eq!(queued(&out), (0, 0));
    out.push(b"ignored".to_vec())
        .expect("dead links accept and drop");
    assert_eq!(out.next(), None);

    let waiting = Outbound::new();
    let writer = waiting.clone();
    let reader = thread::spawn(move || writer.next());
    thread::sleep(Duration::from_millis(20));
    waiting.push(b"late".to_vec()).expect("push");
    assert_eq!(reader.join().expect("reader"), Some(b"late".to_vec()));
}

#[test]
fn capabilities_are_fresh_private_hex_secrets() {
    let dir = TempDir::new("capability");
    let first = write_capability(&dir.0).expect("write");
    let path = dir.0.join(CONTROL_CAPABILITY_NAME);
    assert_eq!(first.len(), 64);
    assert!(first.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(fs::read_to_string(&path).expect("read"), first);
    let meta = fs::metadata(&path).expect("metadata");
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    assert_eq!(meta.uid(), service_uid());

    fs::write(
        dir.0.join(format!("{CONTROL_CAPABILITY_NAME}.tmp")),
        "stale",
    )
    .expect("stale");
    let second = write_capability(&dir.0).expect("rewrite over a stale temporary");
    assert_ne!(first, second);
    assert_eq!(fs::read_to_string(&path).expect("read"), second);
    assert!(
        !dir.0
            .join(format!("{CONTROL_CAPABILITY_NAME}.tmp"))
            .exists()
    );
}

#[test]
fn binding_replaces_a_stale_socket_but_refuses_a_live_one_or_a_file() {
    let dir = TempDir::new("bind");
    let socket = dir.0.join(CONTROL_SOCKET_NAME);
    ensure_not_running(&dir.0).expect("nothing there yet");
    let live = bind_socket(&dir.0).expect("bind");
    assert_eq!(
        fs::metadata(&socket).expect("socket").permissions().mode() & 0o777,
        0o600
    );
    assert!(matches!(
        ensure_not_running(&dir.0),
        Err(TransportError::AlreadyRunning)
    ));
    assert!(matches!(
        bind_socket(&dir.0),
        Err(TransportError::AlreadyRunning)
    ));
    drop(live);
    ensure_not_running(&dir.0).expect("a socket nobody answers on is stale");
    drop(bind_socket(&dir.0).expect("rebinds over the stale socket"));

    fs::remove_file(&socket).expect("remove");
    fs::write(&socket, "not a socket").expect("file");
    assert!(matches!(
        bind_socket(&dir.0),
        Err(TransportError::UnsafeSocketPath)
    ));
    ensure_not_running(&dir.0).expect("a regular file is not a running service");
}

struct Served {
    _dir: TempDir,
    socket: PathBuf,
    capability: String,
    listener: Listener,
    inbound: Receiver<Inbound>,
}

fn served(tag: &str) -> Served {
    let dir = TempDir::new(tag);
    let capability = write_capability(&dir.0).expect("capability");
    let socket = dir.0.join(CONTROL_SOCKET_NAME);
    let (tx, inbound) = mpsc::sync_channel(16);
    let listener = serve(
        bind_socket(&dir.0).expect("bind"),
        socket.clone(),
        capability.clone(),
        tx,
    )
    .expect("serve");
    Served {
        _dir: dir,
        socket,
        capability,
        listener,
        inbound,
    }
}

fn hello(socket: &Path) -> (FrameWriter<UnixStream>, FrameReader<UnixStream>) {
    let stream = UnixStream::connect(socket).expect("connect");
    stream.set_read_timeout(Some(WAIT)).expect("timeout");
    let mut reader = FrameReader::new(stream.try_clone().expect("clone"), Framing::Client);
    let mut writer = FrameWriter::new(stream, Framing::Client);
    writer
        .write(&ClientEnvelope::new(ClientBody::Hello))
        .expect("hello");
    let reply: ClientEnvelope = reader.read().expect("read").expect("hello back");
    assert!(matches!(reply.body, ClientBody::Hello));
    (writer, reader)
}

fn auth(writer: &mut FrameWriter<UnixStream>, capability: &str) {
    writer
        .write(&ClientEnvelope::new(ClientBody::Auth {
            capability: Capability::parse(capability).expect("capability"),
        }))
        .expect("auth");
}

#[test]
fn admitted_links_attach_forward_requests_and_detach() {
    let mut served = served("admit");
    let (mut writer, mut reader) = hello(&served.socket);
    auth(&mut writer, &served.capability);
    let Ok(Inbound::Attach { conn, out }) = served.inbound.recv_timeout(WAIT) else {
        panic!("expected attach");
    };

    let welcome = ClientEnvelope::new(ClientBody::Welcome {
        service_epoch: Id::from_random_bytes([1; 16]),
        lease_id: Id::from_random_bytes([2; 16]),
    });
    out.push(encode(&welcome, Framing::Client).expect("encode"))
        .expect("push");
    let received: ClientEnvelope = reader.read().expect("read").expect("frame");
    assert!(matches!(received.body, ClientBody::Welcome { .. }));

    let request_id = Id::from_random_bytes([3; 16]);
    let frame =
        RequestFrame::new(request_id.clone(), None, Request::Snapshot(Empty {})).expect("frame");
    writer
        .write(&ClientEnvelope::new(ClientBody::Request(frame)))
        .expect("request");
    match served.inbound.recv_timeout(WAIT) {
        Ok(Inbound::Request { conn: from, frame }) => {
            assert_eq!(from, conn);
            assert_eq!(frame.request_id, request_id);
        }
        other => panic!("expected request, got {other:?}"),
    }

    drop(writer);
    drop(reader);
    assert!(matches!(
        served.inbound.recv_timeout(WAIT),
        Ok(Inbound::Detach { conn: from }) if from == conn
    ));
    served.listener.stop();
    served.listener.drain(WAIT);
}

#[test]
fn a_wrong_capability_or_a_missing_hello_is_never_admitted() {
    let mut served = served("refuse");
    let (mut writer, mut reader) = hello(&served.socket);
    auth(&mut writer, &"0".repeat(64));
    assert!(
        matches!(reader.read::<ClientEnvelope>(), Ok(None) | Err(_)),
        "the link is closed without a reply"
    );

    let stream = UnixStream::connect(&served.socket).expect("connect");
    stream.set_read_timeout(Some(WAIT)).expect("timeout");
    let mut writer = FrameWriter::new(stream.try_clone().expect("clone"), Framing::Client);
    auth(&mut writer, &served.capability);
    let mut reader = FrameReader::new(stream, Framing::Client);
    assert!(matches!(reader.read::<ClientEnvelope>(), Ok(None) | Err(_)));

    assert!(
        served.inbound.try_recv().is_err(),
        "neither link reached the service"
    );
    served.listener.stop();
}
