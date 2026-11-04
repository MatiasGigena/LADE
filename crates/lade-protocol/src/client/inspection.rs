//! Changed files, diffs, search results and the byte-exact references drafts anchor to.

use super::{ChangeStatus, DiffLineKind, SearchResultKind, Side};
use crate::types::{Counter, Digest, GitId, Id, WirePath};
use serde::{Deserialize, Serialize};

wire_struct!(ChangedFile {
    path: WirePath,
    status: ChangeStatus,
    added: Counter,
    removed: Counter,
    binary: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    old_path: Option<WirePath>,
    #[serde(skip_serializing_if = "Option::is_none")]
    filtered: Option<bool>,
});
wire_enum!(ContentNoticeKind {
    NestedRepository,
    FilteredContent,
});
wire_struct!(
    ContentNotice {
        kind: ContentNoticeKind,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<WirePath>,
    }
);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Basis {
    Working,
    Candidate {
        candidate_id: Id,
    },
    Since {
        from_candidate_id: Id,
        to_candidate_id: Id,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Start {
    Target,
    Commit { commit: GitId },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReferenceVersion {
    Working { capture_id: Id },
    Candidate { candidate_id: Id },
}

wire_struct!(
    Reference {
        artifact_digest: Digest,
        path: WirePath,
        version: ReferenceVersion,
        side: Side,
        byte_start: Counter,
        byte_end: Counter,
        selected_text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        line_hint: Option<Counter>,
    }
);

wire_struct!(DiffLine {
    kind: DiffLineKind,
    text: String,
    no_newline: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    old_no: Option<Counter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_no: Option<Counter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    old_byte_start: Option<Counter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_byte_start: Option<Counter>,
});
wire_struct!(DiffHunk {
    old_start: Counter,
    old_lines: Counter,
    new_start: Counter,
    new_lines: Counter,
    lines: Vec<DiffLine>,
});
wire_struct!(SearchResult {
    kind: SearchResultKind,
    preview: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    objective_id: Option<Id>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<WirePath>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<Counter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    byte_start: Option<Counter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    column: Option<Counter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    byte_end: Option<Counter>,
});
