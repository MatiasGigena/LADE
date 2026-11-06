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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::fixtures::{counter, digest, git, id, path, round_trip};
    use crate::schema::{self, Contract};
    use serde_json::json;

    fn reference(version: ReferenceVersion) -> Reference {
        Reference {
            artifact_digest: digest(1),
            path: path("src/main.rs"),
            version,
            side: Side::New,
            byte_start: counter(3),
            byte_end: counter(5),
            selected_text: "ab".into(),
            line_hint: None,
        }
    }

    #[test]
    fn tagged_choices_name_their_kind_and_carry_only_their_fields() {
        let bases = [
            (Basis::Working, json!({"kind": "working"})),
            (
                Basis::Candidate {
                    candidate_id: id(1),
                },
                json!({"kind": "candidate", "candidate_id": id(1).as_str()}),
            ),
            (
                Basis::Since {
                    from_candidate_id: id(1),
                    to_candidate_id: id(2),
                },
                json!({"kind": "since", "from_candidate_id": id(1).as_str(), "to_candidate_id": id(2).as_str()}),
            ),
        ];
        for (basis, expected) in bases {
            assert_eq!(round_trip(&basis, "Basis"), expected);
        }
        let commit = Start::Commit { commit: git(1) };
        assert_eq!(round_trip(&commit, "Start")["kind"], json!("commit"));
        assert_eq!(
            round_trip(&Start::Target, "Start"),
            json!({"kind": "target"})
        );
        // serde ignores extra fields on a tagged unit variant; the contract refuses them first.
        let extra = json!({"kind": "working", "candidate_id": id(1).as_str()});
        assert!(!schema::conforms(&extra, Contract::Client, "/$defs/Basis"));
        assert!(serde_json::from_value::<Basis>(json!({"kind": "head"})).is_err());
    }

    #[test]
    fn references_anchor_either_a_capture_or_a_candidate() {
        let working = reference(ReferenceVersion::Working { capture_id: id(4) });
        let json = round_trip(&working, "Reference");
        assert_eq!(
            json["version"],
            json!({"kind": "working", "capture_id": id(4).as_str()})
        );
        assert!(json.get("line_hint").is_none());
        let mut candidate = reference(ReferenceVersion::Candidate {
            candidate_id: id(5),
        });
        candidate.line_hint = Some(counter(2));
        assert_eq!(round_trip(&candidate, "Reference")["line_hint"], json!("2"));
    }

    #[test]
    fn diffs_changes_and_search_results_omit_unset_positions() {
        let line = DiffLine {
            kind: DiffLineKind::Add,
            text: "let x = 1;".into(),
            no_newline: true,
            old_no: None,
            new_no: Some(counter(4)),
            old_byte_start: None,
            new_byte_start: Some(counter(40)),
        };
        let json = round_trip(&line, "DiffLine");
        assert!(json.get("old_no").is_none() && json.get("old_byte_start").is_none());
        round_trip(
            &DiffHunk {
                old_start: counter(0),
                old_lines: counter(0),
                new_start: counter(1),
                new_lines: counter(1),
                lines: vec![line],
            },
            "DiffHunk",
        );
        let renamed = ChangedFile {
            path: path("b.rs"),
            status: ChangeStatus::Renamed,
            added: counter(0),
            removed: counter(0),
            binary: false,
            old_path: Some(path("a.rs")),
            filtered: None,
        };
        assert_eq!(
            round_trip(&renamed, "ChangedFile")["status"],
            json!("renamed")
        );
        let notice = ContentNotice {
            kind: ContentNoticeKind::NestedRepository,
            message: "vendor/lib is a repository.".into(),
            path: Some(path("vendor/lib")),
        };
        round_trip(&notice, "ContentNotice");
        let hit = SearchResult {
            kind: SearchResultKind::Objective,
            preview: "Fix login".into(),
            objective_id: Some(id(1)),
            path: None,
            line: None,
            byte_start: None,
            column: None,
            byte_end: None,
        };
        assert_eq!(
            round_trip(&hit, "SearchResult"),
            json!({"kind": "objective", "preview": "Fix login", "objective_id": id(1).as_str()})
        );
    }
}
