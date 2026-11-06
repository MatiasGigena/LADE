//! Objective summaries and details, with their execution, candidates, checks and approval.

use super::{
    ActionName, ApprovalState, Attention, Badge, CandidateStatus, ExecutionState, Lifecycle,
    VerificationState,
};
use crate::types::{ActivityRole, Counter, GitId, Id, Model, ThinkingLevel, Timestamp, WirePath};
use serde::{Deserialize, Serialize};

wire_struct!(Empty {});
wire_struct!(Intent {
    outcome: String,
    constraints: String,
    acceptance: String,
});
wire_struct!(Settings {
    verification_commands: Vec<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_model: Option<Model>,
});
wire_struct!(Workspace {
    workspace_id: Id,
    path: WirePath,
    name: String,
    branches: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_branch: Option<String>,
});
wire_struct!(TargetRef {
    branch: String,
    commit: GitId,
});
wire_struct!(Execution {
    state: ExecutionState,
    generation: Counter,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
});
wire_struct!(ObjectiveSummary {
    objective_id: Id,
    workspace_id: Id,
    title: String,
    outcome: String,
    lifecycle: Lifecycle,
    execution: Execution,
    attention: Attention,
    badges: Vec<Badge>,
    updated_at: Timestamp,
    revision: Counter,
});
wire_struct!(SessionInfo {
    session_id: Id,
    association_revision: Counter,
    execution_generation: Counter,
    model: Model,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingLevel>,
});
wire_struct!(ChangeCounts {
    files: Counter,
    added: Counter,
    removed: Counter,
});
wire_struct!(AvailableAction {
    action: ActionName,
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
});
wire_struct!(CandidateSummary {
    candidate_id: Id,
    ordinal: Counter,
    commit: GitId,
    base_target_commit: GitId,
    status: CandidateStatus,
    files: Counter,
    added: Counter,
    removed: Counter,
    created_at: Timestamp,
});
wire_struct!(VerificationSummary {
    verification_id: Id,
    objective_id: Id,
    candidate_id: Id,
    command: Vec<String>,
    state: VerificationState,
    started_at: Timestamp,
    output_tail: String,
    truncated: bool,
    provenance: Provenance,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<Counter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ended_at: Option<Timestamp>,
});
wire_enum!(Provenance { ServiceObserved });
wire_struct!(ApprovalSummary {
    approval_id: Id,
    candidate_id: Id,
    target_commit: GitId,
    state: ApprovalState,
    #[serde(skip_serializing_if = "Option::is_none")]
    override_reason: Option<String>,
});
wire_struct!(ObjectiveDetail {
    objective_id: Id,
    workspace_id: Id,
    title: String,
    outcome: String,
    lifecycle: Lifecycle,
    execution: Execution,
    attention: Attention,
    badges: Vec<Badge>,
    updated_at: Timestamp,
    revision: Counter,
    intent: Intent,
    intent_revision: Counter,
    integration_target: TargetRef,
    start_commit: GitId,
    branch: String,
    candidates: Vec<CandidateSummary>,
    verifications: Vec<VerificationSummary>,
    change_counts: ChangeCounts,
    unsent_drafts: Counter,
    available_actions: Vec<AvailableAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<SessionInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    approval: Option<ApprovalSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    history_deleted: Option<bool>,
});
impl ObjectiveDetail {
    #[must_use]
    pub fn summary(&self) -> ObjectiveSummary {
        ObjectiveSummary {
            objective_id: self.objective_id.clone(),
            workspace_id: self.workspace_id.clone(),
            title: self.title.clone(),
            outcome: self.outcome.clone(),
            lifecycle: self.lifecycle,
            execution: self.execution.clone(),
            attention: self.attention,
            badges: self.badges.clone(),
            updated_at: self.updated_at.clone(),
            revision: self.revision.clone(),
        }
    }
}
wire_struct!(ActivityItem {
    item_id: Id,
    seq: Counter,
    at: Timestamp,
    role: ActivityRole,
    title: String,
    text: String,
    is_error: bool,
    truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_name: Option<String>,
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::fixtures::{counter, git, id, objective_detail, path, round_trip, ts};
    use serde_json::json;

    #[test]
    fn a_detail_and_its_summary_match_the_contract_and_share_every_summary_field() {
        let detail = objective_detail();
        let detail_json = round_trip(&detail, "ObjectiveDetail");
        let summary_json = round_trip(&detail.summary(), "ObjectiveSummary");
        let summary = summary_json.as_object().expect("object");
        assert_eq!(summary.len(), 10);
        for (key, value) in summary {
            assert_eq!(detail_json.get(key), Some(value), "{key}");
        }
    }

    #[test]
    fn optional_detail_fields_are_omitted_until_set() {
        let mut detail = objective_detail();
        let json = round_trip(&detail, "ObjectiveDetail");
        for key in ["session", "approval", "history_deleted"] {
            assert!(json.get(key).is_none(), "{key}");
        }
        assert!(json["execution"].get("reason").is_none());
        detail.session = Some(SessionInfo {
            session_id: id(3),
            association_revision: counter(1),
            execution_generation: counter(2),
            model: Model {
                provider: "anthropic".into(),
                id: "model".into(),
            },
            thinking: Some(ThinkingLevel::High),
        });
        detail.approval = Some(ApprovalSummary {
            approval_id: id(4),
            candidate_id: id(5),
            target_commit: git(1),
            state: ApprovalState::Valid,
            override_reason: None,
        });
        detail.history_deleted = Some(true);
        let json = round_trip(&detail, "ObjectiveDetail");
        assert_eq!(json["session"]["thinking"], json!("high"));
        assert_eq!(json["approval"]["state"], json!("valid"));
        assert_eq!(json["history_deleted"], json!(true));
    }

    #[test]
    fn session_info_reads_a_missing_thinking_level_as_unset() {
        let info: SessionInfo = serde_json::from_value(json!({
            "session_id": id(3).as_str(),
            "association_revision": "1",
            "execution_generation": "2",
            "model": {"provider": "p", "id": "m"}
        }))
        .expect("decode");
        assert_eq!(info.thinking, None);
    }

    #[test]
    fn candidates_checks_and_activity_take_the_contract_shapes() {
        let candidate = CandidateSummary {
            candidate_id: id(5),
            ordinal: counter(1),
            commit: git(2),
            base_target_commit: git(1),
            status: CandidateStatus::Current,
            files: counter(1),
            added: counter(2),
            removed: counter(3),
            created_at: ts(5),
        };
        round_trip(&candidate, "CandidateSummary");
        let check = VerificationSummary {
            verification_id: id(6),
            objective_id: id(1),
            candidate_id: id(5),
            command: vec!["cargo".into(), "test".into()],
            state: VerificationState::Failed,
            started_at: ts(6),
            output_tail: "1 failed".into(),
            truncated: false,
            provenance: Provenance::ServiceObserved,
            exit_code: Some(counter(101)),
            ended_at: Some(ts(7)),
        };
        let json = round_trip(&check, "VerificationSummary");
        assert_eq!(json["provenance"], json!("service_observed"));
        let item = ActivityItem {
            item_id: id(7),
            seq: counter(1),
            at: ts(8),
            role: ActivityRole::Tool,
            title: "bash".into(),
            text: "ls".into(),
            is_error: false,
            truncated: false,
            tool_name: Some("bash".into()),
        };
        round_trip(&item, "ActivityItem");
        round_trip(
            &Settings {
                verification_commands: vec![vec!["make".into()]],
                default_model: None,
            },
            "Settings",
        );
        round_trip(
            &Workspace {
                workspace_id: id(2),
                path: path("/src/app"),
                name: "app".into(),
                branches: vec!["main".into()],
                default_branch: Some("main".into()),
            },
            "Workspace",
        );
    }

    #[test]
    fn unknown_fields_are_refused() {
        let mut json = serde_json::to_value(objective_detail()).expect("serialize");
        json["extra"] = json!(1);
        assert!(serde_json::from_value::<ObjectiveDetail>(json).is_err());
        assert!(serde_json::from_value::<Empty>(json!({"x": 1})).is_err());
        assert_eq!(
            serde_json::from_value::<Empty>(json!({})).ok(),
            Some(Empty {})
        );
    }
}
