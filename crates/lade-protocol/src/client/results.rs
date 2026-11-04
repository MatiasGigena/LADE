//! Reply data for the read requests.

use super::{
    ActivityItem, ChangedFile, ContentNotice, DiffHunk, Draft, DraftCursor, FileEncoding,
    InputSummary, ObjectiveSummary, OperationState, Outcome, SearchResult, Settings, TargetRef,
    Workspace,
};
use crate::types::{Counter, Digest, Id};
use serde::{Deserialize, Serialize};

wire_struct!(SnapshotData {
    service_epoch: Id,
    sequence: Counter,
    workspaces: Vec<Workspace>,
    objectives: Vec<ObjectiveSummary>,
    settings: Settings,
});
wire_struct!(ActivityPage {
    items: Vec<ActivityItem>,
    has_more: bool,
});
wire_struct!(ChangeList {
    basis_label: String,
    target: TargetRef,
    files: Vec<ChangedFile>,
    truncated: bool,
    total_added: Counter,
    total_removed: Counter,
    #[serde(skip_serializing_if = "Option::is_none")]
    capture_id: Option<Id>,
    #[serde(skip_serializing_if = "Option::is_none")]
    candidate_id: Option<Id>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notices: Option<Vec<ContentNotice>>,
});
wire_struct!(FileContent {
    artifact_digest: Digest,
    total_bytes: Counter,
    offset: Counter,
    length: Counter,
    encoding: FileEncoding,
    eof: bool,
    version_label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chunked: Option<bool>,
});
wire_struct!(FileDiff {
    binary: bool,
    hunks: Vec<DiffHunk>,
    truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    old_digest: Option<Digest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_digest: Option<Digest>,
});
wire_struct!(SearchResults {
    scope_label: String,
    results: Vec<SearchResult>,
    truncated: bool,
});
wire_struct!(DraftList {
    drafts: Vec<Draft>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_after: Option<DraftCursor>,
});
wire_struct!(InputList {
    inputs: Vec<InputSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_after: Option<Counter>,
});
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationStatus {
    pub state: OperationState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::fixtures::{counter, digest, git, id, objective_detail, round_trip, ts};
    use crate::client::{Empty, ReplyData};
    use serde_json::json;

    #[test]
    fn file_content_carries_text_or_a_chunked_transfer() {
        let text = FileContent {
            artifact_digest: digest(1),
            total_bytes: counter(5),
            offset: counter(0),
            length: counter(5),
            encoding: FileEncoding::Utf8,
            eof: true,
            version_label: "working copy".into(),
            text: Some("hello".into()),
            chunked: None,
        };
        let json = round_trip(&text, "FileContent");
        assert!(json.get("chunked").is_none());
        let binary = FileContent {
            encoding: FileEncoding::Binary,
            text: None,
            chunked: Some(true),
            ..text
        };
        let json = round_trip(&binary, "FileContent");
        assert_eq!((json.get("text"), &json["chunked"]), (None, &json!(true)));
    }

    #[test]
    fn pages_name_their_next_cursor_only_when_there_is_more() {
        let last = DraftList {
            drafts: Vec::new(),
            next_after: None,
        };
        assert_eq!(round_trip(&last, "DraftList"), json!({"drafts": []}));
        let more = DraftList {
            drafts: Vec::new(),
            next_after: Some(DraftCursor {
                created_at: ts(1),
                draft_id: id(2),
            }),
        };
        assert_eq!(
            round_trip(&more, "DraftList")["next_after"]["draft_id"],
            json!(id(2).as_str())
        );
        let inputs = InputList {
            inputs: Vec::new(),
            next_after: Some(counter(200)),
        };
        assert_eq!(round_trip(&inputs, "InputList")["next_after"], json!("200"));
        round_trip(
            &ActivityPage {
                items: Vec::new(),
                has_more: false,
            },
            "ActivityPage",
        );
    }

    #[test]
    fn snapshots_changes_and_search_results_match_the_contract() {
        let snapshot = SnapshotData {
            service_epoch: id(9),
            sequence: counter(42),
            workspaces: Vec::new(),
            objectives: vec![objective_detail().summary()],
            settings: Settings {
                verification_commands: Vec::new(),
                default_model: None,
            },
        };
        round_trip(&snapshot, "SnapshotData");
        let changes = ChangeList {
            basis_label: "working copy".into(),
            target: TargetRef {
                branch: "main".into(),
                commit: git(1),
            },
            files: Vec::new(),
            truncated: false,
            total_added: counter(0),
            total_removed: counter(0),
            capture_id: Some(id(3)),
            candidate_id: None,
            notices: None,
        };
        let json = round_trip(&changes, "ChangeList");
        assert!(json.get("candidate_id").is_none() && json.get("notices").is_none());
        round_trip(
            &FileDiff {
                binary: true,
                hunks: Vec::new(),
                truncated: false,
                old_digest: Some(digest(1)),
                new_digest: None,
            },
            "FileDiff",
        );
        round_trip(
            &SearchResults {
                scope_label: "this Objective".into(),
                results: Vec::new(),
                truncated: true,
            },
            "SearchResults",
        );
    }

    #[test]
    fn operation_status_embeds_the_recorded_outcome() {
        let pending = OperationStatus {
            state: OperationState::Pending,
            outcome: None,
        };
        assert_eq!(
            round_trip(&pending, "OperationStatus"),
            json!({"state": "pending"})
        );
        let completed = OperationStatus {
            state: OperationState::Completed,
            outcome: Some(Outcome::Ok(Box::new(ReplyData::DraftDelete(Empty {})))),
        };
        let json = round_trip(&completed, "OperationStatus");
        assert_eq!(json["outcome"]["name"], json!("draft.delete"));
    }
}
