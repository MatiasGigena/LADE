#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code fails loudly on broken fixtures"
)]

mod service_support;

use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use lade_protocol::client::{
    ActionName, Basis, ErrorCode, Event, ExecutionState, ObjectiveSelector, ReplyData, Request,
    SafeAction, ServiceStopCommand, StopReason, TerminalCloseReason, TerminalInfo,
    TerminalInputCommand, TerminalMode, TerminalOpenCommand, TerminalResizeCommand,
    TerminalSelector,
};
use lade_protocol::types::{Counter, Id};
use service_support::*;

fn open_command(objective: &Id) -> Request {
    Request::TerminalOpen(TerminalOpenCommand {
        objective_id: objective.clone(),
        cols: Counter::new(80),
        rows: Counter::new(24),
    })
}

fn open(client: &mut Client, objective: &Id) -> TerminalInfo {
    match client.call(open_command(objective)) {
        Ok(ReplyData::TerminalOpen(info)) => info,
        other => panic!("terminal.open: {other:?}"),
    }
}

fn input(client: &mut Client, terminal: &Id, line: &str) {
    match client.call(Request::TerminalInput(TerminalInputCommand {
        terminal_id: terminal.clone(),
        data: format!("{line}\n"),
    })) {
        Ok(ReplyData::TerminalInput(_)) => {}
        other => panic!("terminal.input: {other:?}"),
    }
}

fn output_until(client: &mut Client, terminal: &Id, needle: &str) -> String {
    let mut seen = String::new();
    let mut last_seq = 0;
    while !seen.contains(needle) {
        let event = client.wait_event(
            needle,
            |e| matches!(e, Event::TerminalOutput(o) if o.terminal_id == *terminal),
        );
        let Event::TerminalOutput(output) = event else {
            unreachable!()
        };
        assert!(output.seq.value() > last_seq, "sequence increases");
        last_seq = output.seq.value();
        seen.push_str(&output.data);
    }
    seen
}

fn closed(client: &mut Client, terminal: &Id) -> TerminalCloseReason {
    match client.wait_event(
        "terminal.closed",
        |e| matches!(e, Event::TerminalClosed(c) if c.terminal_id == *terminal),
    ) {
        Event::TerminalClosed(c) => c.reason,
        _ => unreachable!(),
    }
}

fn sleepers(marker: &str) -> usize {
    let expected = format!("sleep\0{marker}\0").into_bytes();
    fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            let live = fs::read_to_string(format!("/proc/{pid}/stat"))
                .is_ok_and(|s| s.rsplit(") ").next().is_some_and(|r| !r.starts_with('Z')));
            live && fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c == "sleep\n")
                && fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|c| c.ends_with(&expected))
        })
        .count()
}

fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    condition()
}

fn started_objective(env: &Env, client: &mut Client, title: &str) -> Id {
    let ws = open_workspace(client, &env.repo);
    let objective = create_objective(client, &ws.workspace_id, title).objective_id;
    let projected = detail(client, &objective)
        .available_actions
        .into_iter()
        .find(|a| a.action == ActionName::TerminalOpen)
        .expect("terminal.open is projected");
    assert!(!projected.enabled);
    let refused = client.call(open_command(&objective)).unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidLifecycle, "{refused:?}");
    assert_eq!(
        refused.action,
        SafeAction::None,
        "a precondition, not stale state"
    );
    assert_eq!(projected.reason.as_deref(), Some(refused.detail.as_str()));
    assert_eq!(
        refused.detail,
        "Start the agent once to create this Objective's worktree."
    );
    start_execution(client, &objective);
    wait_state(client, &objective, ExecutionState::Idle);
    assert!(worktree(env, &objective).is_dir());
    objective
}

#[test]
fn terminal_writes_only_in_scope_and_freeze_excludes_it() {
    let env = Env::new("svc-term-scope");
    let _service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let objective = started_objective(&env, &mut client, "Terminal scope");
    if !sandbox_works() {
        let refused = client.call(open_command(&objective)).unwrap_err();
        assert_eq!(refused.code, ErrorCode::ExecutionUnavailable);
        let projected = detail(&mut client, &objective)
            .available_actions
            .into_iter()
            .find(|a| a.action == ActionName::TerminalOpen)
            .unwrap();
        assert!(!projected.enabled);
        assert_eq!(projected.reason.as_deref(), Some(refused.detail.as_str()));
        eprintln!("note: sandbox unavailable; terminal refused as expected");
        return;
    }
    let refused = client
        .call(Request::TerminalOpen(TerminalOpenCommand {
            objective_id: objective.clone(),
            cols: Counter::new(0),
            rows: Counter::new(24),
        }))
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidRequest);

    let term = open(&mut client, &objective);
    assert_eq!(term.mode, TerminalMode::ReadWrite);
    input(
        &mut client,
        &term.terminal_id,
        "echo hello > from-terminal.txt; echo OK_$((1+1))",
    );
    output_until(&mut client, &term.terminal_id, "OK_2");
    let file = worktree(&env, &objective).join("from-terminal.txt");
    assert_eq!(fs::read_to_string(&file).unwrap(), "hello\n");
    input(
        &mut client,
        &term.terminal_id,
        "echo D_$(basename $(pwd))_$((3+3))",
    );
    let out = output_until(&mut client, &term.terminal_id, "_6");
    assert!(
        out.contains(&format!("D_{}_6", objective.as_str())),
        "{out:?}"
    );
    assert!(
        client
            .call(Request::TerminalResize(TerminalResizeCommand {
                terminal_id: term.terminal_id.clone(),
                cols: Counter::new(120),
                rows: Counter::new(40),
            }))
            .is_ok()
    );
    let marker = format!("{}.{}", 8000 + std::process::id() % 1000, 43);
    input(
        &mut client,
        &term.terminal_id,
        &format!("(setsid nohup sleep {marker} >/dev/null 2>&1 &); echo BG_$((4+4))"),
    );
    output_until(&mut client, &term.terminal_id, "BG_8");
    assert!(wait_until(|| sleepers(&marker) == 1));
    changes(&mut client, &objective, Basis::Working);
    match client.call(Request::ReviewFreeze(ObjectiveSelector {
        objective_id: objective.clone(),
    })) {
        Ok(ReplyData::ReviewFreeze(_)) => {}
        other => panic!("review.freeze: {other:?}"),
    }
    assert_eq!(
        closed(&mut client, &term.terminal_id),
        TerminalCloseReason::ReviewFreeze
    );
    assert_eq!(sleepers(&marker), 0, "writer excluded before the capture");
    let gone = client
        .call(Request::TerminalInput(TerminalInputCommand {
            terminal_id: term.terminal_id.clone(),
            data: "x".into(),
        }))
        .unwrap_err();
    assert_eq!(gone.code, ErrorCode::NotFound);

    let review = open(&mut client, &objective);
    assert_eq!(review.mode, TerminalMode::ReadOnly);
    input(
        &mut client,
        &review.terminal_id,
        "cat from-terminal.txt; echo changed > from-terminal.txt; echo RO_${LADE_TERMINAL}_$((2+5))",
    );
    let out = output_until(&mut client, &review.terminal_id, "RO_read_only_7");
    assert!(out.contains("hello"), "{out:?}");
    assert_eq!(fs::read_to_string(&file).unwrap(), "hello\n");
    match client.call(Request::TerminalClose(TerminalSelector {
        terminal_id: review.terminal_id.clone(),
    })) {
        Ok(ReplyData::TerminalClose(_)) => {}
        other => panic!("terminal.close: {other:?}"),
    }
    assert_eq!(
        closed(&mut client, &review.terminal_id),
        TerminalCloseReason::Closed
    );
}

#[test]
fn concurrent_opens_respect_the_per_objective_bound() {
    let env = Env::new("svc-term-bound");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let objective = started_objective(&env, &mut client, "Terminal bound");
    if !sandbox_works() {
        return;
    }
    let mutating = open_command(&objective).name().is_mutating();
    let sent: Vec<Id> = (0..5)
        .map(|_| client.send(open_command(&objective), mutating))
        .collect();
    let mut opened = 0;
    let mut refused = 0;
    for request_id in &sent {
        match result(client.wait_reply(request_id)) {
            Ok(ReplyData::TerminalOpen(_)) => opened += 1,
            Err(rejection) => {
                assert_eq!(rejection.code, ErrorCode::LimitReached, "{rejection:?}");
                refused += 1;
            }
            Ok(other) => panic!("terminal.open: {other:?}"),
        }
    }
    assert_eq!((opened, refused), (4, 1));
    drop(client);
    service.shutdown();
}

#[test]
fn lease_loss_kills_terminals_and_a_second_client_gets_none() {
    let env = Env::new("svc-term-lease");
    let _service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let objective = started_objective(&env, &mut client, "Terminal lease");
    if !sandbox_works() {
        return;
    }
    let term = open(&mut client, &objective);
    let marker = format!("{}.{}", 8000 + std::process::id() % 1000, 47);
    input(
        &mut client,
        &term.terminal_id,
        &format!("(setsid nohup sleep {marker} >/dev/null 2>&1 &); echo BG_$((4+5))"),
    );
    output_until(&mut client, &term.terminal_id, "BG_9");
    assert!(wait_until(|| sleepers(&marker) == 1));
    assert!(Client::connect(&env.socket(), &env.capability()).is_err());
    drop(client);
    assert!(
        wait_until(|| sleepers(&marker) == 0),
        "terminal survived the loss of its lease"
    );
    let mut next = eventually("lease freed", || {
        Client::connect(&env.socket(), &env.capability()).ok()
    });
    let gone = next
        .call(Request::TerminalClose(TerminalSelector {
            terminal_id: term.terminal_id.clone(),
        }))
        .unwrap_err();
    assert_eq!(gone.code, ErrorCode::NotFound);
    assert!(
        wait_until(|| fs::read_dir(env.root.path().join("state/run"))
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().starts_with("terminal-"))),
        "pidfile removed"
    );
}

#[test]
fn flood_output_is_bounded_and_rate_limited() {
    let env = Env::new("svc-term-flood");
    let _service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let objective = started_objective(&env, &mut client, "Terminal flood");
    if !sandbox_works() {
        return;
    }
    let term = open(&mut client, &objective);
    input(
        &mut client,
        &term.terminal_id,
        "stty -echo; tr '\\0' x < /dev/zero",
    );
    let window = Duration::from_secs(2);
    let start = Instant::now();
    let mut total = 0usize;
    while start.elapsed() < window {
        let event = client.wait_event(
            "flood output",
            |e| matches!(e, Event::TerminalOutput(o) if o.terminal_id == term.terminal_id),
        );
        let Event::TerminalOutput(output) = event else {
            unreachable!()
        };
        assert!(
            output.data.len() <= 32 << 10,
            "one event carries a bounded chunk"
        );
        total += output.data.len();
    }
    let elapsed = start.elapsed().as_secs_f64();
    let ceiling = (elapsed + 0.5) * 10.0 * f64::from(64u32 << 10) + f64::from(1u32 << 20);
    assert!(total > 64 << 10, "the flood made progress: {total}");
    assert!(
        f64::from(u32::try_from(total).unwrap()) <= ceiling,
        "rate is bounded: {total} bytes in {elapsed:.1}s"
    );
    match client.call(Request::TerminalClose(TerminalSelector {
        terminal_id: term.terminal_id.clone(),
    })) {
        Ok(ReplyData::TerminalClose(_)) => {}
        other => panic!("terminal.close: {other:?}"),
    }
    assert_eq!(
        closed(&mut client, &term.terminal_id),
        TerminalCloseReason::Closed
    );
    assert_eq!(
        detail(&mut client, &objective).execution.state,
        ExecutionState::Idle
    );
}

#[test]
fn explicit_stop_ends_the_service_and_nothing_resumes() {
    let env = Env::new("svc-term-stop");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let objective = started_objective(&env, &mut client, "Terminal stop");
    let marker = format!("{}.{}", 8000 + std::process::id() % 1000, 53);
    let term = sandbox_works().then(|| {
        let term = open(&mut client, &objective);
        input(
            &mut client,
            &term.terminal_id,
            &format!("(setsid nohup sleep {marker} >/dev/null 2>&1 &); echo BG_$((5+5))"),
        );
        output_until(&mut client, &term.terminal_id, "BG_10");
        term
    });
    match client.call(Request::ServiceStop(ServiceStopCommand {
        reason: StopReason::Exit,
    })) {
        Ok(ReplyData::ServiceStop(_)) => {}
        other => panic!("service.stop: {other:?}"),
    }
    let waiter = thread::spawn(move || service.wait());
    assert!(wait_until(|| waiter.is_finished()), "service did not stop");
    waiter.join().unwrap();
    assert!(!env.socket().exists(), "socket removed");
    assert_eq!(sleepers(&marker), 0, "no terminal process lingers");
    drop(client);

    let _service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let d = detail(&mut client, &objective);
    assert!(
        matches!(d.execution.state, ExecutionState::Cancelled),
        "{:?}",
        d.execution
    );
    if let Some(term) = term {
        let gone = client
            .call(Request::TerminalClose(TerminalSelector {
                terminal_id: term.terminal_id,
            }))
            .unwrap_err();
        assert_eq!(gone.code, ErrorCode::NotFound);
    }
}
