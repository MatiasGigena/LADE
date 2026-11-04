use std::collections::BTreeSet;
use std::path::PathBuf;

use lade_protocol::client::{
    self as wire, ClientBody, ClientEnvelope, ErrorCode, Rejection, ReplyData, RequestFrame,
    RequestName, SafeAction,
};
use lade_protocol::types::{Counter, Digest, GitAlgorithm, GitId, Id, Timestamp};
use lade_protocol::{Framing, encode};

use super::*;
use crate::presentation::controller::intents::DraftRevisionRef;
use crate::presentation::state::agent_session::{
    AgentSessionUpdate, ForkPosition, ThinkingLevel, UiAnswerView,
};
use crate::presentation::state::model::{
    ApprovalId, ArtifactDigest, CandidateId, CaptureId, DeliveryMode, ModelRef, Reference,
    ReferenceVersion, SessionId, WorkspaceId,
};

pub(crate) const O: &str = "0b1c2d3e-4f50-4a61-8b72-839405a6b7c8";
pub(crate) const W: &str = "1b1c2d3e-4f50-4a61-8b72-839405a6b7c8";
const C: &str = "2b1c2d3e-4f50-4a61-8b72-839405a6b7c8";
const D: &str = "3b1c2d3e-4f50-4a61-8b72-839405a6b7c8";
const S: &str = "4b1c2d3e-4f50-4a61-8b72-839405a6b7c8";
const A: &str = "5b1c2d3e-4f50-4a61-8b72-839405a6b7c8";
const T: &str = "7b1c2d3e-4f50-4a61-8b72-839405a6b7c8";
const CAPTURE: &str = "6b1c2d3e-4f50-4a61-8b72-839405a6b7c8";

pub(crate) fn oid() -> ObjectiveId {
    ObjectiveId::new(O)
}

pub(crate) fn epoch() -> Id {
    Id::parse("eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee").unwrap()
}

pub(crate) fn snapshot_data(sequence: u64) -> ReplyData {
    ReplyData::Snapshot(wire::SnapshotData {
        service_epoch: epoch(),
        sequence: Counter::new(sequence),
        workspaces: Vec::new(),
        objectives: Vec::new(),
        settings: wire::Settings {
            verification_commands: Vec::new(),
            default_model: None,
        },
    })
}

pub(crate) fn event(sequence: u64) -> wire::EventFrame {
    wire::EventFrame {
        service_epoch: epoch(),
        sequence: Counter::new(sequence),
        event: wire::Event::ObjectiveChanged(wire_detail(O).summary()),
    }
}

fn reference() -> Reference {
    Reference {
        artifact_digest: ArtifactDigest::new("a".repeat(64)),
        path: RepoPath::utf8("src/lib.rs"),
        version: ReferenceVersion::Working {
            capture_id: CaptureId::new(CAPTURE),
        },
        side: Side::New,
        byte_start: 3,
        byte_end: 9,
        selected_text: "fn main".into(),
        line_hint: Some(1),
    }
}

fn every_intent() -> Vec<(Intent, Option<RequestName>)> {
    use Intent as I;
    use RequestName as N;
    let candidate = || Basis::Candidate {
        candidate_id: CandidateId::new(C),
    };
    let all = vec![
        (I::RetryConnection, None),
        (
            I::OpenWorkspace {
                path: PathBuf::from("/tmp/repo"),
            },
            Some(N::WorkspaceOpen),
        ),
        (
            I::LoadObjective {
                objective_id: oid(),
            },
            Some(N::ObjectiveGet),
        ),
        (
            I::LoadActivity {
                objective_id: oid(),
                before: Some(7),
            },
            Some(N::ActivityPage),
        ),
        (
            I::LoadChanges {
                objective_id: oid(),
                basis: Basis::Since {
                    from_candidate_id: CandidateId::new(C),
                    to_candidate_id: CandidateId::new(D),
                },
            },
            Some(N::ChangesList),
        ),
        (
            I::LoadDiff {
                objective_id: oid(),
                basis: candidate(),
                path: RepoPath::from_bytes(b"src/\xff.rs".to_vec()),
            },
            Some(N::DiffGet),
        ),
        (
            I::OpenFile {
                objective_id: oid(),
                basis: Basis::Working,
                path: RepoPath::utf8("README"),
                side: Side::Old,
                offset: 262_144,
            },
            Some(N::FileRead),
        ),
        (
            I::LoadDrafts {
                objective_id: oid(),
            },
            Some(N::DraftsList),
        ),
        (
            I::LoadInputs {
                objective_id: oid(),
            },
            Some(N::InputsList),
        ),
        (
            I::Search {
                workspace_id: WorkspaceId::new(W),
                scope: SearchScope::Content,
                objective_id: Some(oid()),
                basis: Some(candidate()),
                query: "retry".into(),
            },
            Some(N::Search),
        ),
        (
            I::CreateObjective {
                workspace_id: WorkspaceId::new(W),
                title: "Hello".into(),
                intent: view::Intent {
                    outcome: "hello.txt exists".into(),
                    constraints: String::new(),
                    acceptance: String::new(),
                },
                integration_target: "main".into(),
                start: StartPoint::Commit("A".repeat(40)),
            },
            Some(N::ObjectiveCreate),
        ),
        (
            I::UpdateObjective {
                objective_id: oid(),
                expected_intent_revision: 1,
                intent: view::Intent {
                    outcome: "hello.txt says hi".into(),
                    constraints: "only hello.txt".into(),
                    acceptance: String::new(),
                },
            },
            Some(N::ObjectiveUpdateIntent),
        ),
        (
            I::StartExecution {
                objective_id: oid(),
                model: ModelRef::new("lade-fixture", "scripted"),
                thinking: Some(ThinkingLevel::High),
            },
            Some(N::ExecutionStart),
        ),
        (
            I::Cancel {
                objective_id: oid(),
            },
            Some(N::ExecutionCancel),
        ),
        (
            I::Resume {
                objective_id: oid(),
            },
            Some(N::ExecutionResume),
        ),
        (
            I::SendMessage {
                objective_id: oid(),
                text: "lade-fixture: write hello.txt hi".into(),
                mode: DeliveryMode::Prompt,
            },
            Some(N::MessageSend),
        ),
        (
            I::SaveDraft {
                objective_id: oid(),
                draft_id: DraftId::new(D),
                expected_revision: 0,
                reference: reference(),
                body: "why?".into(),
            },
            Some(N::DraftSave),
        ),
        (
            I::DeleteDraft {
                draft_id: DraftId::new(D),
                expected_revision: 2,
            },
            Some(N::DraftDelete),
        ),
        (
            I::SubmitFeedback {
                objective_id: oid(),
                destination_session_id: SessionId::new(S),
                association_revision: 1,
                drafts: vec![DraftRevisionRef {
                    draft_id: DraftId::new(D),
                    revision: 2,
                }],
                mode: DeliveryMode::Steer,
                note: None,
            },
            Some(N::FeedbackSubmit),
        ),
        (
            I::Freeze {
                objective_id: oid(),
            },
            Some(N::ReviewFreeze),
        ),
        (
            I::ReturnForRevision {
                objective_id: oid(),
                candidate_id: CandidateId::new(C),
            },
            Some(N::ReviewReturn),
        ),
        (
            I::Verify {
                objective_id: oid(),
                candidate_id: CandidateId::new(C),
                argv: vec!["git".into(), "diff".into(), "--check".into()],
            },
            Some(N::ReviewVerify),
        ),
        (
            I::Approve {
                objective_id: oid(),
                candidate_id: CandidateId::new(C),
                target_commit: view::GitId::sha1("b".repeat(40)),
                override_reason: Some("no checks for docs".into()),
            },
            Some(N::ReviewApprove),
        ),
        (
            I::Refresh {
                objective_id: oid(),
                candidate_id: CandidateId::new(C),
            },
            Some(N::ReviewRefresh),
        ),
        (
            I::Integrate {
                objective_id: oid(),
                approval_id: ApprovalId::new(A),
            },
            Some(N::ReviewIntegrate),
        ),
        (
            I::Complete {
                objective_id: oid(),
            },
            Some(N::ObjectiveComplete),
        ),
        (
            I::Discard {
                objective_id: oid(),
            },
            Some(N::ObjectiveDiscard),
        ),
        (
            I::Cleanup {
                objective_id: oid(),
                confirm_data_loss: true,
            },
            Some(N::ObjectiveCleanup),
        ),
        (
            I::UpdateSettings {
                default_model: ModelRef::new("anthropic", "claude-sonnet-5"),
            },
            Some(N::SettingsUpdate),
        ),
        (
            I::LoadModels {
                provider: Some("lade-fixture".into()),
            },
            Some(N::ModelsList),
        ),
        (
            I::LoadSessionTree {
                objective_id: oid(),
            },
            Some(N::SessionTree),
        ),
        (
            I::NavigateSession {
                objective_id: oid(),
                entry_id: "a1b2c3d4".into(),
            },
            Some(N::SessionNavigate),
        ),
        (
            I::ForkSession {
                objective_id: oid(),
                entry_id: "a1b2c3d4".into(),
                position: ForkPosition::At,
            },
            Some(N::SessionFork),
        ),
        (
            I::ImportSession {
                objective_id: oid(),
                path: PathBuf::from("/tmp/session.jsonl"),
            },
            Some(N::SessionImport),
        ),
        (
            I::ExportSession {
                objective_id: oid(),
            },
            Some(N::SessionExport),
        ),
        (
            I::CompactSession {
                objective_id: oid(),
            },
            Some(N::SessionCompact),
        ),
        (
            I::ReloadSession {
                objective_id: oid(),
            },
            Some(N::SessionReload),
        ),
        (
            I::ConfigureSession {
                objective_id: oid(),
                model: ModelRef::new("lade-fixture", "scripted"),
                thinking: ThinkingLevel::Off,
            },
            Some(N::SessionConfigure),
        ),
        (I::LoadResources, Some(N::ResourcesList)),
        (
            I::DecideResource {
                name: "ask.js".into(),
                sha256: "ab".repeat(32),
                approve: true,
            },
            Some(N::ResourceDecide),
        ),
        (
            I::ImportProfile {
                source: PathBuf::from("/home/me/.pi/agent"),
            },
            Some(N::ProfileImport),
        ),
        (
            I::RespondDialog {
                objective_id: oid(),
                ui_id: D.into(),
                generation: 2,
                answer: UiAnswerView::Confirm(false),
            },
            Some(N::UiRespond),
        ),
        (
            I::OpenTerminal {
                objective_id: oid(),
                cols: 80,
                rows: 24,
            },
            Some(N::TerminalOpen),
        ),
        (
            I::TerminalInput {
                objective_id: oid(),
                terminal_id: T.into(),
                data: "ls\r".into(),
            },
            Some(N::TerminalInput),
        ),
        (
            I::ResizeTerminal {
                objective_id: oid(),
                terminal_id: T.into(),
                cols: 120,
                rows: 40,
            },
            Some(N::TerminalResize),
        ),
        (
            I::CloseTerminal {
                objective_id: oid(),
                terminal_id: T.into(),
            },
            Some(N::TerminalClose),
        ),
        (
            I::StopService {
                reason: crate::presentation::controller::intents::StopReason::Logout,
            },
            Some(N::ServiceStop),
        ),
        (
            I::DeleteHistory {
                objective_id: oid(),
            },
            Some(N::ObjectiveDeleteHistory),
        ),
    ];
    for (intent, _) in &all {
        match intent {
            I::RetryConnection
            | I::OpenWorkspace { .. }
            | I::LoadObjective { .. }
            | I::LoadActivity { .. }
            | I::LoadChanges { .. }
            | I::LoadDiff { .. }
            | I::OpenFile { .. }
            | I::LoadDrafts { .. }
            | I::LoadInputs { .. }
            | I::Search { .. }
            | I::CreateObjective { .. }
            | I::UpdateObjective { .. }
            | I::StartExecution { .. }
            | I::Cancel { .. }
            | I::Resume { .. }
            | I::SendMessage { .. }
            | I::SaveDraft { .. }
            | I::DeleteDraft { .. }
            | I::SubmitFeedback { .. }
            | I::Freeze { .. }
            | I::ReturnForRevision { .. }
            | I::Verify { .. }
            | I::Approve { .. }
            | I::Refresh { .. }
            | I::Integrate { .. }
            | I::Complete { .. }
            | I::Discard { .. }
            | I::Cleanup { .. }
            | I::DeleteHistory { .. }
            | I::UpdateSettings { .. }
            | I::LoadModels { .. }
            | I::LoadSessionTree { .. }
            | I::NavigateSession { .. }
            | I::ForkSession { .. }
            | I::ImportSession { .. }
            | I::ExportSession { .. }
            | I::CompactSession { .. }
            | I::ReloadSession { .. }
            | I::ConfigureSession { .. }
            | I::LoadResources
            | I::DecideResource { .. }
            | I::ImportProfile { .. }
            | I::RespondDialog { .. }
            | I::OpenTerminal { .. }
            | I::TerminalInput { .. }
            | I::ResizeTerminal { .. }
            | I::CloseTerminal { .. }
            | I::StopService { .. } => {}
        }
    }
    all
}

fn test_id(n: u8) -> Id {
    Id::from_random_bytes([n; 16])
}

#[test]
fn every_intent_maps_to_its_request_and_encodes_against_the_schema() {
    let mut covered = BTreeSet::new();
    for (intent, expected) in every_intent() {
        let mapped = request(&intent).expect("maps");
        assert_eq!(
            mapped.as_ref().map(|(r, _)| r.name()),
            expected,
            "{intent:?}"
        );
        let Some((request, context)) = mapped else {
            continue;
        };
        if let Context::Command { label, .. } = &context {
            assert_eq!(*label, intent.label());
        }
        covered.insert(request.name());
        let operation_id = request.name().is_mutating().then(|| test_id(2));
        let frame = RequestFrame::new(test_id(1), operation_id, request).unwrap();
        encode(
            &ClientEnvelope::new(ClientBody::Request(frame)),
            Framing::Client,
        )
        .unwrap_or_else(|e| panic!("{intent:?} does not encode: {e}"));
    }
    let catalog: BTreeSet<RequestName> = RequestName::ALL
        .iter()
        .copied()
        .filter(|n| !matches!(n, RequestName::Snapshot | RequestName::OperationGet))
        .collect();
    assert_eq!(
        covered, catalog,
        "every user-facing request name has an intent"
    );
}

#[test]
fn payload_fields_follow_the_intent() {
    let open = request(&Intent::OpenFile {
        objective_id: oid(),
        basis: Basis::Working,
        path: RepoPath::utf8("README"),
        side: Side::Old,
        offset: 10,
    })
    .unwrap()
    .unwrap();
    let Request::FileRead(query) = open.0 else {
        panic!("file.read")
    };
    assert_eq!(query.offset, Counter::new(10));
    assert_eq!(query.limit.value(), READ_WINDOW);
    assert_eq!(query.side, wire::Side::Old);
    assert_eq!(
        open.1,
        Context::File(oid(), Basis::Working, RepoPath::utf8("README"), Side::Old)
    );

    let create = every_intent()
        .into_iter()
        .find(|(i, _)| matches!(i, Intent::CreateObjective { .. }))
        .unwrap()
        .0;
    let Request::ObjectiveCreate(command) = request(&create).unwrap().unwrap().0 else {
        panic!("objective.create")
    };
    assert_eq!(
        command.start,
        wire::Start::Commit {
            commit: GitId::new(GitAlgorithm::Sha1, "a".repeat(40)).unwrap()
        }
    );
}

#[test]
fn start_point_parses_target_and_both_object_formats() {
    assert_eq!(start_point(&StartPoint::Target), Ok(wire::Start::Target));
    assert_eq!(
        start_point(&StartPoint::Commit(" target ".into())),
        Ok(wire::Start::Target)
    );
    let sha256 = "c".repeat(64);
    assert!(matches!(
        start_point(&StartPoint::Commit(sha256)),
        Ok(wire::Start::Commit { commit }) if commit.algorithm() == GitAlgorithm::Sha256
    ));
    assert!(start_point(&StartPoint::Commit("abc123".into())).is_err());
    assert!(start_point(&StartPoint::Commit("g".repeat(40))).is_err());
}

#[test]
fn invalid_view_values_are_refused_before_sending() {
    let err = request(&Intent::LoadObjective {
        objective_id: ObjectiveId::new("not-a-uuid"),
    })
    .unwrap_err();
    assert!(err.0.contains("objective id"));
}

fn timestamp() -> Timestamp {
    Timestamp::parse("2026-10-10T12:00:00.000Z").unwrap()
}

pub(crate) fn wire_detail(objective_id: &str) -> wire::ObjectiveDetail {
    let sha = |c: &str| GitId::new(GitAlgorithm::Sha1, c.repeat(40)).unwrap();
    wire::ObjectiveDetail {
        objective_id: Id::parse(objective_id).unwrap(),
        workspace_id: Id::parse(W).unwrap(),
        title: "Hello".into(),
        outcome: "hello.txt exists".into(),
        lifecycle: wire::Lifecycle::Active,
        execution: wire::Execution {
            state: wire::ExecutionState::Running,
            generation: Counter::new(1),
            reason: None,
        },
        attention: wire::Attention::Working,
        badges: vec![wire::Badge::UnsentDrafts],
        updated_at: timestamp(),
        revision: Counter::new(4),
        intent: wire::Intent {
            outcome: "hello.txt exists".into(),
            constraints: "none".into(),
            acceptance: "file says hi".into(),
        },
        intent_revision: Counter::new(1),
        integration_target: wire::TargetRef {
            branch: "main".into(),
            commit: sha("a"),
        },
        start_commit: sha("a"),
        branch: "lade/hello".into(),
        candidates: vec![wire::CandidateSummary {
            candidate_id: Id::parse(C).unwrap(),
            ordinal: Counter::new(1),
            commit: sha("b"),
            base_target_commit: sha("a"),
            status: wire::CandidateStatus::Current,
            files: Counter::new(1),
            added: Counter::new(1),
            removed: Counter::new(0),
            created_at: timestamp(),
        }],
        verifications: Vec::new(),
        change_counts: wire::ChangeCounts {
            files: Counter::new(1),
            added: Counter::new(1),
            removed: Counter::new(0),
        },
        unsent_drafts: Counter::new(0),
        available_actions: vec![
            wire::AvailableAction {
                action: wire::ActionName::MessageSend,
                enabled: true,
                reason: None,
            },
            wire::AvailableAction {
                action: wire::ActionName::ReviewIntegrate,
                enabled: false,
                reason: Some("no valid approval".into()),
            },
            wire::AvailableAction {
                action: wire::ActionName::TerminalOpen,
                enabled: false,
                reason: Some("Start the agent once to create this Objective's worktree.".into()),
            },
        ],
        session: Some(wire::SessionInfo {
            session_id: Id::parse(S).unwrap(),
            association_revision: Counter::new(1),
            execution_generation: Counter::new(1),
            model: lade_protocol::types::Model {
                provider: "lade-fixture".into(),
                id: "scripted".into(),
            },
            thinking: None,
        }),
        approval: None,
        history_deleted: None,
    }
}

#[test]
fn objective_detail_converts_field_by_field() {
    let detail = convert::detail(&wire_detail(O));
    assert_eq!(detail.summary.objective_id, oid());
    assert_eq!(detail.summary.lifecycle, view::Lifecycle::Active);
    assert_eq!(
        detail.summary.execution.state,
        view::ExecutionState::Running
    );
    assert_eq!(detail.summary.badges, vec![view::Badge::UnsentDrafts]);
    assert_eq!(
        detail.summary.updated_at,
        view::Timestamp(timestamp().unix_ms())
    );
    assert_eq!(detail.integration_target.label(), "main@aaaaaaa");
    assert_eq!(detail.current_candidate().unwrap().ordinal, 1);
    assert_eq!(
        detail.availability(view::ActionName::ReviewIntegrate),
        view::ActionAvailability::Disabled {
            reason: "no valid approval".into()
        }
    );
    // `terminal.open` is kept apart from the Objective actions.
    assert_eq!(detail.available_actions.len(), 2);
    assert_eq!(
        detail.terminal_availability(),
        view::ActionAvailability::Disabled {
            reason: "Start the agent once to create this Objective's worktree.".into()
        }
    );
    assert_eq!(
        detail.session.as_ref().unwrap().model,
        ModelRef::new("lade-fixture", "scripted")
    );
}

#[test]
fn references_and_paths_round_trip_losslessly() {
    let r = reference();
    assert_eq!(convert::reference(&convert::to_reference(&r).unwrap()), r);
    let raw = RepoPath::from_bytes(b"a/\xfe\xff".to_vec());
    let wire_path = convert::to_path(&raw).unwrap();
    assert_eq!(wire_path.as_bytes(), raw.bytes.as_slice());
    assert_eq!(convert::path(&wire_path), raw);
    for basis in [
        Basis::Working,
        Basis::Candidate {
            candidate_id: CandidateId::new(C),
        },
    ] {
        assert!(convert::to_basis(&basis).is_ok());
    }
}

#[test]
fn query_replies_carry_their_request_context() {
    let path = RepoPath::utf8("hello.txt");
    let context = Context::Diff(oid(), Basis::Working, path.clone());
    let effects = reply_effects(
        &context,
        ReplyData::DiffGet(wire::FileDiff {
            binary: false,
            hunks: vec![wire::DiffHunk {
                old_start: Counter::new(0),
                old_lines: Counter::new(0),
                new_start: Counter::new(1),
                new_lines: Counter::new(1),
                lines: vec![wire::DiffLine {
                    no_newline: true,
                    kind: wire::DiffLineKind::Add,
                    text: "hi".into(),
                    old_no: None,
                    new_no: Some(Counter::new(1)),
                    old_byte_start: None,
                    new_byte_start: Some(Counter::new(0)),
                }],
            }],
            truncated: false,
            old_digest: None,
            new_digest: Some(Digest::from_bytes([7; 32])),
        }),
    );
    let [StoreUpdate::Diff(diff)] = effects.updates.as_slice() else {
        panic!("{effects:?}")
    };
    assert_eq!(diff.path, path);
    assert_eq!(diff.basis, Basis::Working);
    assert_eq!(diff.hunks[0].lines[0].kind, view::DiffLineKind::Add);
    assert_eq!(diff.hunks[0].lines[0].new_byte_start, Some(0));
    assert!(diff.hunks[0].lines[0].no_newline);

    let page = reply_effects(
        &Context::File(oid(), Basis::Working, path.clone(), Side::New),
        ReplyData::FileRead(wire::FileContent {
            artifact_digest: Digest::from_bytes([1; 32]),
            total_bytes: Counter::new(3),
            offset: Counter::new(0),
            length: Counter::new(3),
            encoding: wire::FileEncoding::Utf8,
            eof: true,
            version_label: "Working capture".into(),
            text: Some("hi\n".into()),
            chunked: None,
        }),
    );
    assert!(matches!(
        page.updates.as_slice(),
        [StoreUpdate::FilePage(p)] if p.side == Side::New && p.eof && p.path == path
    ));
}

#[test]
fn objective_create_selects_and_loads_the_new_objective() {
    let effects = reply_effects(
        &Context::command("Create Objective", None),
        ReplyData::ObjectiveCreate(Box::new(wire_detail(O))),
    );
    assert!(
        effects
            .updates
            .iter()
            .any(|u| matches!(u, StoreUpdate::Select(o) if o == &oid()))
    );
    assert!(effects.follow_ups.contains(&Intent::LoadChanges {
        objective_id: oid(),
        basis: Basis::Working
    }));
}

/// The context `request` builds for `intent`.
fn context_of(intent: &Intent) -> Context {
    request(intent).unwrap().unwrap().1
}

#[test]
fn command_success_clears_and_rejection_reaches_only_its_objective_and_surface() {
    let context = context_of(&Intent::Freeze {
        objective_id: oid(),
    });
    let rejection = Rejection::new(
        ErrorCode::InvalidLifecycle,
        SafeAction::Refresh,
        "Wait until the agent has stopped.",
    );
    let effects = rejection_effects(&context, &rejection);
    let message = "Couldn't freeze for review. Wait until the agent has stopped.";
    assert!(effects.updates.contains(&StoreUpdate::Refused {
        objective_id: oid(),
        surface: ActionSurface::Review,
        refusal: Refusal {
            message: message.into(),
            details: "invalid_lifecycle: Wait until the agent has stopped.".into(),
        },
    }));
    // Shown on its surface only: no copy in the Objective's command note, no global notice.
    assert!(effects.updates.contains(&StoreUpdate::CommandStatus {
        objective_id: oid(),
        status: None,
    }));
    assert!(
        !effects
            .updates
            .iter()
            .any(|u| matches!(u, StoreUpdate::Notice(_)))
    );
    assert!(effects.follow_ups.contains(&Intent::LoadObjective {
        objective_id: oid()
    }));
    let ok = reply_effects(
        &context,
        ReplyData::ReviewFreeze(wire_detail(O).candidates[0].clone()),
    );
    assert!(ok.updates.contains(&StoreUpdate::CommandStatus {
        objective_id: oid(),
        status: None
    }));
    assert!(ok.updates.contains(&StoreUpdate::RefusalCleared {
        objective_id: oid(),
        surface: ActionSurface::Review,
    }));
}

#[test]
fn refusals_are_plain_sentences_with_the_code_kept_apart() {
    // A precondition: no code, no "Refresh" advice (refreshing would not help).
    let terminal = refusal(
        "open terminal",
        &Rejection::new(
            ErrorCode::InvalidLifecycle,
            SafeAction::Refresh,
            "Start the agent once to create this Objective's worktree.",
        ),
    );
    assert_eq!(
        terminal.message,
        "Couldn't open terminal. Start the agent once to create this Objective's worktree."
    );
    assert_eq!(
        terminal.details,
        "invalid_lifecycle: Start the agent once to create this Objective's worktree."
    );
    // Stale state: refreshing helps, and says so once.
    let stale = refusal(
        "approve",
        &Rejection::new(ErrorCode::StaleTarget, SafeAction::Refresh, ""),
    );
    assert_eq!(
        stale.message,
        "Couldn't approve. The Integration Target moved. Refresh and try again."
    );
    assert_eq!(stale.details, "stale_target");
    let worded = refusal(
        "edit intent",
        &Rejection::new(
            ErrorCode::StaleAssociation,
            SafeAction::Refresh,
            "The Agent Session changed. Refresh and try again.",
        ),
    );
    assert_eq!(
        worded.message,
        "Couldn't edit intent. The Agent Session changed. Refresh and try again."
    );
    // No detail falls back to the code's wording; a lowercase service detail becomes a sentence.
    let bare = refusal(
        "start agent",
        &Rejection::new(
            ErrorCode::ExecutionUnavailable,
            SafeAction::ExplicitContinue,
            "",
        ),
    );
    assert_eq!(
        bare.message,
        "Couldn't start agent. The agent isn't running."
    );
    let lower = refusal(
        "send terminal input",
        &Rejection::new(
            ErrorCode::NotFound,
            SafeAction::Refresh,
            "terminal not found",
        ),
    );
    assert_eq!(
        lower.message,
        "Couldn't send terminal input. Terminal not found."
    );
    assert_eq!(
        rejection_message("check the outcome of “Approve”", &stale_rejection()),
        "Couldn't check the outcome of “Approve”. The Integration Target moved. Refresh and try again."
    );
    // Every code reads without its spelling.
    for code in [
        ErrorCode::Unauthorized,
        ErrorCode::DurabilityFailure,
        ErrorCode::LimitReached,
        ErrorCode::Conflict,
        ErrorCode::VersionMismatch,
    ] {
        let message = rejection_message("approve", &Rejection::new(code, SafeAction::None, ""));
        assert!(!message.contains(convert::error_code(code)), "{message}");
        assert!(!message.contains('_'), "{message}");
    }
}

fn stale_rejection() -> Rejection {
    Rejection::new(ErrorCode::StaleTarget, SafeAction::Refresh, "")
}

#[test]
fn commands_without_an_objective_still_use_the_notice() {
    let effects = rejection_effects(
        &context_of(&Intent::UpdateSettings {
            default_model: ModelRef::new("lade-fixture", "scripted"),
        }),
        &Rejection::new(ErrorCode::InvalidRequest, SafeAction::None, "unknown model"),
    );
    assert!(matches!(
        effects.updates.as_slice(),
        [StoreUpdate::Notice(n)] if n.message == "Couldn't save default model. Unknown model."
    ));
}

#[test]
fn a_failed_workspace_open_is_scoped_and_explains_the_folder() {
    let effects = rejection_effects(
        &context_of(&Intent::OpenWorkspace {
            path: PathBuf::from("/tmp/repo/.git"),
        }),
        &Rejection::new(
            ErrorCode::InvalidRequest,
            SafeAction::None,
            "not a repository",
        ),
    );
    let [StoreUpdate::Notice(notice)] = effects.updates.as_slice() else {
        panic!("one notice: {:?}", effects.updates);
    };
    assert_eq!(notice.scope, NoticeScope::WorkspaceOpen);
    assert_eq!(notice.code, "invalid_request");
    assert!(
        notice
            .message
            .starts_with("Couldn't open workspace. Not a repository.")
    );
    assert!(notice.message.ends_with("It needs at least one commit."));
    let other = rejection_effects(
        &context_of(&Intent::LoadResources),
        &Rejection::new(ErrorCode::InvalidRequest, SafeAction::None, "bad"),
    );
    assert!(matches!(
        other.updates.as_slice(),
        [StoreUpdate::Notice(n)] if n.scope == NoticeScope::General
    ));
}

#[test]
fn stale_reference_names_the_stale_drafts() {
    let context = Context::Command {
        label: "Submit feedback",
        objective_id: Some(oid()),
        draft_id: None,
        batch: vec![DraftId::new(D), DraftId::new(C)],
        surface: Some(ActionSurface::Feedback),
    };
    let rejection = Rejection::new(
        ErrorCode::StaleReference,
        SafeAction::Refresh,
        "a feedback draft refers to code that changed",
    )
    .with_stale_drafts(vec![Id::parse(D).unwrap()]);
    let effects = rejection_effects(&context, &rejection);
    let message = effects
        .updates
        .iter()
        .find_map(|u| match u {
            StoreUpdate::Refused {
                surface: ActionSurface::Feedback,
                refusal,
                ..
            } => Some(refusal.message.clone()),
            _ => None,
        })
        .unwrap();
    assert!(message.contains("Stale draft: 3b1c2d3e."), "{message}");
    assert!(
        !message.contains(D),
        "draft ids stay in the details: {message}"
    );
    assert!(effects.follow_ups.contains(&Intent::LoadDrafts {
        objective_id: oid()
    }));
}

#[test]
fn refused_draft_save_keeps_text_and_reports_beside_the_draft() {
    let context = Context::Command {
        label: "Save draft",
        objective_id: Some(oid()),
        draft_id: Some(DraftId::new(D)),
        batch: Vec::new(),
        surface: Some(ActionSurface::Drafts),
    };
    let effects = rejection_effects(
        &context,
        &Rejection::new(
            ErrorCode::DurabilityFailure,
            SafeAction::QueryOperation,
            "The draft was not written (injected test fault).",
        ),
    );
    // Shown once, beside the draft: no Objective status, no notice.
    assert_eq!(
        effects.updates,
        vec![StoreUpdate::DraftSaveRefused {
            draft_id: DraftId::new(D),
            refusal: Refusal {
                message: "The draft was not written (injected test fault).".into(),
                details: "durability_failure: The draft was not written (injected test fault)."
                    .into(),
            },
        }]
    );
    let deleted = rejection_effects(
        &context_of(&Intent::DeleteDraft {
            draft_id: DraftId::new(D),
            expected_revision: 2,
        }),
        &Rejection::new(
            ErrorCode::Conflict,
            SafeAction::Refresh,
            "The draft was changed elsewhere. Refresh and try again.",
        ),
    );
    assert_eq!(
        deleted.updates,
        vec![StoreUpdate::DraftDeleteRefused {
            draft_id: DraftId::new(D),
            refusal: Refusal {
                message:
                    "Couldn't delete draft. The draft was changed elsewhere. Refresh and try again."
                        .into(),
                details: "conflict: The draft was changed elsewhere. Refresh and try again.".into(),
            },
        }]
    );
}

#[test]
fn settings_offer_known_models_with_the_default_selected() {
    let settings = convert::settings(&wire::Settings {
        verification_commands: vec![vec!["git".into(), "diff".into(), "--check".into()]],
        default_model: Some(lade_protocol::types::Model {
            provider: "lade-fixture".into(),
            id: "scripted".into(),
        }),
    });
    assert_eq!(
        settings.default_model,
        Some(ModelRef::new("lade-fixture", "scripted"))
    );
}

#[test]
fn agent_session_replies_and_events_reach_the_store() {
    let models = reply_effects(
        &Context::Models(Some("lade-fixture".into())),
        ReplyData::ModelsList(wire::ModelCatalog {
            sdk_version: "1.5.0".into(),
            providers: vec![wire::ProviderInfo {
                provider: "openai-codex".into(),
                name: "OpenAI Codex".into(),
                auth: vec![wire::ProviderAuth::OauthLogin],
                status: wire::ProviderStatus::Restricted,
                models: Counter::new(8),
                reason: Some("interactive OAuth login is restricted".into()),
                variables: None,
            }],
            models: vec![wire::ModelInfo {
                provider: "lade-fixture".into(),
                id: "scripted".into(),
                name: "Scripted".into(),
                reasoning: false,
                thinking_levels: vec![lade_protocol::types::ThinkingLevel::Off],
            }],
            truncated: false,
            error: None,
        }),
    );
    assert!(matches!(
        models.updates.as_slice(),
        [
            StoreUpdate::AgentSession(AgentSessionUpdate::Catalog(c)),
            StoreUpdate::AgentSession(AgentSessionUpdate::Models { provider, models }),
        ] if c.providers[0].status == crate::presentation::state::agent_session::ProviderStatus::Restricted
            && c.providers[0].reason.is_some()
            && provider == "lade-fixture"
            && models.len() == 1
    ));
    let state = wire::SessionState {
        objective_id: Id::parse(O).unwrap(),
        generation: Counter::new(2),
        reason: lade_protocol::types::SessionChangeReason::Fork,
        model: lade_protocol::types::Model {
            provider: "lade-fixture".into(),
            id: "scripted".into(),
        },
        thinking: lade_protocol::types::ThinkingLevel::Off,
        session_id: None,
        association_revision: Some(Counter::new(2)),
        pi_session_id: Some("pi".into()),
        leaf_id: None,
    };
    let forked = reply_effects(
        &Context::command("Fork session", Some(&oid())),
        ReplyData::SessionFork(state.clone()),
    );
    assert!(forked.updates.iter().any(|u| matches!(
        u,
        StoreUpdate::AgentSession(AgentSessionUpdate::Session { session, .. }) if session.generation == 2
    )));
    assert!(
        forked
            .follow_ups
            .iter()
            .any(|i| matches!(i, Intent::LoadSessionTree { .. }))
    );
    let changed = crate::transport::core::event_updates(wire::Event::SessionChanged(state));
    assert!(matches!(
        changed.as_slice(),
        [StoreUpdate::AgentSession(
            AgentSessionUpdate::Session { .. }
        )]
    ));
    let opened = crate::transport::core::event_updates(wire::Event::UiRequest(wire::UiRequest {
        objective_id: Id::parse(O).unwrap(),
        ui_id: Id::parse(D).unwrap(),
        generation: Counter::new(2),
        prompt: lade_protocol::types::UiPrompt::Confirm {
            title: "Proceed?".into(),
            message: "m".into(),
        },
    }));
    assert!(matches!(
        opened.as_slice(),
        [StoreUpdate::AgentSession(AgentSessionUpdate::DialogOpened(d))] if d.generation == 2
    ));
    let snapshot = reply_effects(
        &Context::Snapshot,
        ReplyData::Snapshot(wire::SnapshotData {
            service_epoch: Id::parse(O).unwrap(),
            sequence: Counter::new(0),
            workspaces: Vec::new(),
            objectives: Vec::new(),
            settings: wire::Settings {
                verification_commands: Vec::new(),
                default_model: None,
            },
        }),
    );
    assert!(
        snapshot
            .follow_ups
            .contains(&Intent::LoadModels { provider: None })
    );
    assert!(snapshot.follow_ups.contains(&Intent::LoadResources));
}

#[test]
fn terminal_replies_and_refusals_land_in_the_terminal_pane() {
    use crate::presentation::terminal::{TerminalEnd, TerminalMode as ViewMode};
    let context = Context::Terminal(oid(), "Open terminal");
    let effects = reply_effects(
        &context,
        ReplyData::TerminalOpen(wire::TerminalInfo {
            terminal_id: Id::parse(T.to_owned()).unwrap(),
            objective_id: Id::parse(O.to_owned()).unwrap(),
            mode: wire::TerminalMode::ReadOnly,
            cols: Counter::new(80),
            rows: Counter::new(24),
        }),
    );
    assert_eq!(
        effects.updates,
        vec![StoreUpdate::TerminalOpened {
            objective_id: oid(),
            terminal_id: T.into(),
            mode: ViewMode::ReadOnly,
            cols: 80,
            rows: 24,
        }]
    );
    let refused = rejection_effects(
        &context,
        &wire::Rejection::new(
            wire::ErrorCode::InvalidLifecycle,
            wire::SafeAction::None,
            "Start the agent once to create this Objective's worktree.",
        ),
    );
    assert_eq!(
        refused.updates,
        vec![StoreUpdate::TerminalRefused {
            objective_id: oid(),
            refusal: Refusal {
                message: "Couldn't open terminal. Start the agent once to create this Objective's worktree.".into(),
                details: "invalid_lifecycle: Start the agent once to create this Objective's worktree.".into(),
            },
        }]
    );
    let stopped = reply_effects(&Context::Stop, ReplyData::ServiceStop(wire::Empty {}));
    assert_eq!(stopped.updates, vec![StoreUpdate::ServiceStopped]);
    let closed =
        crate::transport::core::event_updates(wire::Event::TerminalClosed(wire::TerminalClosed {
            objective_id: Id::parse(O.to_owned()).unwrap(),
            terminal_id: Id::parse(T.to_owned()).unwrap(),
            reason: wire::TerminalCloseReason::ReviewFreeze,
            exit_code: None,
            signal: None,
        }));
    assert_eq!(
        closed,
        vec![StoreUpdate::TerminalClosed {
            terminal_id: T.into(),
            end: TerminalEnd::ReviewFreeze,
        }]
    );
}
