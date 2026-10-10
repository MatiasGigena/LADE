//! Deterministic fixtures shared by unit tests.

use crate::domain::execution::{ExecutionGeneration, ModelRef};
use crate::domain::ids::{
    ApprovalId, CandidateId, CaptureId, DraftId, GitAlgorithm, GitId, InputId, ObjectiveId,
    OperationId, SessionId, Timestamp, Uuid, VerificationId, WorkspaceId,
};
use crate::domain::inspection::{
    ArtifactDigest, ByteRange, InspectedVersion, Reference, RepoPath, Side,
};
use crate::domain::objectives::{BranchName, Intent, NewObjective, Objective, Title};
use crate::error::DomainError;

pub fn uuid(n: u128) -> Uuid {
    Uuid::from_random_bytes(n.to_be_bytes())
}

macro_rules! fixture_ids {
    ($($function:ident => $type:ident),+ $(,)?) => {$(
        pub fn $function(n: u128) -> $type {
            $type::new(uuid(n))
        }
    )+};
}

fixture_ids!(
    objective_id => ObjectiveId,
    workspace_id => WorkspaceId,
    session_id => SessionId,
    draft_id => DraftId,
    input_id => InputId,
    candidate_id => CandidateId,
    verification_id => VerificationId,
    approval_id => ApprovalId,
    operation_id => OperationId,
    capture_id => CaptureId,
);

pub fn ts(millis: i64) -> Timestamp {
    Timestamp::from_unix_ms(millis)
}

pub fn git(n: u128) -> GitId {
    GitId::new(GitAlgorithm::Sha1, &format!("{n:040x}")).expect("fixture git id")
}

pub fn digest(n: u128) -> ArtifactDigest {
    ArtifactDigest::parse(&format!("{n:064x}")).expect("fixture digest")
}

pub fn model() -> ModelRef {
    ModelRef::new("lade-fixture", "scripted").expect("fixture model")
}

/// Reference into `src/main.rs` of working capture 1, with artifact digest `digest(n)`.
pub fn reference_at(
    n: u128,
    start: usize,
    end: usize,
    selected: &str,
) -> Result<Reference, DomainError> {
    let range = ByteRange::new(start as u64, end as u64)?;
    Reference::new(
        digest(n),
        RepoPath::new(b"src/main.rs".to_vec())?,
        InspectedVersion::Working {
            capture_id: capture_id(1),
        },
        Side::New,
        range,
        selected.to_string(),
        1,
    )
}

pub fn defined_objective() -> Objective {
    let new = NewObjective {
        id: objective_id(1),
        workspace_id: workspace_id(1),
        title: Title::new("Fix login").expect("title"),
        intent: Intent::new("Users can log in", "No new dependencies", "Tests pass")
            .expect("intent"),
        integration_target: BranchName::new("main").expect("branch"),
        start_commit: git(1),
    };
    Objective::define(new, ts(1))
}

/// Active Objective whose first execution is ready and idle.
pub fn active_objective() -> (Objective, ExecutionGeneration) {
    let mut objective = defined_objective();
    let generation = objective
        .start_execution(session_id(1), model(), ts(2))
        .expect("start");
    objective
        .observe_worker_ready(generation, ts(2))
        .expect("ready");
    (objective, generation)
}

/// Active Objective whose execution was cancelled (writers excluded), ready to freeze.
pub fn stopped_objective() -> Objective {
    let (mut objective, generation) = active_objective();
    objective.request_stop(ts(3)).expect("stop");
    objective
        .observe_stopped(generation, ts(3))
        .expect("stopped");
    objective
}
