//! Review content: activity, Change Sets, diffs, file pages, references, drafts, inputs,
//! search results and terminal ends.

use lade_protocol::client as wire;
use lade_protocol::types::{self, Counter};

use super::objective::target;
use super::{Invalid, delivery_mode, id, path, side, timestamp, to_path, to_side};
use crate::presentation::state::model as view;
use crate::presentation::terminal::TerminalEnd;

pub(super) fn activity_role(r: types::ActivityRole) -> view::ActivityRole {
    match r {
        types::ActivityRole::User => view::ActivityRole::User,
        types::ActivityRole::Assistant => view::ActivityRole::Assistant,
        types::ActivityRole::Thinking => view::ActivityRole::Thinking,
        types::ActivityRole::Tool => view::ActivityRole::Tool,
        types::ActivityRole::System => view::ActivityRole::System,
    }
}

pub fn activity_item(i: &wire::ActivityItem) -> view::ActivityItem {
    view::ActivityItem {
        item_id: view::ActivityItemId::new(i.item_id.as_str()),
        seq: i.seq.value(),
        at: timestamp(&i.at),
        role: activity_role(i.role),
        title: i.title.clone(),
        text: i.text.clone(),
        tool_name: i.tool_name.clone(),
        is_error: i.is_error,
        truncated: i.truncated,
    }
}

pub(super) fn changed_file(f: &wire::ChangedFile) -> view::ChangedFile {
    view::ChangedFile {
        path: path(&f.path),
        old_path: f.old_path.as_ref().map(path),
        status: match f.status {
            wire::ChangeStatus::Added => view::ChangeStatus::Added,
            wire::ChangeStatus::Modified => view::ChangeStatus::Modified,
            wire::ChangeStatus::Deleted => view::ChangeStatus::Deleted,
            wire::ChangeStatus::Renamed => view::ChangeStatus::Renamed,
            wire::ChangeStatus::TypeChanged => view::ChangeStatus::TypeChanged,
        },
        added: f.added.value(),
        removed: f.removed.value(),
        binary: f.binary,
        filtered: f.filtered == Some(true),
    }
}

pub fn change_list(
    objective_id: &view::ObjectiveId,
    requested: &view::Basis,
    l: &wire::ChangeList,
) -> view::ChangeList {
    view::ChangeList {
        objective_id: objective_id.clone(),
        basis: requested.clone(),
        basis_label: l.basis_label.clone(),
        target: target(&l.target),
        capture_id: l
            .capture_id
            .as_ref()
            .map(|c| view::CaptureId::new(c.as_str())),
        candidate_id: l
            .candidate_id
            .as_ref()
            .map(|c| view::CandidateId::new(c.as_str())),
        files: l.files.iter().map(changed_file).collect(),
        truncated: l.truncated,
        total_added: l.total_added.value(),
        total_removed: l.total_removed.value(),
        notices: l
            .notices
            .iter()
            .flatten()
            .map(|n| view::ContentNotice {
                kind: match n.kind {
                    wire::ContentNoticeKind::NestedRepository => {
                        view::ContentNoticeKind::NestedRepository
                    }
                    wire::ContentNoticeKind::FilteredContent => {
                        view::ContentNoticeKind::FilteredContent
                    }
                },
                message: n.message.clone(),
                path: n.path.as_ref().map(path),
            })
            .collect(),
    }
}

pub(super) fn digest(d: &types::Digest) -> view::ArtifactDigest {
    view::ArtifactDigest::new(d.as_str())
}

pub fn file_diff(
    objective_id: &view::ObjectiveId,
    requested: &view::Basis,
    file: &view::RepoPath,
    d: &wire::FileDiff,
) -> view::FileDiff {
    view::FileDiff {
        objective_id: objective_id.clone(),
        basis: requested.clone(),
        path: file.clone(),
        old_digest: d.old_digest.as_ref().map(digest),
        new_digest: d.new_digest.as_ref().map(digest),
        binary: d.binary,
        hunks: d
            .hunks
            .iter()
            .map(|h| view::DiffHunk {
                old_start: h.old_start.value(),
                old_lines: h.old_lines.value(),
                new_start: h.new_start.value(),
                new_lines: h.new_lines.value(),
                lines: h
                    .lines
                    .iter()
                    .map(|l| view::DiffLine {
                        no_newline: l.no_newline,
                        kind: match l.kind {
                            wire::DiffLineKind::Context => view::DiffLineKind::Context,
                            wire::DiffLineKind::Add => view::DiffLineKind::Add,
                            wire::DiffLineKind::Del => view::DiffLineKind::Del,
                        },
                        old_no: l.old_no.as_ref().map(Counter::value),
                        new_no: l.new_no.as_ref().map(Counter::value),
                        text: l.text.clone(),
                        new_byte_start: l.new_byte_start.as_ref().map(Counter::value),
                        old_byte_start: l.old_byte_start.as_ref().map(Counter::value),
                    })
                    .collect(),
            })
            .collect(),
        truncated: d.truncated,
    }
}

pub fn file_page(
    objective_id: &view::ObjectiveId,
    requested: &view::Basis,
    file: &view::RepoPath,
    requested_side: view::Side,
    c: &wire::FileContent,
) -> view::FilePage {
    view::FilePage {
        objective_id: objective_id.clone(),
        basis: requested.clone(),
        path: file.clone(),
        side: requested_side,
        artifact_digest: digest(&c.artifact_digest),
        total_bytes: c.total_bytes.value(),
        offset: c.offset.value(),
        length: c.length.value(),
        encoding: match c.encoding {
            wire::FileEncoding::Utf8 => view::FileEncoding::Utf8,
            wire::FileEncoding::Binary => view::FileEncoding::Binary,
        },
        text: c.text.clone(),
        eof: c.eof,
        version_label: c.version_label.clone(),
        chunked: c.chunked == Some(true),
    }
}

pub(super) fn reference_version(v: &wire::ReferenceVersion) -> view::ReferenceVersion {
    match v {
        wire::ReferenceVersion::Working { capture_id } => view::ReferenceVersion::Working {
            capture_id: view::CaptureId::new(capture_id.as_str()),
        },
        wire::ReferenceVersion::Candidate { candidate_id } => view::ReferenceVersion::Candidate {
            candidate_id: view::CandidateId::new(candidate_id.as_str()),
        },
    }
}

pub fn reference(r: &wire::Reference) -> view::Reference {
    view::Reference {
        artifact_digest: digest(&r.artifact_digest),
        path: path(&r.path),
        version: reference_version(&r.version),
        side: side(r.side),
        byte_start: r.byte_start.value(),
        byte_end: r.byte_end.value(),
        selected_text: r.selected_text.clone(),
        line_hint: r.line_hint.as_ref().map(Counter::value),
    }
}

pub fn to_reference(r: &view::Reference) -> Result<wire::Reference, Invalid> {
    Ok(wire::Reference {
        artifact_digest: types::Digest::parse(r.artifact_digest.as_str())
            .map_err(|_| Invalid::new("artifact digest"))?,
        path: to_path(&r.path)?,
        version: match &r.version {
            view::ReferenceVersion::Working { capture_id } => wire::ReferenceVersion::Working {
                capture_id: id(capture_id.as_str(), "capture id")?,
            },
            view::ReferenceVersion::Candidate { candidate_id } => {
                wire::ReferenceVersion::Candidate {
                    candidate_id: id(candidate_id.as_str(), "candidate id")?,
                }
            }
        },
        side: to_side(r.side),
        byte_start: Counter::new(r.byte_start),
        byte_end: Counter::new(r.byte_end),
        selected_text: r.selected_text.clone(),
        line_hint: r.line_hint.map(Counter::new),
    })
}

pub fn draft(d: &wire::Draft) -> view::Draft {
    view::Draft {
        id: view::DraftId::new(d.draft_id.as_str()),
        objective_id: view::ObjectiveId::new(d.objective_id.as_str()),
        destination_session_id: view::SessionId::new(d.destination_session_id.as_str()),
        reference: reference(&d.reference),
        body: d.body.clone(),
        revision: d.revision.value(),
        state: match d.state {
            wire::DraftState::Unsent => view::DraftState::Unsent,
            wire::DraftState::Submitted => view::DraftState::Submitted,
        },
        stale: d.stale,
        input_id: d.input_id.as_ref().map(|i| view::InputId::new(i.as_str())),
    }
}

pub fn input(i: &wire::InputSummary) -> view::InputSummary {
    view::InputSummary {
        input_id: view::InputId::new(i.input_id.as_str()),
        objective_id: view::ObjectiveId::new(i.objective_id.as_str()),
        operation_id: view::OperationId::new(i.operation_id.as_str()),
        mode: delivery_mode(i.mode),
        text_preview: i.text_preview.clone(),
        accepted_at: timestamp(&i.accepted_at),
        delivery: match i.delivery {
            wire::DeliveryState::Queued => view::DeliveryState::Queued,
            wire::DeliveryState::Dispatched => view::DeliveryState::Dispatched,
            wire::DeliveryState::Received => view::DeliveryState::Received,
            wire::DeliveryState::Uncertain => view::DeliveryState::Uncertain,
            wire::DeliveryState::Blocked => view::DeliveryState::Blocked,
        },
        blocked_reason: i.blocked_reason.clone(),
    }
}

pub fn search_result(r: &wire::SearchResult) -> view::SearchResult {
    view::SearchResult {
        kind: match r.kind {
            wire::SearchResultKind::Objective => view::SearchResultKind::Objective,
            wire::SearchResultKind::File => view::SearchResultKind::File,
            wire::SearchResultKind::Content => view::SearchResultKind::Content,
        },
        objective_id: r
            .objective_id
            .as_ref()
            .map(|o| view::ObjectiveId::new(o.as_str())),
        path: r.path.as_ref().map(path),
        line: r.line.as_ref().map(Counter::value),
        preview: r.preview.clone(),
        byte_start: r.byte_start.as_ref().map(Counter::value),
        byte_end: r.byte_end.as_ref().map(Counter::value),
    }
}

pub fn terminal_end(c: &wire::TerminalClosed) -> TerminalEnd {
    use TerminalEnd as E;
    match c.reason {
        wire::TerminalCloseReason::Exited => E::Exited {
            code: c.exit_code.as_ref().map(Counter::value),
            signal: c.signal.as_ref().map(Counter::value),
        },
        wire::TerminalCloseReason::Closed => E::Closed,
        wire::TerminalCloseReason::ReviewFreeze => E::ReviewFreeze,
        wire::TerminalCloseReason::Lifecycle => E::Lifecycle,
        wire::TerminalCloseReason::Failed => E::Failed,
    }
}
