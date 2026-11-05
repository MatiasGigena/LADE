mod service_support;

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::SyncSender;

use lade_system::supervision::{
    CredentialSource, LaunchError, LaunchRequest, LauncherConfig, SupervisorEvent, WorkerControl,
    WorkerLauncher, default_launcher,
};

use lade_protocol::client::{
    ActionName, Basis, CandidateCommand, ErrorCode, ExecutionState, Intent, Lifecycle,
    MessageSendCommand, ObjectiveCleanupCommand, ObjectiveDetail, ObjectiveSelector,
    ObjectiveUpdateIntentCommand, ReplyData, Request, ReviewApproveCommand, ReviewIntegrateCommand,
    ReviewVerifyCommand,
};
use lade_protocol::types::{DeliveryMode, Id, Model};
use service_support::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Defined,
    ActiveIdleWithChanges,
    ActiveIdleEmpty,
    CancelledWithChanges,
    CancelledEmpty,
    Frozen,
    FrozenTargetMoved,
    Approved,
    Integrated,
    Discarded,
    CleanedUp,
}

const STATES: [State; 11] = [
    State::Defined,
    State::ActiveIdleWithChanges,
    State::ActiveIdleEmpty,
    State::CancelledWithChanges,
    State::CancelledEmpty,
    State::Frozen,
    State::FrozenTargetMoved,
    State::Approved,
    State::Integrated,
    State::Discarded,
    State::CleanedUp,
];

static FILES: AtomicU32 = AtomicU32::new(0);

fn selector(objective: &Id) -> ObjectiveSelector {
    ObjectiveSelector {
        objective_id: objective.clone(),
    }
}

fn ok(result: Result<ReplyData, lade_protocol::client::Rejection>, what: &str) -> ReplyData {
    result.unwrap_or_else(|r| panic!("{what}: {:?} {}", r.code, r.detail))
}

fn write_change(env: &Env, client: &mut Client, objective: &Id) {
    let n = FILES.fetch_add(1, Ordering::SeqCst);
    fs::write(
        worktree(env, objective).join(format!("change-{n}.txt")),
        format!("change {n}\n"),
    )
    .unwrap();
    assert!(!changes(client, objective, Basis::Working).files.is_empty());
}

fn running(env: &Env, client: &mut Client, objective: &Id, with_changes: bool) {
    start_execution(client, objective);
    wait_state(client, objective, ExecutionState::Idle);
    if with_changes {
        write_change(env, client, objective);
    } else {
        assert!(changes(client, objective, Basis::Working).files.is_empty());
    }
}

fn cancel(client: &mut Client, objective: &Id) {
    ok(
        client.call(Request::ExecutionCancel(selector(objective))),
        "cancel",
    );
    wait_state(client, objective, ExecutionState::Cancelled);
}

fn approve_override(client: &mut Client, objective: &Id) -> Id {
    let d = detail(client, objective);
    let candidate = d.candidates.last().unwrap().candidate_id.clone();
    match ok(
        client.call(Request::ReviewApprove(ReviewApproveCommand {
            objective_id: objective.clone(),
            candidate_id: candidate,
            target_commit: d.integration_target.commit.clone(),
            override_reason: Some("consistency test".into()),
        })),
        "approve",
    ) {
        ReplyData::ReviewApprove(a) => a.approval_id,
        other => panic!("{other:?}"),
    }
}

fn build(env: &Env, client: &mut Client, workspace: &Id, state: State) -> Id {
    let objective = create_objective(client, workspace, &format!("{state:?}")).objective_id;
    match state {
        State::Defined => {}
        State::ActiveIdleWithChanges => running(env, client, &objective, true),
        State::ActiveIdleEmpty => running(env, client, &objective, false),
        State::CancelledWithChanges => {
            running(env, client, &objective, true);
            cancel(client, &objective);
        }
        State::CancelledEmpty => {
            running(env, client, &objective, false);
            cancel(client, &objective);
        }
        State::Frozen | State::FrozenTargetMoved | State::Approved | State::Integrated => {
            running(env, client, &objective, true);
            ok(
                client.call(Request::ReviewFreeze(selector(&objective))),
                "freeze",
            );
            match state {
                State::FrozenTargetMoved => {
                    let n = FILES.fetch_add(1, Ordering::SeqCst);
                    fs::write(env.repo.join(format!("main-{n}.txt")), "main\n").unwrap();
                    commit_all(&env.repo, "move the target");
                }
                State::Approved => {
                    approve_override(client, &objective);
                }
                State::Integrated => {
                    let approval = approve_override(client, &objective);
                    ok(
                        client.call(Request::ReviewIntegrate(ReviewIntegrateCommand {
                            objective_id: objective.clone(),
                            approval_id: approval,
                        })),
                        "integrate",
                    );
                }
                _ => {}
            }
        }
        State::Discarded | State::CleanedUp => {
            ok(
                client.call(Request::ObjectiveDiscard(selector(&objective))),
                "discard",
            );
            if state == State::CleanedUp {
                ok(
                    client.call(Request::ObjectiveCleanup(ObjectiveCleanupCommand {
                        objective_id: objective.clone(),
                        confirm_data_loss: true,
                    })),
                    "cleanup",
                );
            }
        }
    }
    objective
}

fn request_for(action: ActionName, d: &ObjectiveDetail) -> Option<Request> {
    let objective = d.objective_id.clone();
    let candidate = d
        .candidates
        .last()
        .map_or_else(new_id, |c| c.candidate_id.clone());
    Some(match action {
        ActionName::ObjectiveUpdateIntent => {
            Request::ObjectiveUpdateIntent(ObjectiveUpdateIntentCommand {
                objective_id: objective,
                expected_intent_revision: d.intent_revision.clone(),
                intent: Intent {
                    outcome: "changed outcome".into(),
                    constraints: "none".into(),
                    acceptance: "checks pass".into(),
                },
            })
        }
        ActionName::ExecutionStart => {
            Request::ExecutionStart(lade_protocol::client::ExecutionStartCommand {
                objective_id: objective,
                model: Model {
                    provider: "lade-fixture".into(),
                    id: "scripted".into(),
                },
                thinking: None,
            })
        }
        ActionName::ExecutionCancel => Request::ExecutionCancel(selector(&objective)),
        ActionName::ExecutionResume => Request::ExecutionResume(selector(&objective)),
        ActionName::MessageSend => Request::MessageSend(MessageSendCommand {
            objective_id: objective,
            text: "hello".into(),
            mode: DeliveryMode::Prompt,
        }),
        ActionName::FeedbackSubmit | ActionName::TerminalOpen => return None,
        ActionName::ReviewFreeze => Request::ReviewFreeze(selector(&objective)),
        ActionName::ReviewReturn => Request::ReviewReturn(CandidateCommand {
            objective_id: objective,
            candidate_id: candidate,
        }),
        ActionName::ReviewVerify => Request::ReviewVerify(ReviewVerifyCommand {
            objective_id: objective,
            candidate_id: candidate,
            command: vec!["sh".into(), "-c".into(), "true".into()],
        }),
        ActionName::ReviewApprove => Request::ReviewApprove(ReviewApproveCommand {
            objective_id: objective,
            candidate_id: candidate,
            target_commit: d.integration_target.commit.clone(),
            override_reason: Some("consistency test".into()),
        }),
        ActionName::ReviewRefresh => Request::ReviewRefresh(CandidateCommand {
            objective_id: objective,
            candidate_id: candidate,
        }),
        ActionName::ReviewIntegrate => Request::ReviewIntegrate(ReviewIntegrateCommand {
            objective_id: objective,
            approval_id: d
                .approval
                .as_ref()
                .map_or_else(new_id, |a| a.approval_id.clone()),
        }),
        ActionName::ObjectiveComplete => Request::ObjectiveComplete(selector(&objective)),
        ActionName::ObjectiveDiscard => Request::ObjectiveDiscard(selector(&objective)),
        ActionName::ObjectiveCleanup => Request::ObjectiveCleanup(ObjectiveCleanupCommand {
            objective_id: objective,
            confirm_data_loss: false,
        }),
        ActionName::ObjectiveDeleteHistory => {
            Request::ObjectiveDeleteHistory(lade_protocol::client::ObjectiveDeleteHistoryCommand {
                objective_id: objective,
                confirm_history_loss: true,
            })
        }
    })
}

#[test]
fn every_projected_action_agrees_with_its_request() {
    let env = Env::new("svc-actions");
    let service = env.start_limited(
        FakeLauncher::shared(),
        lade_system::service::ResourceLimits {
            max_workers: 256,
            ..Default::default()
        },
    );
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let mut failures = Vec::new();
    let mut checked = 0;
    for state in STATES {
        let probe = build(&env, &mut client, &ws.workspace_id, state);
        let actions = detail(&mut client, &probe).available_actions;
        assert_eq!(actions.len(), 17, "one entry per action");
        for available in actions {
            let objective = build(&env, &mut client, &ws.workspace_id, state);
            let d = detail(&mut client, &objective);
            let projected = d
                .available_actions
                .iter()
                .find(|a| a.action == available.action)
                .unwrap()
                .clone();
            let Some(request) = request_for(projected.action, &d) else {
                continue;
            };
            checked += 1;
            let result = client.call(request);
            match (&result, projected.enabled) {
                (Ok(_), true) | (Err(_), false) => {}
                (Err(r), true) => failures.push(format!(
                    "{state:?}: {:?} enabled ({:?}) but rejected {:?}: {}",
                    projected.action, projected.reason, r.code, r.detail
                )),
                (Ok(_), false) => failures.push(format!(
                    "{state:?}: {:?} disabled ({:?}) but accepted",
                    projected.action, projected.reason
                )),
            }
            if projected.action == ActionName::ReviewFreeze
                && !projected.enabled
                && let Err(r) = &result
                && d.lifecycle == Lifecycle::Active
            {
                assert_eq!(
                    r.code,
                    ErrorCode::InvalidLifecycle,
                    "{state:?}: {}",
                    r.detail
                );
            }
        }
    }
    assert!(checked >= 130, "checked {checked}");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    drop(client);
    service.shutdown();
}

#[test]
fn empty_change_set_disables_freeze_with_the_same_reason_as_the_rejection() {
    let env = Env::new("svc-freeze-empty");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = build(&env, &mut client, &ws.workspace_id, State::ActiveIdleEmpty);
    let d = detail(&mut client, &objective);
    let freeze = d
        .available_actions
        .iter()
        .find(|a| a.action == ActionName::ReviewFreeze)
        .unwrap();
    assert!(!freeze.enabled);
    let reason = freeze.reason.clone().unwrap();
    assert_eq!(
        reason,
        "There are no changes to freeze. Complete without changes instead."
    );
    let rejected = client
        .call(Request::ReviewFreeze(selector(&objective)))
        .unwrap_err();
    assert_eq!(rejected.code, ErrorCode::InvalidLifecycle);
    assert_eq!(rejected.detail, reason);
    assert_eq!(
        detail(&mut client, &objective).execution.state,
        ExecutionState::Idle
    );
    drop(client);
    service.shutdown();
}

fn start_with(
    client: &mut Client,
    objective: &Id,
    provider: &str,
    id: &str,
) -> Result<ReplyData, lade_protocol::client::Rejection> {
    client.call(Request::ExecutionStart(
        lade_protocol::client::ExecutionStartCommand {
            objective_id: objective.clone(),
            model: Model {
                provider: provider.into(),
                id: id.into(),
            },
            thinking: None,
        },
    ))
}

#[test]
fn start_without_credentials_is_refused_synchronously_naming_the_variable() {
    if !sandbox_works() {
        eprintln!("skipping: bubblewrap sandbox unavailable");
        return;
    }
    let env = Env::new("svc-creds");
    let mut config = LauncherConfig::new(
        env.root.path().join("state/run"),
        env.root.path().join("run/g"),
        std::env::var_os("LADE_NODE").map(PathBuf::from),
        std::env::var_os("LADE_WORKER_DIR").map(PathBuf::from),
    )
    .unwrap();
    config.credentials = CredentialSource::empty();
    config.test_fixture_provider = true;
    let launcher: Arc<dyn WorkerLauncher> = Arc::from(default_launcher(config).unwrap());
    let service = env.start(launcher);
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "Creds").objective_id;
    let refused =
        start_with(&mut client, &objective, "anthropic", "claude-sonnet-4-5").unwrap_err();
    assert_eq!(refused.code, ErrorCode::ExecutionUnavailable);
    assert!(
        refused.detail.contains("ANTHROPIC_API_KEY"),
        "{}",
        refused.detail
    );
    let unchanged = detail(&mut client, &objective);
    assert_eq!(unchanged.lifecycle, Lifecycle::Defined);
    assert_eq!(unchanged.execution.state, ExecutionState::None);
    let refused = start_with(&mut client, &objective, "no-such-provider", "m").unwrap_err();
    assert_eq!(refused.code, ErrorCode::ExecutionUnavailable);
    ok(
        start_with(&mut client, &objective, "lade-fixture", "scripted"),
        "fixture start",
    );
    drop(client);
    service.shutdown();
}

struct NoSandbox;

impl WorkerLauncher for NoSandbox {
    fn launch(
        &self,
        _request: LaunchRequest,
        _events: SyncSender<SupervisorEvent>,
    ) -> Result<Box<dyn WorkerControl>, LaunchError> {
        Err(LaunchError::SandboxUnavailable("bwrap not found".into()))
    }

    fn preflight(&self, _provider: &str) -> Result<(), LaunchError> {
        Err(LaunchError::SandboxUnavailable("bwrap not found".into()))
    }
}

#[test]
fn start_without_a_sandbox_is_refused_synchronously() {
    let env = Env::new("svc-nosandbox");
    let service = env.start(Arc::new(NoSandbox));
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "NoSandbox").objective_id;
    let refused = start_with(&mut client, &objective, "lade-fixture", "scripted").unwrap_err();
    assert_eq!(refused.code, ErrorCode::ExecutionUnavailable);
    assert_eq!(
        refused.detail,
        "LADE can't run agents on this computer because the sandbox check failed."
    );
    assert_eq!(
        detail(&mut client, &objective).execution.state,
        ExecutionState::None
    );
    drop(client);
    service.shutdown();
}
