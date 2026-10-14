mod service_support;

use std::fs;

use lade_protocol::client::{ActionName, Badge, Event, ExecutionState, NoticeLevel};
use service_support::*;

#[test]
fn exceeding_writable_storage_stops_the_agent_with_limit_reached() {
    let env = Env::new("svc-storage");
    let config = lade_system::service::ServiceConfig {
        storage_limit_bytes: 256 << 10,
        ..env.config()
    };
    let service = lade_system::service::start(config, FakeLauncher::shared()).unwrap();
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "Fill the disk");
    let id = objective.objective_id.clone();
    start_execution(&mut client, &id);
    wait_state(&mut client, &id, ExecutionState::Idle);
    fs::write(worktree(&env, &id).join("small.txt"), vec![b'a'; 4096]).unwrap();
    fs::write(worktree(&env, &id).join("big.bin"), vec![7u8; 1 << 20]).unwrap();

    let failed = wait_state(&mut client, &id, ExecutionState::Failed);
    let reason = failed.execution.reason.clone().unwrap_or_default();
    assert!(
        reason.starts_with("limit_reached: writable storage"),
        "{reason}"
    );
    assert!(reason.contains("exceeds the 256 KiB limit"), "{reason}");
    assert_eq!(failed.badges.first(), Some(&Badge::LimitReached));
    let notice = client.wait_event(
        "limit notice",
        |event| matches!(event, Event::ServiceNotice(n) if n.code == "limit_reached"),
    );
    let Event::ServiceNotice(notice) = notice else {
        unreachable!()
    };
    assert_eq!(notice.level, NoticeLevel::Error);
    assert!(action_enabled(&failed, ActionName::ExecutionResume));
    std::thread::sleep(std::time::Duration::from_secs(6));
    assert_eq!(
        detail(&mut client, &id).execution.state,
        ExecutionState::Failed
    );
    drop(client);
    service.shutdown();
}
