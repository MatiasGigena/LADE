mod service_support;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use lade_protocol::client::{
    ClientBody, ClientEnvelope, Empty, ErrorCode, Event, ReplyData, Request, RequestFrame,
};
use lade_protocol::types::Capability;
use service_support::{Client, Env, FakeLauncher, new_id, raw_connect, raw_write};

#[test]
fn long_runtime_dir_is_refused_with_a_clear_error_and_long_state_dir_is_fine() {
    let env = Env::new("longpaths");
    let long_state = env.root.path().join("s".repeat(200));
    let config = lade_system::service::ServiceConfig {
        state_dir: long_state.clone(),
        runtime_dir: env.root.path().join("run"),
        ..env.config()
    };
    let running = lade_system::service::start(config, FakeLauncher::shared()).expect("starts");
    running.shutdown();
    let long_runtime = env.root.path().join("r".repeat(100));
    let config = lade_system::service::ServiceConfig {
        state_dir: long_state,
        runtime_dir: long_runtime.clone(),
        ..env.config()
    };
    let Err(error) = lade_system::service::start(config, FakeLauncher::shared()) else {
        panic!("service started with an unbindable socket path");
    };
    assert!(
        matches!(
            error,
            lade_system::service::StartError::SocketPathTooLong(_)
        ),
        "{error:?}"
    );
    assert!(
        error.to_string().contains("shorter --runtime-dir"),
        "{error}"
    );
    assert!(!long_runtime.exists());
}

#[test]
fn handshake_snapshot_and_files_are_private() {
    let env = Env::new("svc-handshake");
    let service = env.start(FakeLauncher::shared());
    let run = env.root.path().join("run");
    assert_eq!(
        fs::metadata(&run).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(run.join("control.cap"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(run.join("control.sock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let capability = env.capability();
    assert_eq!(capability.len(), 64);
    let mut client = env.connect();
    assert_eq!(client.service_epoch.as_str(), service.service_epoch());
    match client.call(Request::Snapshot(Empty {})) {
        Ok(ReplyData::Snapshot(snapshot)) => {
            assert_eq!(snapshot.service_epoch.as_str(), service.service_epoch());
            assert!(snapshot.objectives.is_empty());
        }
        other => panic!("snapshot: {other:?}"),
    }
    drop(client);
    service.shutdown();
    assert!(!run.join("control.sock").exists());
}

#[test]
fn wrong_capability_and_pre_auth_frames_close_the_link() {
    let env = Env::new("svc-wrongcap");
    let service = env.start(FakeLauncher::shared());
    let wrong = "0".repeat(64);
    let error = Client::connect(&env.socket(), &wrong)
        .err()
        .expect("refused");
    assert!(error.contains("welcome"), "{error}");

    let (mut writer, frames) = raw_connect(&env.socket());
    writer
        .write(&ClientEnvelope::new(ClientBody::Hello))
        .unwrap();
    assert!(matches!(
        frames.recv_timeout(service_support::TIMEOUT),
        Ok(ClientBody::Hello)
    ));
    let frame = RequestFrame::new(new_id(), None, Request::Snapshot(Empty {})).unwrap();
    let _ = writer.write(&ClientEnvelope::new(ClientBody::Request(frame)));
    assert!(frames.recv_timeout(service_support::TIMEOUT).is_err());

    let (mut writer, frames) = raw_connect(&env.socket());
    let auth = ClientBody::Auth {
        capability: Capability::parse(env.capability()).unwrap(),
    };
    writer.write(&ClientEnvelope::new(auth)).unwrap();
    assert!(frames.recv_timeout(service_support::TIMEOUT).is_err());

    let stream = raw_write(&env.socket(), &[0xff, 0xff, 0xff, 0xff]);
    drop(stream);

    let mut client = env.connect();
    assert!(client.call(Request::Snapshot(Empty {})).is_ok());
    service.shutdown();
}

#[test]
fn second_client_is_refused_and_lease_frees_on_disconnect() {
    let env = Env::new("svc-lease");
    let service = env.start(FakeLauncher::shared());
    let mut first = env.connect();
    let refused = Client::connect(&env.socket(), &env.capability())
        .err()
        .expect("second client refused");
    assert!(refused.contains("unauthorized"), "{refused}");
    assert!(first.call(Request::Snapshot(Empty {})).is_ok());
    drop(first);
    let mut again = service_support::eventually("lease released", || {
        Client::connect(&env.socket(), &env.capability()).ok()
    });
    match again.call(Request::Snapshot(Empty {})) {
        Ok(ReplyData::Snapshot(snapshot)) => {
            assert_eq!(snapshot.service_epoch.as_str(), service.service_epoch());
        }
        other => panic!("snapshot: {other:?}"),
    }
    service.shutdown();
}

#[test]
fn invalid_request_keeps_the_link_and_restart_rotates_admission() {
    let env = Env::new("svc-invalid");
    let service = env.start(FakeLauncher::shared());
    let old_capability = env.capability();
    let old_epoch = service.service_epoch().to_string();
    let mut client = env.connect();
    let request_id = new_id();
    let frame = serde_json_frame(&request_id);
    client.write_raw(&frame);
    let reply = client.wait_reply(&request_id);
    match service_support::result(reply) {
        Err(rejection) => assert_eq!(rejection.code, ErrorCode::InvalidRequest),
        Ok(data) => panic!("accepted invalid request: {data:?}"),
    }
    assert!(client.call(Request::Snapshot(Empty {})).is_ok());
    drop(client);
    service.shutdown();

    let service = env.start(FakeLauncher::shared());
    assert_ne!(env.capability(), old_capability);
    assert_ne!(service.service_epoch(), old_epoch);
    assert!(Client::connect(&env.socket(), &old_capability).is_err());
    let client = env.connect();
    let notice = client
        .events
        .iter()
        .any(|e| matches!(e.event, Event::ServiceNotice(_)));
    assert!(!notice);
    service.shutdown();
}

#[test]
fn second_service_on_the_same_runtime_dir_leaves_admission_intact() {
    let env = Env::new("svc-second");
    let service = env.start(FakeLauncher::shared());
    let capability = env.capability();
    let gateways = env.root.path().join("run/g");
    fs::create_dir_all(&gateways).unwrap();
    fs::set_permissions(&gateways, fs::Permissions::from_mode(0o700)).unwrap();
    let gateway = gateways.join("0123456789ab-1.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&gateway).unwrap();
    let config = lade_system::service::ServiceConfig {
        state_dir: env.root.path().join("other-state"),
        ..env.config()
    };
    let Err(error) = lade_system::service::start(config, FakeLauncher::shared()) else {
        panic!("a second service started on the same runtime directory");
    };
    assert!(
        matches!(
            error,
            lade_system::service::StartError::Transport(
                lade_system::transport::TransportError::AlreadyRunning
            )
        ),
        "{error:?}"
    );
    assert_eq!(env.capability(), capability, "the capability was replaced");
    assert!(
        gateway.exists(),
        "a running worker's gateway socket was swept"
    );
    let mut client = env.connect();
    assert!(client.call(Request::Snapshot(Empty {})).is_ok());
    drop(client);
    service.shutdown();
}

fn serde_json_frame(request_id: &lade_protocol::types::Id) -> Vec<u8> {
    let body = format!(
        r#"{{"protocol":1,"bundle":"{}","body":{{"kind":"request","request_id":"{}","operation_id":"{}","name":"snapshot","payload":{{}}}}}}"#,
        lade_protocol::BUNDLE,
        request_id,
        new_id()
    );
    let mut frame = (body.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(body.as_bytes());
    frame
}
