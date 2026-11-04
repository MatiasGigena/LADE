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
