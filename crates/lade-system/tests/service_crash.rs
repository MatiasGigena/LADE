mod service_support;

use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use lade_protocol::client::{
    ApprovalState, Badge, Basis, DeliveryState, ExecutionState, InputSummary, InputsListQuery,
    Lifecycle, MessageSendCommand, ObjectiveCleanupCommand, ObjectiveSelector, OperationGetQuery,
    OperationState, OperationStatus, ReplyData, Request, RequestFrame, ReviewApproveCommand,
    ReviewIntegrateCommand, ReviewVerifyCommand, TerminalInputCommand, TerminalOpenCommand,
    VerificationState,
};
use lade_protocol::types::{Counter, DeliveryMode, Id, PageLimit};
use service_support::*;

fn send_op(client: &mut Client, request: Request) -> Id {
    let operation = new_id();
    client.send_frame(RequestFrame::new(new_id(), Some(operation.clone()), request).unwrap());
    operation
}

fn crashed(daemon: &mut Daemon, mut client: Client, point: &str) {
    let status = daemon.wait_exit();
    assert_eq!(
        status.signal(),
        Some(6),
        "{point}: {status} {}",
        daemon.log_text()
    );
    assert!(client.closed(), "{point}: link still open");
}

fn operation(client: &mut Client, op: &Id) -> OperationStatus {
    match client.call(Request::OperationGet(OperationGetQuery {
        operation_id: op.clone(),
    })) {
        Ok(ReplyData::OperationGet(status)) => status,
        other => panic!("operation.get: {other:?}"),
    }
}

fn message(text: &str, objective: &Id) -> Request {
    Request::MessageSend(MessageSendCommand {
        objective_id: objective.clone(),
        text: text.into(),
        mode: DeliveryMode::Prompt,
    })
}

fn inputs(client: &mut Client, objective: &Id) -> Vec<InputSummary> {
    match client.call(Request::InputsList(InputsListQuery {
        objective_id: objective.clone(),
        limit: PageLimit::parse("200").unwrap(),
        after: None,
    })) {
        Ok(ReplyData::InputsList(list)) => list.inputs,
        other => panic!("inputs.list: {other:?}"),
    }
}

fn resume(client: &mut Client, objective: &Id) {
    match client.call(Request::ExecutionResume(ObjectiveSelector {
        objective_id: objective.clone(),
    })) {
        Ok(ReplyData::ExecutionResume(_)) => {}
        other => panic!("execution.resume: {other:?}"),
    }
    wait_state(client, objective, ExecutionState::Idle);
}

fn wait_path(path: &Path) {
    eventually(&format!("{} exists", path.display()), || {
        path.exists().then_some(())
    });
}

fn approve(client: &mut Client, objective: &Id) -> Id {
    let current = detail(client, objective);
    let candidate = current
        .candidates
        .iter()
        .find(|c| c.status == lade_protocol::client::CandidateStatus::Current)
        .expect("current candidate")
        .candidate_id
        .clone();
    match client.call(Request::ReviewApprove(ReviewApproveCommand {
        objective_id: objective.clone(),
        candidate_id: candidate,
        target_commit: current.integration_target.commit.clone(),
        override_reason: Some("crash-window test".into()),
    })) {
        Ok(ReplyData::ReviewApprove(approval)) => approval.approval_id,
        other => panic!("review.approve: {other:?}"),
    }
}

fn integrate_request(objective: &Id, approval: Id) -> Request {
    Request::ReviewIntegrate(ReviewIntegrateCommand {
        objective_id: objective.clone(),
        approval_id: approval,
    })
}

fn refs(repo: &Path, prefix: &str) -> Vec<String> {
    git(repo, &["for-each-ref", "--format=%(refname)", prefix])
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn review_path_recovers_at_every_crash_window() {
    if !sandbox_works() {
        eprintln!("skipping: bubblewrap sandbox unavailable");
        return;
    }
    let env = Env::new("crash-review");
    let main_before = git(&env.repo, &["rev-parse", "main"]);

    let mut daemon = env.spawn_laded(Some("capture.retained"), &[]);
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "Crash review").objective_id;
    start_execution(&mut client, &objective);
    wait_state(&mut client, &objective, ExecutionState::Idle);
    send_op(
        &mut client,
        message("lade-fixture: write a.txt A", &objective),
    );
    crashed(&mut daemon, client, "capture.retained");
    let orphans = refs(&env.repo, "refs/lade/retain/");
    assert_eq!(orphans.len(), 1, "{orphans:?}");

    let mut daemon = env.spawn_laded(Some("freeze.branch_moved"), &[]);
    let mut client = env.connect();
    let recovered = detail(&mut client, &objective);
    assert!(
        recovered.badges.contains(&Badge::Interrupted),
        "{recovered:?}"
    );
    assert_eq!(recovered.lifecycle, Lifecycle::Active);
    resume(&mut client, &objective);
    let wt = worktree(&env, &objective);
    if !wt.join("a.txt").exists() {
        client
            .call(message("lade-fixture: write a.txt A", &objective))
            .unwrap();
        wait_path(&wt.join("a.txt"));
    }
    wait_state(&mut client, &objective, ExecutionState::Idle);
    let list = changes(&mut client, &objective, Basis::Working);
    assert_eq!(list.files.len(), 1);
    let capture = list.capture_id.clone().unwrap();
    assert!(
        !orphans[0].ends_with(capture.as_str()),
        "a fresh capture is recorded next to the orphan ref"
    );
    let branch = recovered.branch.clone();
    let freeze_op = send_op(
        &mut client,
        Request::ReviewFreeze(ObjectiveSelector {
            objective_id: objective.clone(),
        }),
    );
    crashed(&mut daemon, client, "freeze.branch_moved");
    let moved = git(&env.repo, &["rev-parse", &format!("refs/heads/{branch}")]);
    assert_ne!(moved, recovered.start_commit.hex());

    let mut daemon = env.spawn_laded(Some("freeze.recorded"), &[]);
    let mut client = env.connect();
    let status = operation(&mut client, &freeze_op);
    assert_eq!(status.state, OperationState::Uncertain, "{status:?}");
    let after = detail(&mut client, &objective);
    assert_eq!(after.lifecycle, Lifecycle::Active);
    assert!(after.candidates.is_empty());
    let freeze_op = send_op(
        &mut client,
        Request::ReviewFreeze(ObjectiveSelector {
            objective_id: objective.clone(),
        }),
    );
    crashed(&mut daemon, client, "freeze.recorded");

    let mut daemon = env.spawn_laded(Some("verify.copy"), &[]);
    let mut client = env.connect();
    let status = operation(&mut client, &freeze_op);
    assert_eq!(status.state, OperationState::Completed, "{status:?}");
    assert!(matches!(
        status.outcome,
        Some(lade_protocol::client::Outcome::Ok(_))
    ));
    let frozen = detail(&mut client, &objective);
    assert_eq!(frozen.lifecycle, Lifecycle::Frozen);
    let current = frozen
        .candidates
        .iter()
        .find(|c| c.status == lade_protocol::client::CandidateStatus::Current)
        .expect("candidate")
        .clone();
    assert_eq!(
        git(&env.repo, &["rev-parse", &format!("refs/heads/{branch}")]),
        current.commit.hex()
    );
    match client.call(Request::ReviewVerify(ReviewVerifyCommand {
        objective_id: objective.clone(),
        candidate_id: current.candidate_id.clone(),
        command: vec!["sh".into(), "-c".into(), "sleep 30".into()],
    })) {
        Ok(ReplyData::ReviewVerify(v)) => assert_eq!(v.state, VerificationState::Running),
        other => panic!("review.verify: {other:?}"),
    }
    crashed(&mut daemon, client, "verify.copy");
    assert_eq!(refs(&env.repo, "refs/heads/lade/verify/").len(), 1);

    let mut daemon = env.spawn_laded(Some("integrate.intent"), &[]);
    let mut client = env.connect();
    let verified = detail(&mut client, &objective);
    assert_eq!(
        verified.verifications[0].state,
        VerificationState::Interrupted
    );
    assert!(refs(&env.repo, "refs/heads/lade/verify/").is_empty());
    assert_eq!(
        fs::read_dir(env.root.path().join("state/verify"))
            .unwrap()
            .count(),
        0
    );
    let approval = approve(&mut client, &objective);
    let intent_op = send_op(&mut client, integrate_request(&objective, approval));
    crashed(&mut daemon, client, "integrate.intent");
    assert_eq!(git(&env.repo, &["rev-parse", "main"]), main_before);

    let mut daemon = env.spawn_laded(Some("integrate.ref_moved"), &[]);
    let mut client = env.connect();
    assert_eq!(
        operation(&mut client, &intent_op).state,
        OperationState::Uncertain
    );
    let aborted = detail(&mut client, &objective);
    assert_eq!(
        aborted.lifecycle,
        Lifecycle::Frozen,
        "restart expires the approval"
    );
    assert_eq!(
        aborted.approval.as_ref().unwrap().state,
        ApprovalState::Invalidated
    );
    assert_eq!(git(&env.repo, &["rev-parse", "main"]), main_before);
    assert!(!env.repo.join("a.txt").exists());
    let approval = approve(&mut client, &objective);
    send_op(&mut client, integrate_request(&objective, approval));
    crashed(&mut daemon, client, "integrate.ref_moved");
    assert_ne!(git(&env.repo, &["rev-parse", "main"]), main_before);
    assert!(
        !env.repo.join("a.txt").exists(),
        "the crash left the checkout behind its ref"
    );

    let mut daemon = env.spawn_laded(Some("cleanup.removed"), &[]);
    let mut client = env.connect();
    let integrated = detail(&mut client, &objective);
    assert_eq!(integrated.lifecycle, Lifecycle::Integrated);
    assert_eq!(fs::read_to_string(env.repo.join("a.txt")).unwrap(), "A");
    assert!(
        git(&env.repo, &["status", "--porcelain"]).is_empty(),
        "{}",
        git(&env.repo, &["status", "--porcelain"])
    );
    assert_eq!(git(&env.repo, &["rev-parse", "main^"]), main_before);
    send_op(
        &mut client,
        Request::ObjectiveCleanup(ObjectiveCleanupCommand {
            objective_id: objective.clone(),
            confirm_data_loss: false,
        }),
    );
    crashed(&mut daemon, client, "cleanup.removed");
    assert!(!wt.exists());

    let _daemon = env.spawn_laded(None, &[]);
    let mut client = env.connect();
    let before = detail(&mut client, &objective);
    assert!(!before.badges.contains(&Badge::CleanedUp));
    match client.call(Request::ObjectiveCleanup(ObjectiveCleanupCommand {
        objective_id: objective.clone(),
        confirm_data_loss: false,
    })) {
        Ok(ReplyData::ObjectiveCleanup(d)) => assert!(d.badges.contains(&Badge::CleanedUp)),
        other => panic!("cleanup: {other:?}"),
    }
    assert!(git(&env.repo, &["branch", "--list", &branch]).is_empty());
    assert!(!refs(&env.repo, "refs/lade/retain/").is_empty());
}

#[test]
fn input_and_launch_windows_recover_without_replay() {
    if !sandbox_works() {
        eprintln!("skipping: bubblewrap sandbox unavailable");
        return;
    }
    let env = Env::new("crash-input");

    let mut daemon = env.spawn_laded(Some("launch.provisioned"), &[]);
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "Crash input").objective_id;
    send_op(
        &mut client,
        Request::ExecutionStart(lade_protocol::client::ExecutionStartCommand {
            objective_id: objective.clone(),
            model: lade_protocol::types::Model {
                provider: "lade-fixture".into(),
                id: "scripted".into(),
            },
            thinking: None,
        }),
    );
    crashed(&mut daemon, client, "launch.provisioned");
    let wt = worktree(&env, &objective);
    assert!(wt.is_dir());

    let mut daemon = env.spawn_laded(Some("dispatch.recorded"), &[]);
    let mut client = env.connect();
    let recovered = detail(&mut client, &objective);
    assert!(
        recovered.badges.contains(&Badge::Interrupted),
        "{recovered:?}"
    );
    resume(&mut client, &objective);
    assert_eq!(
        refs(&env.repo, "refs/heads/lade/").len(),
        1,
        "the unrecorded worktree was adopted, not duplicated"
    );
    let dispatched_op = send_op(
        &mut client,
        message("lade-fixture: write b.txt B", &objective),
    );
    crashed(&mut daemon, client, "dispatch.recorded");

    let mut daemon = env.spawn_laded(Some("input.accepted"), &[]);
    let mut client = env.connect();
    assert_eq!(
        operation(&mut client, &dispatched_op).state,
        OperationState::Completed
    );
    let listed = inputs(&mut client, &objective);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].delivery, DeliveryState::Uncertain);
    assert!(
        detail(&mut client, &objective)
            .badges
            .contains(&Badge::DeliveryUncertain)
    );
    resume(&mut client, &objective);
    let accepted_op = send_op(
        &mut client,
        message("lade-fixture: write c.txt C", &objective),
    );
    crashed(&mut daemon, client, "input.accepted");

    let mut daemon = env.spawn_laded(Some("integrate.applied"), &[]);
    let mut client = env.connect();
    let status = operation(&mut client, &accepted_op);
    assert_eq!(status.state, OperationState::Completed, "{status:?}");
    let listed = inputs(&mut client, &objective);
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].delivery, DeliveryState::Uncertain);
    assert!(
        matches!(
            listed[1].delivery,
            DeliveryState::Queued | DeliveryState::Blocked
        ),
        "{:?}",
        listed[1]
    );
    assert!(!wt.join("c.txt").exists());
    resume(&mut client, &objective);
    wait_path(&wt.join("c.txt"));
    wait_state(&mut client, &objective, ExecutionState::Idle);
    // A later input completes only after any resend of the uncertain one would have.
    send_op(
        &mut client,
        message("lade-fixture: write d.txt D", &objective),
    );
    wait_path(&wt.join("d.txt"));
    wait_state(&mut client, &objective, ExecutionState::Idle);
    assert!(
        !wt.join("b.txt").exists(),
        "uncertain input is never resent"
    );
    assert_eq!(
        inputs(&mut client, &objective)[0].delivery,
        DeliveryState::Uncertain
    );
    match client.call(Request::ReviewFreeze(ObjectiveSelector {
        objective_id: objective.clone(),
    })) {
        Ok(ReplyData::ReviewFreeze(_)) => {}
        other => panic!("review.freeze: {other:?}"),
    }
    let approval = approve(&mut client, &objective);
    let integrate_op = send_op(&mut client, integrate_request(&objective, approval));
    crashed(&mut daemon, client, "integrate.applied");

    let _daemon = env.spawn_laded(None, &[]);
    let mut client = env.connect();
    let done = detail(&mut client, &objective);
    assert_eq!(done.lifecycle, Lifecycle::Integrated);
    assert_eq!(
        operation(&mut client, &integrate_op).state,
        OperationState::Uncertain,
        "the lost reply stays uncertain; the objective shows the outcome"
    );
    assert_eq!(fs::read_to_string(env.repo.join("c.txt")).unwrap(), "C");
    assert!(git(&env.repo, &["status", "--porcelain"]).is_empty());
}

fn sleepers(marker: &str) -> usize {
    processes_mentioning(&format!("sleep\0{marker}"))
        .into_iter()
        .filter(|pid| fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c == "sleep\n"))
        .count()
}

#[test]
fn killing_the_service_ends_four_executions_and_their_terminals() {
    if !sandbox_works() {
        eprintln!("skipping: bubblewrap sandbox unavailable");
        return;
    }
    let env = Env::new("crash-kill4");
    let mut daemon = env.spawn_laded(None, &[]);
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let mut objectives = Vec::new();
    for n in 0..4 {
        let objective =
            create_objective(&mut client, &ws.workspace_id, &format!("Kill {n}")).objective_id;
        start_execution(&mut client, &objective);
        objectives.push(objective);
    }
    for objective in &objectives {
        wait_state(&mut client, objective, ExecutionState::Idle);
    }
    let marker = format!("{}", 31_000 + std::process::id() % 1000);
    for objective in &objectives {
        let terminal = match client.call(Request::TerminalOpen(TerminalOpenCommand {
            objective_id: objective.clone(),
            cols: Counter::new(80),
            rows: Counter::new(24),
        })) {
            Ok(ReplyData::TerminalOpen(info)) => info.terminal_id,
            other => panic!("terminal.open: {other:?}"),
        };
        client
            .call(Request::TerminalInput(TerminalInputCommand {
                terminal_id: terminal,
                data: format!("setsid nohup sleep {marker} >/dev/null 2>&1 &\n"),
            }))
            .unwrap();
    }
    eventually("four detached sleepers", || {
        (sleepers(&marker) == 4).then_some(())
    });
    let state = env.root.path().join("state");
    let state = state.to_str().unwrap();
    assert!(
        processes_mentioning(state).len() >= 8,
        "sandboxes are running"
    );

    daemon.kill();
    drop(client);
    let deadline = Instant::now() + Duration::from_secs(15);
    while (sleepers(&marker) > 0 || !processes_mentioning(state).is_empty())
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(sleepers(&marker), 0, "no terminal descendant survives");
    assert_eq!(
        processes_mentioning(state),
        Vec::<u32>::new(),
        "no worker or terminal sandbox survives the service"
    );

    let daemon = env.spawn_laded(None, &[]);
    let mut client = env.connect();
    for objective in &objectives {
        let d = detail(&mut client, objective);
        assert_eq!(d.execution.state, ExecutionState::Cancelled, "{d:?}");
        assert!(d.badges.contains(&Badge::Interrupted));
    }
    assert_eq!(processes_mentioning(state), vec![daemon.pid()]);
}
