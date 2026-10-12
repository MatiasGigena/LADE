//! The first-release collaboration loop over the real service: real pi worker with the
//! `lade-fixture` model (inside the real bubblewrap sandbox when available), real Git, real
//! SQLite, real socket: message, working change, draft, feedback, freeze, sandboxed
//! verification, approve, integrate, cleanup.

mod service_support;

use std::fs;
use std::path::Path;

use lade_protocol::client::{
    ActionName, ApprovalState, Basis, CandidateCommand, CandidateStatus, DeliveryState,
    DiffGetQuery, DraftRevision, DraftSaveCommand, ErrorCode, Event, ExecutionState,
    FeedbackSubmitCommand, Lifecycle, MessageSendCommand, ObjectiveCleanupCommand, ObjectiveDetail,
    ObjectiveSelector, Reference, ReferenceVersion, ReplyData, Request, ReviewApproveCommand,
    ReviewIntegrateCommand, ReviewVerifyCommand, SearchQuery, SearchScope, Side, VerificationState,
};
use lade_protocol::types::{Counter, DeliveryMode, Id, PageLimit, WirePath};
use service_support::*;

fn send_message(client: &mut Client, objective: &Id, text: &str) {
    match client.call(Request::MessageSend(MessageSendCommand {
        objective_id: objective.clone(),
        text: text.into(),
        mode: DeliveryMode::Prompt,
    })) {
        Ok(ReplyData::MessageSend(input)) => {
            assert!(matches!(
                input.delivery,
                DeliveryState::Queued | DeliveryState::Dispatched | DeliveryState::Received
            ));
        }
        other => panic!("message.send: {other:?}"),
    }
}

fn wait_file(client: &mut Client, objective: &Id, path: &str, content: &str) {
    eventually(
        &format!("{path} = {content:?} in the working capture"),
        || {
            let list = changes(client, objective, Basis::Working);
            if !list
                .files
                .iter()
                .any(|f| f.path.as_bytes() == path.as_bytes())
            {
                return None;
            }
            let file = read_file(client, objective, Basis::Working, path);
            (file.text.as_deref() == Some(content)).then_some(())
        },
    );
}

fn freeze(client: &mut Client, objective: &Id) -> lade_protocol::client::CandidateSummary {
    match client.call(Request::ReviewFreeze(ObjectiveSelector {
        objective_id: objective.clone(),
    })) {
        Ok(ReplyData::ReviewFreeze(candidate)) => candidate,
        other => panic!("review.freeze: {other:?}"),
    }
}

fn approve(
    client: &mut Client,
    detail: &ObjectiveDetail,
    candidate: &Id,
    reason: Option<&str>,
) -> Result<ReplyData, lade_protocol::client::Rejection> {
    client.call(Request::ReviewApprove(ReviewApproveCommand {
        objective_id: detail.objective_id.clone(),
        candidate_id: candidate.clone(),
        target_commit: detail.integration_target.commit.clone(),
        override_reason: reason.map(str::to_string),
    }))
}

fn approval_id(data: ReplyData) -> Id {
    match data {
        ReplyData::ReviewApprove(a) => {
            assert_eq!(a.state, ApprovalState::Valid);
            a.approval_id
        }
        other => panic!("review.approve: {other:?}"),
    }
}

fn integrate(
    client: &mut Client,
    objective: &Id,
    approval: &Id,
) -> Result<ReplyData, lade_protocol::client::Rejection> {
    client.call(Request::ReviewIntegrate(ReviewIntegrateCommand {
        objective_id: objective.clone(),
        approval_id: approval.clone(),
    }))
}

fn head(repo: &Path) -> String {
    git(repo, &["rev-parse", "HEAD"])
}

#[test]
fn full_loop_with_the_real_worker() {
    let env = Env::new("svc-flow");
    // The real bubblewrap launcher when the host supports it (the release path), else the
    // unsandboxed test launcher.
    let (launcher, sandboxed) = best_launcher(&env);
    eprintln!("full loop sandboxed: {sandboxed}");
    let service = env.start(launcher);
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    assert_eq!(ws.default_branch.as_deref(), Some("main"));
    let objective = create_objective(&mut client, &ws.workspace_id, "Greeting").objective_id;
    start_execution(&mut client, &objective);
    wait_state(&mut client, &objective, ExecutionState::Idle);

    // Agent work in the isolated worktree.
    send_message(&mut client, &objective, "lade-fixture: write hello.txt hi");
    wait_file(&mut client, &objective, "hello.txt", "hi");
    assert!(
        !env.repo.join("hello.txt").exists(),
        "shared checkout untouched"
    );
    client.wait_event("activity", |e| matches!(e, Event::ActivityAppended(_)));
    match client.call(Request::ActivityPage(
        lade_protocol::client::ActivityPageQuery {
            objective_id: objective.clone(),
            limit: PageLimit::parse("50").unwrap(),
            before: None,
        },
    )) {
        Ok(ReplyData::ActivityPage(page)) => assert!(!page.items.is_empty()),
        other => panic!("{other:?}"),
    }
    match client.call(Request::Search(SearchQuery {
        workspace_id: ws.workspace_id.clone(),
        scope: SearchScope::Files,
        query: "hello".into(),
        limit: PageLimit::parse("10").unwrap(),
        objective_id: Some(objective.clone()),
        basis: Some(Basis::Working),
    })) {
        Ok(ReplyData::Search(found)) => {
            assert_eq!(found.results.len(), 1);
            assert!(found.scope_label.contains("working capture"));
        }
        other => panic!("{other:?}"),
    }

    // Draft on an exact range of the inspected bytes, then submit it as feedback.
    let list = changes(&mut client, &objective, Basis::Working);
    let capture = list.capture_id.clone().unwrap();
    let file = read_file(&mut client, &objective, Basis::Working, "hello.txt");
    let detail_now = detail(&mut client, &objective);
    let session = detail_now.session.clone().unwrap();
    let draft = new_id();
    client
        .call(Request::DraftSave(DraftSaveCommand {
            draft_id: draft.clone(),
            objective_id: objective.clone(),
            expected_revision: Counter::new(0),
            reference: Reference {
                artifact_digest: file.artifact_digest.clone(),
                path: WirePath::from_bytes(b"hello.txt".to_vec()).unwrap(),
                version: ReferenceVersion::Working {
                    capture_id: capture,
                },
                side: Side::New,
                byte_start: Counter::new(0),
                byte_end: Counter::new(2),
                selected_text: "hi".into(),
                line_hint: Some(Counter::new(1)),
            },
            body: "Be friendlier.\nlade-fixture: write hello.txt hi there".into(),
        }))
        .unwrap();
    let input = match client
        .call(Request::FeedbackSubmit(FeedbackSubmitCommand {
            objective_id: objective.clone(),
            destination_session_id: session.session_id,
            association_revision: session.association_revision,
            drafts: vec![DraftRevision {
                draft_id: draft,
                revision: Counter::new(1),
            }],
            mode: DeliveryMode::FollowUp,
            note: None,
        }))
        .unwrap()
    {
        ReplyData::FeedbackSubmit(input) => input,
        other => panic!("{other:?}"),
    };
    client.wait_event("feedback received", |e| {
        matches!(e, Event::InputChanged(i) if i.input_id == input.input_id
            && i.delivery == DeliveryState::Received)
    });
    wait_file(&mut client, &objective, "hello.txt", "hi there");
    wait_state(&mut client, &objective, ExecutionState::Idle);

    // Frozen review: writers excluded, exact candidate.
    let candidate = freeze(&mut client, &objective);
    assert_eq!(candidate.status, CandidateStatus::Current);
    assert_eq!(candidate.files.value(), 1);
    let frozen = detail(&mut client, &objective);
    assert_eq!(frozen.lifecycle, Lifecycle::Frozen);
    assert_eq!(frozen.execution.state, ExecutionState::Cancelled);
    // The worktree's own Git view follows the candidate: HEAD on the branch at the candidate
    // commit, index refreshed, nothing reported as changed.
    let objective_wt = worktree(&env, &objective);
    assert_eq!(
        git(&objective_wt, &["rev-parse", "HEAD"]),
        candidate.commit.hex()
    );
    assert_eq!(
        git(&objective_wt, &["symbolic-ref", "HEAD"]),
        format!("refs/heads/{}", frozen.branch)
    );
    assert!(
        git(&objective_wt, &["status", "--porcelain"]).is_empty(),
        "{}",
        git(&objective_wt, &["status", "--porcelain"])
    );
    assert!(action_enabled(&frozen, ActionName::ReviewVerify));
    let candidate_basis = Basis::Candidate {
        candidate_id: candidate.candidate_id.clone(),
    };
    let cand_list = changes(&mut client, &objective, candidate_basis.clone());
    assert_eq!(cand_list.files.len(), 1);
    match client.call(Request::DiffGet(DiffGetQuery {
        objective_id: objective.clone(),
        basis: candidate_basis,
        path: WirePath::from_bytes(b"hello.txt".to_vec()).unwrap(),
    })) {
        Ok(ReplyData::DiffGet(diff)) => {
            assert!(diff.new_digest.is_some());
            assert!(diff.hunks[0].lines.iter().any(|l| l.text == "hi there"));
        }
        other => panic!("{other:?}"),
    }

    // Verification in an isolated copy when the sandbox works; otherwise a recorded override.
    let approval = if sandbox_works() {
        let started = match client
            .call(Request::ReviewVerify(ReviewVerifyCommand {
                objective_id: objective.clone(),
                candidate_id: candidate.candidate_id.clone(),
                command: vec!["sh".into(), "-c".into(), "test -f hello.txt".into()],
            }))
            .unwrap()
        {
            ReplyData::ReviewVerify(v) => v,
            other => panic!("{other:?}"),
        };
        assert_eq!(started.state, VerificationState::Running);
        let done = client.wait_event("verification finished", |e| {
            matches!(e, Event::VerificationChanged(v) if v.verification_id == started.verification_id
                && v.state != VerificationState::Running)
        });
        let Event::VerificationChanged(done) = done else {
            unreachable!()
        };
        assert_eq!(
            done.state,
            VerificationState::Passed,
            "{}",
            done.output_tail
        );
        assert!(
            !env.root
                .path()
                .join("state/verify")
                .join(started.verification_id.as_str())
                .exists()
        );
        let current = detail(&mut client, &objective);
        assert!(action_enabled(&current, ActionName::ReviewApprove));
        approval_id(approve(&mut client, &current, &candidate.candidate_id, None).unwrap())
    } else {
        eprintln!("note: bubblewrap unavailable; approving with an override reason");
        let current = detail(&mut client, &objective);
        assert!(approve(&mut client, &current, &candidate.candidate_id, None).is_err());
        approval_id(
            approve(
                &mut client,
                &current,
                &candidate.candidate_id,
                Some("no sandbox on host"),
            )
            .unwrap(),
        )
    };

    // Protected integration into the checked-out, clean main.
    let before = head(&env.repo);
    let integrated = match integrate(&mut client, &objective, &approval).unwrap() {
        ReplyData::ReviewIntegrate(d) => *d,
        other => panic!("{other:?}"),
    };
    assert_eq!(integrated.lifecycle, Lifecycle::Integrated);
    let after = head(&env.repo);
    assert_ne!(before, after);
    assert_eq!(git(&env.repo, &["rev-parse", "HEAD^"]), before);
    assert_eq!(
        fs::read_to_string(env.repo.join("hello.txt")).unwrap(),
        "hi there"
    );
    assert!(git(&env.repo, &["status", "--porcelain"]).is_empty());
    assert!(git(&env.repo, &["log", "-1", "--format=%s"]).contains("Greeting"));

    // Cleanup removes branch and worktree, keeps the record.
    let branch = integrated.branch.clone();
    match client.call(Request::ObjectiveCleanup(ObjectiveCleanupCommand {
        objective_id: objective.clone(),
        confirm_data_loss: false,
    })) {
        Ok(ReplyData::ObjectiveCleanup(d)) => {
            assert!(d.badges.contains(&lade_protocol::client::Badge::CleanedUp));
        }
        other => panic!("cleanup: {other:?}"),
    }
    assert!(!worktree(&env, &objective).exists());
    assert!(git(&env.repo, &["branch", "--list", &branch]).is_empty());
    let retained = git(&env.repo, &["for-each-ref", "refs/lade/retain/"]);
    assert!(retained.contains("candidate-"), "{retained}");

    // Only an explicit, confirmed request deletes the retained history; the record stays.
    match client.call(Request::ObjectiveDeleteHistory(
        lade_protocol::client::ObjectiveHistoryDeleteCommand {
            objective_id: objective.clone(),
            confirm_history_loss: true,
        },
    )) {
        Ok(ReplyData::ObjectiveDeleteHistory(d)) => {
            assert_eq!(d.history_deleted, Some(true));
            assert_eq!(d.lifecycle, Lifecycle::Integrated);
        }
        other => panic!("delete history: {other:?}"),
    }
    assert!(git(&env.repo, &["for-each-ref", "refs/lade/retain/"]).is_empty());
    assert_eq!(head(&env.repo), after, "the integration stays");
    drop(client);
    service.shutdown();
}

/// Frozen candidate produced from direct worktree writes (fake worker).
fn frozen_objective(
    env: &Env,
    client: &mut Client,
) -> (Id, lade_protocol::client::CandidateSummary) {
    let ws = open_workspace(client, &env.repo);
    let objective = create_objective(client, &ws.workspace_id, "Protected").objective_id;
    start_execution(client, &objective);
    wait_state(client, &objective, ExecutionState::Idle);
    fs::write(worktree(env, &objective).join("feature.txt"), "feature\n").unwrap();
    let candidate = freeze(client, &objective);
    (objective, candidate)
}

#[test]
fn dirty_target_refuses_and_moved_target_invalidates_approval() {
    let env = Env::new("svc-target");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let (objective, candidate) = frozen_objective(&env, &mut client);
    let current = detail(&mut client, &objective);
    let approval = approval_id(
        approve(
            &mut client,
            &current,
            &candidate.candidate_id,
            Some("manual check"),
        )
        .unwrap(),
    );

    // A dirty checked-out target refuses and stays untouched.
    let before = head(&env.repo);
    fs::write(env.repo.join("README"), "local edit\n").unwrap();
    let refused = integrate(&mut client, &objective, &approval).unwrap_err();
    assert_eq!(refused.code, ErrorCode::Conflict, "{}", refused.detail);
    assert_eq!(head(&env.repo), before);
    assert_eq!(
        fs::read_to_string(env.repo.join("README")).unwrap(),
        "local edit\n"
    );
    assert!(!env.repo.join("feature.txt").exists());
    git(&env.repo, &["checkout", "--", "README"]);
    assert_eq!(
        detail(&mut client, &objective).lifecycle,
        Lifecycle::Approved
    );

    // The target moves after approval: integration is stale and the approval expires.
    fs::write(env.repo.join("other.txt"), "other\n").unwrap();
    commit_all(&env.repo, "moved target");
    let moved = head(&env.repo);
    let stale = integrate(&mut client, &objective, &approval).unwrap_err();
    assert_eq!(stale.code, ErrorCode::StaleTarget);
    assert_eq!(head(&env.repo), moved);
    let after = detail(&mut client, &objective);
    assert_eq!(after.lifecycle, Lifecycle::Frozen);
    assert_eq!(
        after.approval.as_ref().unwrap().state,
        ApprovalState::Invalidated
    );
    assert!(
        after
            .badges
            .contains(&lade_protocol::client::Badge::StaleTarget)
    );
    assert!(action_enabled(&after, ActionName::ReviewRefresh));
    assert!(!action_enabled(&after, ActionName::ReviewIntegrate));

    // Refresh onto the moved target, approve again, integrate.
    let refreshed = match client
        .call(Request::ReviewRefresh(CandidateCommand {
            objective_id: objective.clone(),
            candidate_id: candidate.candidate_id.clone(),
        }))
        .unwrap()
    {
        ReplyData::ReviewRefresh(c) => c,
        other => panic!("{other:?}"),
    };
    assert_eq!(refreshed.base_target_commit.hex(), moved);
    assert_eq!(refreshed.ordinal.value(), 2);
    let current = detail(&mut client, &objective);
    let approval = approval_id(
        approve(
            &mut client,
            &current,
            &refreshed.candidate_id,
            Some("manual check"),
        )
        .unwrap(),
    );
    integrate(&mut client, &objective, &approval).unwrap();
    assert_eq!(git(&env.repo, &["rev-parse", "HEAD^"]), moved);
    assert!(env.repo.join("feature.txt").is_file());
    assert!(env.repo.join("other.txt").is_file());
    drop(client);
    service.shutdown();
}

#[test]
fn sandboxed_launcher_runs_the_worker_when_available() {
    if !sandbox_works() {
        eprintln!("skipping: bubblewrap sandbox unavailable");
        return;
    }
    let env = Env::new("svc-sandboxed");
    let (launcher, sandboxed) = best_launcher(&env);
    assert!(sandboxed);
    let service = env.start(launcher);
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "Sandboxed").objective_id;
    start_execution(&mut client, &objective);
    let ready = eventually("sandboxed worker ready or failed", || {
        let d = detail(&mut client, &objective);
        matches!(
            d.execution.state,
            ExecutionState::Idle | ExecutionState::Failed
        )
        .then_some(d)
    });
    assert_eq!(
        ready.execution.state,
        ExecutionState::Idle,
        "{:?}",
        ready.execution.reason
    );
    send_message(
        &mut client,
        &objective,
        "lade-fixture: write boxed.txt inside",
    );
    wait_file(&mut client, &objective, "boxed.txt", "inside");
    drop(client);
    service.shutdown();
}
