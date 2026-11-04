mod service_support;

use std::fs;
use std::io::Read;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use lade_protocol::client::{
    ActionName, ApprovalState, Badge, CandidateCommand, ClientBody, ClientEnvelope, Empty,
    ErrorCode, Event, ExecutionState, Lifecycle, MessageSendCommand, NoticeCode, ObjectiveDetail,
    ObjectiveSelector, Rejection, ReplyData, Request, RequestFrame, ReviewApproveCommand,
    ReviewIntegrateCommand, TerminalCloseReason, TerminalInputCommand, TerminalOpenCommand,
    TerminalSelector,
};
use lade_protocol::types::{Capability, Counter, DeliveryMode, Id, PageLimit};
use lade_protocol::{FrameReader, FrameWriter, Framing};
use lade_system::service::ResourceLimits;
use service_support::*;

fn send_message(client: &mut Client, objective: &Id, text: &str) -> Id {
    client.send(
        Request::MessageSend(MessageSendCommand {
            objective_id: objective.clone(),
            text: text.into(),
            mode: DeliveryMode::Prompt,
        }),
        true,
    )
}

fn freeze(client: &mut Client, objective: &Id) {
    match client.call(Request::ReviewFreeze(ObjectiveSelector {
        objective_id: objective.clone(),
    })) {
        Ok(ReplyData::ReviewFreeze(_)) => {}
        other => panic!("review.freeze: {other:?}"),
    }
}

fn current_candidate(detail: &ObjectiveDetail) -> Id {
    detail
        .candidates
        .iter()
        .find(|c| c.status == lade_protocol::client::CandidateStatus::Current)
        .expect("current candidate")
        .candidate_id
        .clone()
}

fn approve(client: &mut Client, objective: &Id) -> Id {
    let current = detail(client, objective);
    match client.call(Request::ReviewApprove(ReviewApproveCommand {
        objective_id: objective.clone(),
        candidate_id: current_candidate(&current),
        target_commit: current.integration_target.commit.clone(),
        override_reason: Some("concurrency test".into()),
    })) {
        Ok(ReplyData::ReviewApprove(approval)) => approval.approval_id,
        other => panic!("review.approve: {other:?}"),
    }
}

fn integrate_request(objective: &Id, approval: &Id) -> Request {
    Request::ReviewIntegrate(ReviewIntegrateCommand {
        objective_id: objective.clone(),
        approval_id: approval.clone(),
    })
}

fn refresh_and_integrate(client: &mut Client, objective: &Id) {
    let stale = detail(client, objective);
    match client.call(Request::ReviewRefresh(CandidateCommand {
        objective_id: objective.clone(),
        candidate_id: current_candidate(&stale),
    })) {
        Ok(ReplyData::ReviewRefresh(_)) => {}
        other => panic!("review.refresh: {other:?}"),
    }
    let approval = approve(client, objective);
    match client.call(integrate_request(objective, &approval)) {
        Ok(ReplyData::ReviewIntegrate(d)) => assert_eq!(d.lifecycle, Lifecycle::Integrated),
        other => panic!("review.integrate: {other:?}"),
    }
}

fn assert_stale(client: &mut Client, objective: &Id) {
    let d = detail(client, objective);
    assert_eq!(d.lifecycle, Lifecycle::Frozen, "{d:?}");
    assert_eq!(
        d.approval.as_ref().map(|a| a.state),
        Some(ApprovalState::Invalidated)
    );
    assert!(d.badges.contains(&Badge::StaleTarget), "{:?}", d.badges);
    assert!(!action_enabled(&d, ActionName::ReviewIntegrate));
    assert!(action_enabled(&d, ActionName::ReviewRefresh));
}

fn refs(repo: &Path, prefix: &str) -> Vec<String> {
    git(repo, &["for-each-ref", "--format=%(refname)", prefix])
        .lines()
        .map(str::to_string)
        .collect()
}

fn open_terminal(client: &mut Client, objective: &Id) -> Result<Id, Rejection> {
    client
        .call(Request::TerminalOpen(TerminalOpenCommand {
            objective_id: objective.clone(),
            cols: Counter::new(80),
            rows: Counter::new(24),
        }))
        .map(|data| match data {
            ReplyData::TerminalOpen(info) => info.terminal_id,
            other => panic!("terminal.open: {other:?}"),
        })
}

fn type_line(client: &mut Client, terminal: &Id, line: &str) {
    client
        .call(Request::TerminalInput(TerminalInputCommand {
            terminal_id: terminal.clone(),
            data: format!("{line}\n"),
        }))
        .unwrap();
}

fn output_until(client: &mut Client, terminal: &Id, needle: &str) -> String {
    let mut seen = String::new();
    while !seen.contains(needle) {
        if let Event::TerminalOutput(output) = client.wait_event(
            needle,
            |e| matches!(e, Event::TerminalOutput(o) if o.terminal_id == *terminal),
        ) {
            seen.push_str(&output.data);
        }
    }
    seen
}

#[test]
fn four_concurrent_objectives_stay_isolated_and_same_target_integrations_serialize() {
    let env = Env::new("conc-four");
    let second = env.root.path().join("second");
    fs::create_dir(&second).unwrap();
    let repo_b = init_repo(&second, "sha1");
    let (launcher, sandboxed) = best_launcher(&env);
    eprintln!("four objectives sandboxed: {sandboxed}");
    let service = env.start(launcher);
    let mut client = env.connect();
    let ws_a = open_workspace(&mut client, &env.repo);
    let ws_b = open_workspace(&mut client, &repo_b);
    let mut objectives = Vec::new();
    for n in 1..=4 {
        let ws = if n == 4 { &ws_b } else { &ws_a };
        objectives.push(
            create_objective(&mut client, &ws.workspace_id, &format!("Four {n}")).objective_id,
        );
    }
    for objective in &objectives {
        start_execution(&mut client, objective);
    }
    for objective in &objectives {
        wait_state(&mut client, objective, ExecutionState::Idle);
    }
    let trees: Vec<_> = objectives.iter().map(|o| worktree(&env, o)).collect();
    let sent: Vec<Id> = objectives
        .iter()
        .enumerate()
        .map(|(i, o)| {
            send_message(
                &mut client,
                o,
                &format!("lade-fixture: write f{i}.txt from-{i}"),
            )
        })
        .collect();
    for request in sent {
        result(client.wait_reply(&request)).unwrap();
    }
    if sandboxed {
        let escape = trees[1].join("escape.txt");
        let request = send_message(
            &mut client,
            &objectives[0],
            &format!("lade-fixture: write {} stolen", escape.display()),
        );
        result(client.wait_reply(&request)).unwrap();
    }
    for (i, tree) in trees.iter().enumerate() {
        let file = tree.join(format!("f{i}.txt"));
        eventually(&format!("{} written", file.display()), || {
            file.exists().then_some(())
        });
    }
    for objective in &objectives {
        wait_state(&mut client, objective, ExecutionState::Idle);
    }
    for (i, tree) in trees.iter().enumerate() {
        for j in 0..trees.len() {
            assert_eq!(
                tree.join(format!("f{j}.txt")).exists(),
                i == j,
                "f{j} in worktree {i}"
            );
        }
        assert!(!env.repo.join(format!("f{i}.txt")).exists());
        assert!(!repo_b.join(format!("f{i}.txt")).exists());
    }
    if sandboxed {
        assert!(
            !trees[1].join("escape.txt").exists(),
            "grant crossed Objectives"
        );
        let terminal = open_terminal(&mut client, &objectives[0]).unwrap();
        let sessions = env.root.path().join("state/sessions");
        type_line(
            &mut client,
            &terminal,
            &format!(
                "test -e {}; echo W_$?; test -e {}; echo S_$?; test -e {}/f0.txt; echo O_$?; echo END_$((3+4))",
                trees[1].display(),
                sessions.display(),
                trees[0].display()
            ),
        );
        let seen = output_until(&mut client, &terminal, "END_7");
        assert!(seen.contains("W_1"), "{seen}");
        assert!(seen.contains("S_1"), "{seen}");
        assert!(seen.contains("O_0"), "{seen}");
        client
            .call(Request::TerminalClose(TerminalSelector {
                terminal_id: terminal,
            }))
            .unwrap();
    }
    let details: Vec<ObjectiveDetail> = objectives.iter().map(|o| detail(&mut client, o)).collect();
    let mut branches: Vec<&str> = details.iter().map(|d| d.branch.as_str()).collect();
    branches.sort_unstable();
    branches.dedup();
    assert_eq!(branches.len(), 4, "one branch per Objective");
    assert_eq!(refs(&env.repo, "refs/heads/lade/").len(), 3);
    assert_eq!(refs(&repo_b, "refs/heads/lade/").len(), 1);
    for (d, tree) in details.iter().zip(&trees) {
        assert_eq!(
            git(tree, &["symbolic-ref", "HEAD"]),
            format!("refs/heads/{}", d.branch)
        );
    }
    assert!(
        refs(&repo_b, "refs/heads/")
            .iter()
            .all(|r| !details[..3].iter().any(|d| r.ends_with(&d.branch)))
    );

    for objective in &objectives {
        freeze(&mut client, objective);
    }
    for (i, d) in objectives
        .iter()
        .map(|o| detail(&mut client, o))
        .enumerate()
    {
        let candidate = current_candidate(&d);
        let (own, other) = if i == 3 {
            (&repo_b, &env.repo)
        } else {
            (&env.repo, &repo_b)
        };
        let name = format!("refs/lade/retain/candidate-{}", candidate.as_str());
        assert_eq!(refs(own, &name).len(), 1, "candidate ref in its repository");
        assert!(refs(other, &name).is_empty(), "candidate ref leaked");
    }
    let approvals: Vec<Id> = objectives.iter().map(|o| approve(&mut client, o)).collect();
    let main_before = git(&env.repo, &["rev-parse", "main"]);

    let first = client.send(integrate_request(&objectives[0], &approvals[0]), true);
    let second_req = client.send(integrate_request(&objectives[1], &approvals[1]), true);
    let other_repo = client.send(integrate_request(&objectives[3], &approvals[3]), true);
    let results = [
        result(client.wait_reply(&first)),
        result(client.wait_reply(&second_req)),
    ];
    let won: Vec<usize> = (0..2).filter(|i| results[*i].is_ok()).collect();
    assert_eq!(
        won.len(),
        1,
        "exactly one same-target integration wins: {results:?}"
    );
    let lost = 1 - won[0];
    let refused = results[lost].as_ref().unwrap_err();
    assert_eq!(refused.code, ErrorCode::StaleTarget, "{refused:?}");
    assert_stale(&mut client, &objectives[lost]);
    match result(client.wait_reply(&other_repo)) {
        Ok(ReplyData::ReviewIntegrate(d)) => assert_eq!(d.lifecycle, Lifecycle::Integrated),
        other => panic!("integration into repository B: {other:?}"),
    }
    assert_eq!(git(&env.repo, &["rev-parse", "main^"]), main_before);
    let stale = client
        .call(integrate_request(&objectives[2], &approvals[2]))
        .unwrap_err();
    assert_eq!(stale.code, ErrorCode::StaleTarget);
    assert_stale(&mut client, &objectives[2]);
    refresh_and_integrate(&mut client, &objectives[lost]);
    assert_stale(&mut client, &objectives[2]);
    refresh_and_integrate(&mut client, &objectives[2]);
    for i in 0..3 {
        assert_eq!(
            fs::read_to_string(env.repo.join(format!("f{i}.txt"))).unwrap(),
            format!("from-{i}")
        );
    }
    assert!(!env.repo.join("f3.txt").exists());
    assert_eq!(fs::read_to_string(repo_b.join("f3.txt")).unwrap(), "from-3");
    assert_eq!(git(&env.repo, &["rev-list", "--count", "main"]), "4");
    assert!(git(&env.repo, &["status", "--porcelain"]).is_empty());
    assert!(git(&repo_b, &["status", "--porcelain"]).is_empty());
    drop(client);
    service.shutdown();
}

#[test]
fn aggregate_worker_quota_is_configurable_visible_and_four_is_not_a_cap() {
    let env = Env::new("conc-quota");
    let service = env.start_limited(
        FakeLauncher::shared(),
        ResourceLimits {
            max_workers: 5,
            ..ResourceLimits::default()
        },
    );
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objectives: Vec<Id> = (0..6)
        .map(|n| {
            create_objective(&mut client, &ws.workspace_id, &format!("Quota {n}")).objective_id
        })
        .collect();
    for objective in &objectives[..5] {
        start_execution(&mut client, objective);
        wait_state(&mut client, objective, ExecutionState::Idle);
    }
    let sixth = detail(&mut client, &objectives[5]);
    let gate = sixth
        .available_actions
        .iter()
        .find(|a| a.action == ActionName::ExecutionStart)
        .unwrap();
    assert!(!gate.enabled);
    let reason = gate.reason.clone().unwrap();
    assert!(
        reason.contains("5 of 5 agents") && !reason.contains("--max-workers"),
        "{reason}"
    );
    let refused = client
        .call(Request::ExecutionStart(
            lade_protocol::client::ExecutionStartCommand {
                objective_id: objectives[5].clone(),
                model: lade_protocol::types::Model {
                    provider: "lade-fixture".into(),
                    id: "scripted".into(),
                },
                thinking: None,
            },
        ))
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::LimitReached);
    assert_eq!(refused.detail, reason);
    assert_eq!(
        detail(&mut client, &objectives[5]).execution.state,
        ExecutionState::None,
        "a refused start records nothing"
    );
    client
        .call(Request::ExecutionCancel(ObjectiveSelector {
            objective_id: objectives[0].clone(),
        }))
        .unwrap();
    eventually("cancelled", || {
        matches!(
            detail(&mut client, &objectives[0]).execution.state,
            ExecutionState::Cancelled
        )
        .then_some(())
    });
    start_execution(&mut client, &objectives[5]);
    let resume = client
        .call(Request::ExecutionResume(ObjectiveSelector {
            objective_id: objectives[0].clone(),
        }))
        .unwrap_err();
    assert_eq!(resume.code, ErrorCode::LimitReached, "{resume:?}");
    drop(client);
    service.shutdown();
}

#[test]
fn terminal_total_is_aggregate_across_objectives() {
    if !sandbox_works() {
        eprintln!("skipping: terminals need the sandbox");
        return;
    }
    let env = Env::new("conc-terms");
    let service = env.start_limited(
        FakeLauncher::shared(),
        ResourceLimits {
            max_terminals: 3,
            ..ResourceLimits::default()
        },
    );
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let a = create_objective(&mut client, &ws.workspace_id, "Terms A").objective_id;
    let b = create_objective(&mut client, &ws.workspace_id, "Terms B").objective_id;
    for objective in [&a, &b] {
        start_execution(&mut client, objective);
        wait_state(&mut client, objective, ExecutionState::Idle);
    }
    let first = open_terminal(&mut client, &a).unwrap();
    open_terminal(&mut client, &a).unwrap();
    open_terminal(&mut client, &b).unwrap();
    let refused = open_terminal(&mut client, &b).unwrap_err();
    assert_eq!(refused.code, ErrorCode::LimitReached);
    assert!(
        refused.detail.contains("3 of 3 in total"),
        "{}",
        refused.detail
    );
    client
        .call(Request::TerminalClose(TerminalSelector {
            terminal_id: first,
        }))
        .unwrap();
    open_terminal(&mut client, &b).unwrap();
    drop(client);
    service.shutdown();
}

#[test]
fn storage_pressure_warns_then_refuses_new_writers_and_keeps_history() {
    let env = Env::new("conc-storage");
    let service = env.start_limited(
        FakeLauncher::shared(),
        ResourceLimits {
            storage_warn_bytes: u64::MAX / 4,
            storage_reserve_bytes: 0,
            ..ResourceLimits::default()
        },
    );
    let mut client = env.connect();
    client.wait_event(
        "storage_low",
        |e| matches!(e, Event::ServiceNotice(n) if n.code == NoticeCode::StorageLow),
    );
    let ws = open_workspace(&mut client, &env.repo);
    let running = create_objective(&mut client, &ws.workspace_id, "Before pressure").objective_id;
    start_execution(&mut client, &running);
    wait_state(&mut client, &running, ExecutionState::Idle);
    drop(client);
    service.shutdown();

    let service = env.start_limited(
        FakeLauncher::shared(),
        ResourceLimits {
            storage_warn_bytes: u64::MAX / 4,
            storage_reserve_bytes: u64::MAX / 8,
            ..ResourceLimits::default()
        },
    );
    let mut client = env.connect();
    let notice = client.wait_event(
        "storage_critical",
        |e| matches!(e, Event::ServiceNotice(n) if n.code == NoticeCode::StorageCritical),
    );
    let Event::ServiceNotice(notice) = notice else {
        unreachable!()
    };
    assert!(notice.message.contains("reserve"), "{}", notice.message);
    let fresh = create_objective(&mut client, &ws.workspace_id, "Under pressure").objective_id;
    let gate = detail(&mut client, &fresh)
        .available_actions
        .into_iter()
        .find(|a| a.action == ActionName::ExecutionStart)
        .unwrap();
    assert!(!gate.enabled);
    assert!(gate.reason.as_deref().unwrap().contains("in reserve"));
    let refused = client
        .call(Request::ExecutionStart(
            lade_protocol::client::ExecutionStartCommand {
                objective_id: fresh.clone(),
                model: lade_protocol::types::Model {
                    provider: "lade-fixture".into(),
                    id: "scripted".into(),
                },
                thinking: None,
            },
        ))
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::LimitReached);
    assert!(refused.detail.contains("in reserve"), "{}", refused.detail);
    let resume = client
        .call(Request::ExecutionResume(ObjectiveSelector {
            objective_id: running.clone(),
        }))
        .unwrap_err();
    assert_eq!(resume.code, ErrorCode::LimitReached);
    match client.call(Request::MessageSend(MessageSendCommand {
        objective_id: running.clone(),
        text: "kept for later".into(),
        mode: DeliveryMode::FollowUp,
    })) {
        Ok(ReplyData::MessageSend(input)) => {
            assert_eq!(
                input.delivery,
                lade_protocol::client::DeliveryState::Blocked
            )
        }
        other => panic!("message.send: {other:?}"),
    }
    match client.call(Request::ActivityPage(
        lade_protocol::client::ActivityPageQuery {
            objective_id: running.clone(),
            limit: PageLimit::parse("50").unwrap(),
            before: None,
        },
    )) {
        Ok(ReplyData::ActivityPage(_)) => {}
        other => panic!("activity.page: {other:?}"),
    }
    drop(client);
    service.shutdown();

    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    start_execution(&mut client, &fresh);
    drop(client);
    service.shutdown();
}

fn sleepers(marker: &str) -> usize {
    processes_mentioning(&format!("sleep\0{marker}"))
        .into_iter()
        .filter(|pid| fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c == "sleep\n"))
        .count()
}

#[test]
fn client_crash_kills_four_terminals_while_four_agents_keep_running() {
    if !sandbox_works() {
        eprintln!("skipping: terminals need the sandbox");
        return;
    }
    let env = Env::new("conc-lease");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objectives: Vec<Id> = (0..4)
        .map(|n| {
            create_objective(&mut client, &ws.workspace_id, &format!("Lease {n}")).objective_id
        })
        .collect();
    for objective in &objectives {
        start_execution(&mut client, objective);
    }
    for objective in &objectives {
        wait_state(&mut client, objective, ExecutionState::Idle);
    }
    let marker = format!("{}", 32_000 + std::process::id() % 1000);
    for objective in &objectives {
        let terminal = open_terminal(&mut client, objective).unwrap();
        type_line(
            &mut client,
            &terminal,
            &format!("setsid nohup sleep {marker} >/dev/null 2>&1 &"),
        );
    }
    eventually("four detached sleepers", || {
        (sleepers(&marker) == 4).then_some(())
    });
    drop(client);
    eventually("terminals killed with the lease", || {
        (sleepers(&marker) == 0).then_some(())
    });
    let mut next = env.connect();
    for objective in &objectives {
        assert_eq!(
            detail(&mut next, objective).execution.state,
            ExecutionState::Idle
        );
    }
    let fresh = open_terminal(&mut next, &objectives[0]).unwrap();
    next.call(Request::TerminalClose(TerminalSelector {
        terminal_id: fresh.clone(),
    }))
    .unwrap();
    match next.wait_event(
        "closed",
        |e| matches!(e, Event::TerminalClosed(c) if c.terminal_id == fresh),
    ) {
        Event::TerminalClosed(c) => assert_eq!(c.reason, TerminalCloseReason::Closed),
        _ => unreachable!(),
    }
    drop(next);
    service.shutdown();
}

fn rss_kib() -> u64 {
    fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        .unwrap()
}

#[test]
fn a_client_that_stops_reading_is_dropped_and_the_next_one_resyncs() {
    let env = Env::new("conc-slow");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    for n in 0..40 {
        create_objective(
            &mut client,
            &ws.workspace_id,
            &format!("Slow client objective {n} {}", "x".repeat(80)),
        );
    }
    drop(client);

    let stream = UnixStream::connect(env.socket()).unwrap();
    let mut reader = FrameReader::new(stream.try_clone().unwrap(), Framing::Client);
    let mut writer = FrameWriter::new(stream.try_clone().unwrap(), Framing::Client);
    let admitted = eventually("slow client admitted", || {
        writer.write(&ClientEnvelope::new(ClientBody::Hello)).ok()?;
        reader.read::<ClientEnvelope>().ok()??;
        writer
            .write(&ClientEnvelope::new(ClientBody::Auth {
                capability: Capability::parse(env.capability().trim()).unwrap(),
            }))
            .ok()?;
        reader.read::<ClientEnvelope>().ok()?
    });
    assert!(
        matches!(admitted.body, ClientBody::Welcome { .. }),
        "{admitted:?}"
    );
    let before = rss_kib();
    for _ in 0..8000 {
        let query = Request::OperationGet(lade_protocol::client::OperationGetQuery {
            operation_id: new_id(),
        });
        let frame = RequestFrame::new(new_id(), None, query).unwrap();
        if writer
            .write(&ClientEnvelope::new(ClientBody::Request(frame)))
            .is_err()
        {
            break;
        }
    }
    let mut next = eventually("next client admitted", || {
        Client::connect(&env.socket(), &env.capability()).ok()
    });
    match next.call(Request::Snapshot(Empty {})) {
        Ok(ReplyData::Snapshot(snapshot)) => assert_eq!(snapshot.objectives.len(), 40),
        other => panic!("snapshot: {other:?}"),
    }
    let grown = rss_kib().saturating_sub(before);
    assert!(
        grown < 64 * 1024,
        "memory grew by {grown} KiB for a stalled client"
    );
    let mut raw = stream;
    raw.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let started = Instant::now();
    let mut total = 0usize;
    let mut buf = vec![0u8; 64 << 10];
    loop {
        match raw.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) => panic!("slow link not closed: {e} after {total} bytes"),
        }
        assert!(started.elapsed() < Duration::from_secs(60));
    }
    assert!(
        total < 16 << 20,
        "the slow link received {total} bytes; the queue bound is 8 MiB"
    );
    thread::sleep(Duration::from_millis(100));
    drop(next);
    service.shutdown();
}
