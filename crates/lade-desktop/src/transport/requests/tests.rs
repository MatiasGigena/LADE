use lade_protocol::client::{self as wire, ReplyData};
use lade_protocol::types::{Counter, GitAlgorithm, GitId, Id, Timestamp};

use super::*;
use crate::presentation::state::model::{ArtifactDigest, CaptureId, Reference, ReferenceVersion};
use crate::transport::convert;

pub(crate) const O: &str = "0b1c2d3e-4f50-4a61-8b72-839405a6b7c8";
pub(crate) const W: &str = "1b1c2d3e-4f50-4a61-8b72-839405a6b7c8";
pub(crate) const C: &str = "2b1c2d3e-4f50-4a61-8b72-839405a6b7c8";
pub(crate) const D: &str = "3b1c2d3e-4f50-4a61-8b72-839405a6b7c8";
pub(crate) const S: &str = "4b1c2d3e-4f50-4a61-8b72-839405a6b7c8";
pub(crate) const A: &str = "5b1c2d3e-4f50-4a61-8b72-839405a6b7c8";
pub(crate) const T: &str = "7b1c2d3e-4f50-4a61-8b72-839405a6b7c8";
pub(crate) const CAPTURE: &str = "6b1c2d3e-4f50-4a61-8b72-839405a6b7c8";

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

pub(crate) fn reference() -> Reference {
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

pub(crate) fn test_id(n: u8) -> Id {
    Id::from_random_bytes([n; 16])
}

#[test]
fn start_point_parses_target_and_both_object_formats() {
    assert_eq!(start_point(&StartPoint::Target), Ok(wire::Start::Target));
    assert!(
        start_point(&StartPoint::Commit("target".into())).is_err(),
        "the target is StartPoint::Target, never a commit spelled \"target\""
    );
    let sha256 = "c".repeat(64);
    assert!(matches!(
        start_point(&StartPoint::Commit(sha256)),
        Ok(wire::Start::Commit { commit }) if commit.algorithm() == GitAlgorithm::Sha256
    ));
    assert!(start_point(&StartPoint::Commit("abc123".into())).is_err());
    assert!(start_point(&StartPoint::Commit("g".repeat(40))).is_err());
}

pub(crate) fn timestamp() -> Timestamp {
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

pub(crate) fn context_of(intent: &Intent) -> Context {
    request(intent).unwrap().unwrap().1
}

#[test]
fn history_pages_carry_their_cursor_and_the_page_limit() {
    let Ok(Request::DraftsList(drafts)) = drafts_page(&oid(), None) else {
        panic!("drafts page");
    };
    assert_eq!(drafts.limit.value(), HISTORY_PAGE);
    assert!(drafts.after.is_none());
    let Ok(Request::InputsList(inputs)) = inputs_page(&oid(), Some(Counter::new(7))) else {
        panic!("inputs page");
    };
    assert_eq!(inputs.limit.value(), HISTORY_PAGE);
    assert_eq!(inputs.after, Some(Counter::new(7)));
    assert_eq!(
        drafts_page(&ObjectiveId::new("not-a-uuid"), None),
        Err(Invalid("invalid objective id".into()))
    );
}

pub(crate) fn wire_draft(byte: u8) -> wire::Draft {
    wire::Draft {
        draft_id: Id::from_random_bytes([byte; 16]),
        objective_id: Id::parse(O).unwrap(),
        destination_session_id: Id::from_random_bytes([99; 16]),
        reference: convert::to_reference(&reference()).unwrap(),
        body: format!("draft {byte}"),
        revision: Counter::new(1),
        state: wire::DraftState::Unsent,
        stale: false,
        input_id: None,
    }
}

pub(crate) fn input_summary(operation_id: &Id) -> wire::InputSummary {
    wire::InputSummary {
        input_id: Id::from_random_bytes([0x77; 16]),
        objective_id: Id::parse(O).unwrap(),
        operation_id: operation_id.clone(),
        mode: lade_protocol::types::DeliveryMode::Prompt,
        text_preview: "lade-fixture: write".into(),
        accepted_at: timestamp(),
        delivery: wire::DeliveryState::Queued,
        blocked_reason: None,
    }
}
