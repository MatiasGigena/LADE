//! The Change Set under review: bases, changed files, diffs, file pages and references into
//! them.

use super::ids::{ArtifactDigest, CandidateId, CaptureId, ObjectiveId, RepoPath};
use super::objective::TargetRef;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Basis {
    Working,
    Candidate {
        candidate_id: CandidateId,
    },
    Since {
        from_candidate_id: CandidateId,
        to_candidate_id: CandidateId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    TypeChanged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: RepoPath,
    pub old_path: Option<RepoPath>,
    pub status: ChangeStatus,
    pub added: u64,
    pub removed: u64,
    pub binary: bool,
    pub filtered: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentNoticeKind {
    NestedRepository,
    FilteredContent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentNotice {
    pub kind: ContentNoticeKind,
    pub message: String,
    pub path: Option<RepoPath>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeList {
    pub objective_id: ObjectiveId,
    pub basis: Basis,
    pub basis_label: String,
    pub target: TargetRef,
    pub capture_id: Option<CaptureId>,
    pub candidate_id: Option<CandidateId>,
    pub files: Vec<ChangedFile>,
    pub truncated: bool,
    pub total_added: u64,
    pub total_removed: u64,
    pub notices: Vec<ContentNotice>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiffLineKind {
    Context,
    Add,
    Del,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_no: Option<u64>,
    pub new_no: Option<u64>,
    pub text: String,
    pub new_byte_start: Option<u64>,
    pub old_byte_start: Option<u64>,
    pub no_newline: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffHunk {
    pub old_start: u64,
    pub old_lines: u64,
    pub new_start: u64,
    pub new_lines: u64,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    pub objective_id: ObjectiveId,
    pub basis: Basis,
    pub path: RepoPath,
    pub old_digest: Option<ArtifactDigest>,
    pub new_digest: Option<ArtifactDigest>,
    pub binary: bool,
    pub hunks: Vec<DiffHunk>,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Old,
    New,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileEncoding {
    Utf8,
    Binary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePage {
    pub objective_id: ObjectiveId,
    pub basis: Basis,
    pub path: RepoPath,
    pub side: Side,
    pub artifact_digest: ArtifactDigest,
    pub total_bytes: u64,
    pub offset: u64,
    pub length: u64,
    pub encoding: FileEncoding,
    pub text: Option<String>,
    pub eof: bool,
    pub version_label: String,
    pub chunked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileContent {
    pub objective_id: ObjectiveId,
    pub basis: Basis,
    pub path: RepoPath,
    pub side: Side,
    pub artifact_digest: ArtifactDigest,
    pub total_bytes: u64,
    pub encoding: FileEncoding,
    pub text: String,
    pub loaded_bytes: u64,
    pub eof: bool,
    pub version_label: String,
    pub chunked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ReferenceVersion {
    Working { capture_id: CaptureId },
    Candidate { candidate_id: CandidateId },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    pub artifact_digest: ArtifactDigest,
    pub path: RepoPath,
    pub version: ReferenceVersion,
    pub side: Side,
    pub byte_start: u64,
    pub byte_end: u64,
    pub selected_text: String,
    pub line_hint: Option<u64>,
}
