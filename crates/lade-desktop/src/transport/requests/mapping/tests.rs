use std::collections::BTreeSet;
use std::path::PathBuf;

use lade_protocol::client::{self as wire, ClientBody, ClientEnvelope, RequestFrame, RequestName};
use lade_protocol::types::{Counter, GitAlgorithm, GitId};
use lade_protocol::{Framing, encode};

use crate::presentation::controller::intents::DraftRevisionRef;
use crate::presentation::controller::intents::StopReason;
use crate::presentation::state::agent_session::{ForkPosition, ThinkingLevel, UiAnswer};
use crate::presentation::state::model::{
    ApprovalId, CandidateId, DeliveryMode, ModelRef, SessionId, WorkspaceId,
};
use crate::transport::requests::tests::*;
use crate::transport::requests::*;

/// Intents that open the workspace, load views, search and create Objectives.
fn workspace_and_load_intents() -> Vec<(Intent, Option<RequestName>)> {
    use Intent as I;
    use RequestName as N;
    let candidate = || Basis::Candidate {
        candidate_id: CandidateId::new(C),
    };
    vec![
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
    ]
}

/// Intents that change an Objective's intent or run its agent.
fn objective_intents() -> Vec<(Intent, Option<RequestName>)> {
    use Intent as I;
    use RequestName as N;
    vec![
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
    ]
}

/// Intents on drafts, feedback, the review and closing.
fn review_intents() -> Vec<(Intent, Option<RequestName>)> {
    use Intent as I;
    use RequestName as N;
    vec![
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
    ]
}

/// Intents on settings, models, the pi session, profile resources and dialogs.
fn session_intents() -> Vec<(Intent, Option<RequestName>)> {
    use Intent as I;
    use RequestName as N;
    vec![
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
                answer: UiAnswer::Confirm(false),
            },
            Some(N::UiRespond),
        ),
    ]
}

/// Intents on terminals, the service and Git history.
fn terminal_and_service_intents() -> Vec<(Intent, Option<RequestName>)> {
    use Intent as I;
    use RequestName as N;
    vec![
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
                reason: StopReason::Logout,
            },
            Some(N::ServiceStop),
        ),
        (
            I::DeleteHistory {
                objective_id: oid(),
            },
            Some(N::ObjectiveDeleteHistory),
        ),
    ]
}

fn every_intent() -> Vec<(Intent, Option<RequestName>)> {
    use Intent as I;
    let all: Vec<_> = [
        workspace_and_load_intents(),
        objective_intents(),
        review_intents(),
        session_intents(),
        terminal_and_service_intents(),
    ]
    .concat();
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
        match &context {
            Context::Command {
                label,
                objective_id,
                surface,
                ..
            } => {
                assert_eq!(*label, intent.label());
                assert_eq!(
                    objective_id.is_some(),
                    surface.is_some(),
                    "a command on an Objective is refused on its surface: {intent:?}"
                );
            }
            Context::DraftDelete { label, .. } => {
                assert!(matches!(intent, Intent::DeleteDraft { .. }), "{intent:?}");
                assert_eq!(*label, intent.label());
            }
            _ => {}
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
fn invalid_view_values_are_refused_before_sending() {
    let err = request(&Intent::LoadObjective {
        objective_id: ObjectiveId::new("not-a-uuid"),
    })
    .unwrap_err();
    assert!(err.0.contains("objective id"));
}

#[test]
fn intents_without_a_request_map_to_none() {
    assert_eq!(request(&Intent::RetryConnection), Ok(None));
}

#[test]
fn agent_session_text_fields_are_bounded_by_characters() {
    let fork = |entry_id: String| Intent::ForkSession {
        objective_id: oid(),
        entry_id,
        position: ForkPosition::Before,
    };
    assert_eq!(
        request(&fork(String::new())).unwrap_err().0,
        "the entry id must be 1 to 256 characters"
    );
    assert!(
        request(&fork("é".repeat(256))).is_ok(),
        "256 characters, 512 bytes"
    );
    assert!(request(&fork("é".repeat(257))).is_err());
    let decide = |name: &str, sha256: &str| Intent::DecideResource {
        name: name.into(),
        sha256: sha256.into(),
        approve: true,
    };
    let digest = "a".repeat(64);
    assert!(request(&decide("ask.js", &digest)).is_ok());
    assert_eq!(
        request(&decide("", &digest)).unwrap_err().0,
        "the extension name must be 1 to 256 characters"
    );
    assert_eq!(
        request(&decide("ask.js", "not-a-digest")).unwrap_err().0,
        "the extension hash is not a sha256 digest"
    );
}

#[test]
fn paths_that_cannot_be_sent_are_refused() {
    assert_eq!(
        request(&Intent::OpenWorkspace {
            path: PathBuf::new()
        })
        .unwrap_err()
        .0,
        "the folder path cannot be sent (empty or over 4096 bytes)"
    );
    assert_eq!(
        request(&Intent::ImportProfile {
            source: PathBuf::from("x".repeat(4097))
        })
        .unwrap_err()
        .0,
        "the path cannot be sent (empty or over 4096 bytes)"
    );
}

#[test]
fn commands_record_the_surface_they_were_issued_from() {
    let surface_of = |intent: &Intent| match request(intent).unwrap().unwrap().1 {
        Context::Command { surface, .. } => surface,
        other => panic!("a command context: {other:?}"),
    };
    assert_eq!(
        surface_of(&Intent::Freeze {
            objective_id: oid()
        }),
        Some(ActionSurface::Review)
    );
    assert_eq!(
        surface_of(&Intent::UpdateSettings {
            default_model: ModelRef::new("p", "m")
        }),
        None
    );
}
