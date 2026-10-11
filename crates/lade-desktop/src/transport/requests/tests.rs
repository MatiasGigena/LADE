use std::collections::BTreeSet;
use std::path::PathBuf;

use lade_protocol::client::{
    self as wire, ClientBody, ClientEnvelope, ErrorCode, Rejection, ReplyData, RequestFrame,
    RequestName, SafeAction,
};
use lade_protocol::types::{Counter, Digest, GitAlgorithm, GitId, Id, Timestamp};
use lade_protocol::{Framing, encode};

use super::*;
use crate::presentation::intents::DraftRevisionRef;
use crate::presentation::model::{
    ApprovalId, ArtifactDigest, CandidateId, CaptureId, DeliveryMode, ModelRef, Reference,
    ReferenceVersion, SessionId, WorkspaceId,
};

pub(crate) const O: &str = "0b1c2d3e-4f50-4a61-8b72-839405a6b7c8";
pub(crate) const W: &str = "1b1c2d3e-4f50-4a61-8b72-839405a6b7c8";
const C: &str = "2b1c2d3e-4f50-4a61-8b72-839405a6b7c8";
const D: &str = "3b1c2d3e-4f50-4a61-8b72-839405a6b7c8";
const S: &str = "4b1c2d3e-4f50-4a61-8b72-839405a6b7c8";
const A: &str = "5b1c2d3e-4f50-4a61-8b72-839405a6b7c8";
const CAPTURE: &str = "6b1c2d3e-4f50-4a61-8b72-839405a6b7c8";

fn oid() -> ObjectiveId {
    ObjectiveId::new(O)
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

/// One instance of every intent, paired with the request name it must produce.
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
    ];
    // Compile-time reminder: a new intent variant must be added above.
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
            | I::UpdateSettings { .. } => {}
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
        // snapshot/operation.get are issued by the connection itself.
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
    // Typed hex is normalised; length selects the object format.
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
        badges: vec![wire::Badge::FixtureModel],
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
    assert_eq!(detail.summary.badges, vec![view::Badge::FixtureModel]);
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

#[test]
fn command_success_clears_and_rejection_sets_the_status_beside_actions() {
    let context = Context::command("Freeze for review", Some(&oid()));
    let rejection = Rejection::new(
        ErrorCode::InvalidLifecycle,
        SafeAction::Refresh,
        "the agent is still running",
    );
    let effects = rejection_effects(&context, &rejection);
    let status = effects
        .updates
        .iter()
        .find_map(|u| match u {
            StoreUpdate::CommandStatus {
                status: Some(s), ..
            } => Some(s.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(status.code, "invalid_lifecycle");
    assert_eq!(
        status.message,
        "Freeze for review was refused (invalid_lifecycle) — the agent is still running. Refresh and try again."
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
}

#[test]
fn stale_reference_names_the_stale_drafts() {
    let context = Context::Command {
        label: "Submit feedback",
        objective_id: Some(oid()),
        draft_id: None,
        batch: vec![DraftId::new(D), DraftId::new(C)],
    };
    let rejection = Rejection::new(
        ErrorCode::StaleReference,
        SafeAction::Refresh,
        &format!("stale drafts: {D}"),
    );
    let effects = rejection_effects(&context, &rejection);
    let notice = effects
        .updates
        .iter()
        .find_map(|u| match u {
            StoreUpdate::Notice(n) => Some(n.message.clone()),
            _ => None,
        })
        .unwrap();
    assert!(notice.contains("Stale draft: 3b1c2d3e."), "{notice}");
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
    };
    let effects = rejection_effects(
        &context,
        &Rejection::new(ErrorCode::Conflict, SafeAction::Refresh, "revision moved"),
    );
    assert!(effects.updates.contains(&StoreUpdate::DraftSaveFailed {
        draft_id: DraftId::new(D),
        message: "conflict: revision moved".into()
    }));
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
    assert!(settings.available_models.len() > 1);
}
