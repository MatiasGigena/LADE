//! Shared values for the client DTO tests, and a round trip that checks a value against the
//! checked-in contract.

use super::{
    ActionName, Attention, AvailableAction, Badge, ChangeCounts, Execution, ExecutionState, Intent,
    Lifecycle, ObjectiveDetail, TargetRef,
};
use crate::schema::{self, Contract};
use crate::types::{Counter, Digest, GitId, Id, Timestamp, WirePath};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::fmt::Debug;

pub(super) fn id(n: u8) -> Id {
    Id::from_random_bytes([n; 16])
}

pub(super) fn counter(n: u64) -> Counter {
    Counter::new(n)
}

pub(super) fn ts(ms: i64) -> Timestamp {
    Timestamp::from_unix_ms(ms).expect("fixture timestamp")
}

pub(super) fn git(n: u8) -> GitId {
    GitId::from_hex(&format!("{n:02x}").repeat(20)).expect("fixture git id")
}

pub(super) fn digest(n: u8) -> Digest {
    Digest::from_bytes([n; 32])
}

pub(super) fn path(text: &str) -> WirePath {
    WirePath::from_bytes(text.as_bytes()).expect("fixture path")
}

/// Serializes `value`, checks the JSON against the client contract's definition `name`, decodes it
/// back to an equal value and returns the JSON.
pub(super) fn round_trip<T>(value: &T, name: &str) -> Value
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let json = serde_json::to_value(value).expect("serialize");
    assert!(
        schema::conforms(&json, Contract::Client, &format!("/$defs/{name}")),
        "{name} does not match the contract: {json}"
    );
    let back: T = serde_json::from_value(json.clone()).expect("deserialize");
    assert_eq!(&back, value);
    json
}

/// A frozen Objective with one disabled action and every optional field left out.
pub(super) fn objective_detail() -> ObjectiveDetail {
    ObjectiveDetail {
        objective_id: id(1),
        workspace_id: id(2),
        title: "Fix login".into(),
        outcome: "Users can log in".into(),
        lifecycle: Lifecycle::Frozen,
        execution: Execution {
            state: ExecutionState::Cancelled,
            generation: counter(2),
            reason: None,
        },
        attention: Attention::ReadyForReview,
        badges: vec![Badge::UnsentDrafts],
        updated_at: ts(1_000),
        revision: counter(7),
        intent: Intent {
            outcome: "Users can log in".into(),
            constraints: String::new(),
            acceptance: "Tests pass".into(),
        },
        intent_revision: counter(1),
        integration_target: TargetRef {
            branch: "main".into(),
            commit: git(1),
        },
        start_commit: git(1),
        branch: "lade/01010101".into(),
        candidates: Vec::new(),
        verifications: Vec::new(),
        change_counts: ChangeCounts {
            files: counter(1),
            added: counter(3),
            removed: counter(0),
        },
        unsent_drafts: counter(1),
        available_actions: vec![AvailableAction {
            action: ActionName::ReviewApprove,
            enabled: false,
            reason: Some("Run the checks first.".into()),
        }],
        session: None,
        approval: None,
        history_deleted: None,
    }
}
