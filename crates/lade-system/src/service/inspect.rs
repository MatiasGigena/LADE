use std::sync::Arc;

use lade_core::domain::ids::{CandidateId, GitId, ObjectiveId};
use lade_protocol::client::{
    ActivityPage, ActivityPageQuery, Basis, ChangeList, ChangeStatus, ChangedFile,
    ChangesListQuery, ClientBody, ClientEnvelope, ContentNotice, ContentNoticeKind, DiffGetQuery,
    DiffHunk, DiffLine, DiffLineKind, ErrorCode, FileContent, FileDiff, FileEncoding,
    FileReadQuery, Rejection, Reply, ReplyData, Request, SafeAction, SearchQuery, SearchResult,
    SearchResultKind, SearchResults, SearchScope, Side, TargetRef,
};
use lade_protocol::types::{Counter, Digest};
use lade_protocol::{Framing, encode, json_len};

use crate::adapters::artifacts::{ArtifactDigest, ArtifactStore, MAX_ARTIFACT_PAGE};
use crate::adapters::git::{self, GrepQuery, LineKind, ObjectKind, Repository};
use crate::storage::{self, CaptureRow};

use super::capture::CapturePurpose;
use super::convert::{
    adapter_git, git_rejection, invalid, not_found, parse_uuid, preview, repo_path, wire_git,
    wire_id, wire_path,
};
use super::execution::wire_activity;
use super::handlers::sql;
use super::jobs::{self, CaptureOut, FileBytes, LargeBlobs};
use super::{Completed, Flow, ReqCtx, Service};

const MAX_FILES: usize = 1024;
const MAX_DIFF_LINES: usize = 1800;
const MAX_DIFF_TEXT: usize = 640 * 1024;
const MAX_PAGE_JSON: usize = 900 * 1024;
const MAX_ACTIVITY_BYTES: usize = 768 * 1024;

#[derive(Clone)]
struct Resolved {
    old: GitId,
    new: GitId,
    label: String,
    target: TargetRef,
    capture_id: Option<lade_core::domain::ids::CaptureId>,
    candidate_id: Option<CandidateId>,
}

enum Basis2 {
    Ready(Box<Resolved>),
    NeedsCapture,
}

fn wire_status(status: git::ChangeStatus) -> ChangeStatus {
    match status {
        git::ChangeStatus::Added | git::ChangeStatus::Copied { .. } => ChangeStatus::Added,
        git::ChangeStatus::Modified => ChangeStatus::Modified,
        git::ChangeStatus::Deleted => ChangeStatus::Deleted,
        git::ChangeStatus::Renamed { .. } => ChangeStatus::Renamed,
        git::ChangeStatus::TypeChanged => ChangeStatus::TypeChanged,
    }
}

fn content_notices(skipped: &[git::RepoPath], filtered: &[git::RepoPath]) -> Vec<ContentNotice> {
    let mut notices: Vec<ContentNotice> = skipped
        .iter()
        .take(63)
        .map(|path| ContentNotice {
            kind: ContentNoticeKind::NestedRepository,
            message: "Nested repository without a commit: its files are not part of this capture. \
                      Commit inside it (it is then recorded as a submodule entry) or move it out."
                .into(),
            path: Some(wire_path(path.as_bytes())),
        })
        .collect();
    if !filtered.is_empty() {
        notices.push(ContentNotice {
            kind: ContentNoticeKind::FilteredContent,
            message: preview(
                &format!(
                    "Filtered repository content (Git LFS or clean/smudge filters): {} changed \
                     file(s) are compared and stored as raw working-tree bytes because LADE never \
                     runs filter drivers. Their content is unsupported: a checked-out LFS file \
                     shows as modified and would be committed unfiltered.",
                    filtered.len()
                ),
                512,
            ),
            path: None,
        });
    }
    notices
}

fn change_list(
    resolved: &Resolved,
    files: &[git::ChangedFile],
    filtered: &[git::RepoPath],
    notices: Vec<ContentNotice>,
) -> ChangeList {
    let mut list = ChangeList {
        basis_label: preview(&resolved.label, 256),
        target: resolved.target.clone(),
        files: Vec::new(),
        truncated: false,
        total_added: Counter::new(files.iter().map(|f| f.added).sum()),
        total_removed: Counter::new(files.iter().map(|f| f.removed).sum()),
        capture_id: resolved.capture_id.map(|c| wire_id(c.uuid())),
        candidate_id: resolved.candidate_id.map(|c| wire_id(c.uuid())),
        notices: None,
    };
    // Leave room for the optional notices field and its array delimiters.
    let mut remaining = MAX_PAGE_JSON.saturating_sub(json_len(&list).saturating_add(16));
    let mut kept_notices = Vec::new();
    for notice in notices {
        let cost = json_len(&notice).saturating_add(1);
        if cost > remaining {
            list.truncated = true;
            break;
        }
        remaining -= cost;
        kept_notices.push(notice);
    }
    list.notices = (!kept_notices.is_empty()).then_some(kept_notices);
    for file in files.iter().take(MAX_FILES) {
        let file = ChangedFile {
            path: wire_path(file.path.as_bytes()),
            status: wire_status(file.status),
            added: Counter::new(file.added),
            removed: Counter::new(file.removed),
            binary: file.binary,
            old_path: file
                .old_path
                .as_ref()
                .map(|path| wire_path(path.as_bytes())),
            filtered: filtered.contains(&file.path).then_some(true),
        };
        let cost = json_len(&file).saturating_add(1);
        if cost > remaining {
            break;
        }
        remaining -= cost;
        list.files.push(file);
    }
    list.truncated |= list.files.len() < files.len();
    list
}

fn capture_label(capture: &CaptureRow) -> String {
    format!("working capture {}", capture.id.uuid().short())
}

fn json_cost(text: &str) -> usize {
    text.chars()
        .map(|c| match c {
            '"' | '\\' => 2,
            c if (c as u32) < 0x20 => 6,
            c => c.len_utf8(),
        })
        .sum()
}

fn file_page(
    bytes: &[u8],
    digest: Digest,
    query: &FileReadQuery,
    label: &str,
) -> Result<FileContent, Rejection> {
    let total = bytes.len() as u64;
    let offset = query.offset.value().min(total);
    let start = usize::try_from(offset).map_err(|_| invalid("offset out of range"))?;
    let limit = query.limit.value() as usize;
    let binary = bytes.iter().take(8000).any(|b| *b == 0) || std::str::from_utf8(bytes).is_err();
    let mut end = start.saturating_add(limit).min(bytes.len());
    let text = if binary {
        None
    } else {
        let whole = std::str::from_utf8(bytes).map_err(|_| invalid("not utf-8"))?;
        if !whole.is_char_boundary(start) {
            return Err(invalid("offset is not at a character boundary"));
        }
        while !whole.is_char_boundary(end) {
            end -= 1;
        }
        while end > start && json_cost(&whole[start..end]) > MAX_PAGE_JSON {
            end = start + (end - start) / 2;
            while !whole.is_char_boundary(end) {
                end -= 1;
            }
        }
        Some(whole[start..end].to_string())
    };
    Ok(FileContent {
        artifact_digest: digest,
        total_bytes: Counter::new(total),
        offset: Counter::new(offset),
        length: Counter::new((end - start) as u64),
        encoding: if binary {
            FileEncoding::Binary
        } else {
            FileEncoding::Utf8
        },
        eof: end as u64 >= total,
        version_label: preview(label, 256),
        text,
        chunked: None,
    })
}

fn chunk_page(
    artifacts: &ArtifactStore,
    digest: &ArtifactDigest,
    query: &FileReadQuery,
    label: &str,
) -> Result<FileContent, Rejection> {
    let unreadable = |_| super::convert::durability("retained content unreadable");
    let probe = artifacts.read_page(digest, 0, 8000).map_err(unreadable)?;
    let binary = probe.data.contains(&0);
    let page = artifacts
        .read_page(
            digest,
            query.offset.value(),
            (query.limit.value() as usize).min(MAX_ARTIFACT_PAGE),
        )
        .map_err(unreadable)?;
    let mut length = page.data.len();
    let text = if binary {
        None
    } else {
        if page.data.first().is_some_and(|b| (0x80..0xc0).contains(b)) {
            return Err(invalid("offset is not at a character boundary"));
        }
        let valid = match std::str::from_utf8(&page.data) {
            Ok(text) => text,
            Err(e) if e.error_len().is_none() => std::str::from_utf8(&page.data[..e.valid_up_to()])
                .map_err(|_| invalid("the page is not valid UTF-8"))?,
            Err(e) => {
                return Err(Rejection::new(
                    lade_protocol::client::ErrorCode::UnsupportedContent,
                    lade_protocol::client::SafeAction::None,
                    &format!(
                        "not UTF-8 text at byte {}; the rest cannot be shown as text",
                        page.offset + e.valid_up_to() as u64
                    ),
                ));
            }
        };
        let mut end = valid.len();
        while end > 0 && json_cost(&valid[..end]) > MAX_PAGE_JSON {
            end /= 2;
            while !valid.is_char_boundary(end) {
                end -= 1;
            }
        }
        length = end;
        Some(valid[..end].to_string())
    };
    Ok(FileContent {
        artifact_digest: Digest::parse(digest.to_hex())
            .map_err(|_| invalid("the content digest is not canonical"))?,
        total_bytes: Counter::new(page.total_bytes),
        offset: Counter::new(page.offset),
        length: Counter::new(length as u64),
        encoding: if binary {
            FileEncoding::Binary
        } else {
            FileEncoding::Utf8
        },
        eof: page.offset + length as u64 >= page.total_bytes,
        version_label: preview(label, 256),
        text,
        chunked: Some(true),
    })
}

fn retain(artifacts: &ArtifactStore, bytes: &[u8]) -> Result<Digest, Rejection> {
    let digest = artifacts
        .put(bytes)
        .map_err(|_| super::convert::durability("content could not be retained"))?;
    Digest::parse(digest.to_hex()).map_err(|_| invalid("the content digest is not canonical"))
}

fn read_file(
    repo: &Repository,
    artifacts: &ArtifactStore,
    large: &LargeBlobs,
    resolved: &Resolved,
    query: &FileReadQuery,
) -> Result<ReplyData, Rejection> {
    let path = repo_path(&query.path)?;
    let tree = match query.side {
        Side::Old => &resolved.old,
        Side::New => &resolved.new,
    };
    let side = match query.side {
        Side::Old => "old side",
        Side::New => "new side",
    };
    let label = format!("{} ({side})", resolved.label);
    match jobs::read_file_bytes(repo, tree, &path)? {
        None => Err(not_found("path in this version")),
        Some(FileBytes::Whole(bytes)) => {
            let digest = retain(artifacts, &bytes)?;
            file_page(&bytes, digest, query, &label).map(ReplyData::FileRead)
        }
        Some(FileBytes::Large { blob }) => {
            let digest = large.retain(repo, artifacts, &blob)?;
            chunk_page(artifacts, &digest, query, &label).map(ReplyData::FileRead)
        }
    }
}

fn read_diff(
    repo: &Repository,
    artifacts: &ArtifactStore,
    resolved: &Resolved,
    query: &DiffGetQuery,
) -> Result<ReplyData, Rejection> {
    let path = repo_path(&query.path)?;
    let files = repo
        .diff_numstat(&adapter_git(&resolved.old), &adapter_git(&resolved.new))
        .map_err(git_rejection)?;
    let old_path = files
        .iter()
        .find(|f| f.path == path)
        .and_then(|f| f.old_path.clone());
    let diff = repo
        .diff_hunks(
            &adapter_git(&resolved.old),
            &adapter_git(&resolved.new),
            &path,
            old_path.as_ref(),
        )
        .map_err(git_rejection)?;
    let side_digest = |tree: &GitId, p: &git::RepoPath| -> Result<Option<Digest>, Rejection> {
        match jobs::read_whole(repo, tree, p) {
            Ok(Some(bytes)) => retain(artifacts, &bytes).map(Some),
            Ok(None) | Err(_) => Ok(None),
        }
    };
    let old_digest = side_digest(&resolved.old, old_path.as_ref().unwrap_or(&path))?;
    let new_digest = side_digest(&resolved.new, &path)?;
    let mut truncated = diff.truncated || diff.too_large;
    let mut lines_left = MAX_DIFF_LINES;
    let mut text_left = MAX_DIFF_TEXT;
    let mut hunks = Vec::new();
    'hunks: for hunk in diff.hunks.iter().take(512) {
        let mut lines = Vec::new();
        for line in hunk.lines.iter().take(1024) {
            let text = String::from_utf8_lossy(&line.text).into_owned();
            let cost = json_cost(&text) + 64;
            if lines_left == 0 || cost > text_left {
                truncated = true;
                if !lines.is_empty() {
                    hunks.push(wire_hunk(hunk, lines));
                }
                break 'hunks;
            }
            lines_left -= 1;
            text_left -= cost;
            lines.push(DiffLine {
                no_newline: line.no_newline,
                kind: match line.kind {
                    LineKind::Context => DiffLineKind::Context,
                    LineKind::Add => DiffLineKind::Add,
                    LineKind::Del => DiffLineKind::Del,
                },
                text,
                old_no: line.old_no.map(Counter::new),
                new_no: line.new_no.map(Counter::new),
                old_byte_start: line.old_byte_start.map(Counter::new),
                new_byte_start: line.new_byte_start.map(Counter::new),
            });
        }
        truncated |= hunk.lines.len() > 1024;
        hunks.push(wire_hunk(hunk, lines));
    }
    truncated |= diff.hunks.len() > 512;
    Ok(ReplyData::DiffGet(FileDiff {
        binary: diff.binary,
        hunks,
        truncated,
        old_digest,
        new_digest,
    }))
}

fn wire_hunk(hunk: &git::Hunk, lines: Vec<DiffLine>) -> DiffHunk {
    DiffHunk {
        old_start: Counter::new(hunk.old_start),
        old_lines: Counter::new(hunk.old_lines),
        new_start: Counter::new(hunk.new_start),
        new_lines: Counter::new(hunk.new_lines),
        lines,
    }
}

fn search_tree(
    repo: &Repository,
    tree: &GitId,
    query: &SearchQuery,
    label: &str,
) -> Result<ReplyData, Rejection> {
    let limit = query.limit.value() as usize;
    let (results, truncated) = match query.scope {
        SearchScope::Files => {
            let needle = query.query.as_bytes().to_ascii_lowercase();
            let entries = repo
                .ls_tree(&adapter_git(tree), None, true)
                .map_err(git_rejection)?;
            let mut matched: Vec<SearchResult> = entries
                .iter()
                .filter(|e| e.kind == ObjectKind::Blob)
                .filter(|e| {
                    e.path
                        .as_bytes()
                        .to_ascii_lowercase()
                        .windows(needle.len())
                        .any(|w| w == needle.as_slice())
                })
                .take(limit + 1)
                .map(|e| SearchResult {
                    kind: SearchResultKind::File,
                    preview: preview(&e.path.display(), 1024),
                    objective_id: query.objective_id.clone(),
                    path: Some(wire_path(e.path.as_bytes())),
                    line: None,
                    byte_start: None,
                    column: None,
                    byte_end: None,
                })
                .collect();
            let truncated = matched.len() > limit;
            matched.truncate(limit);
            (matched, truncated)
        }
        SearchScope::Content => {
            if query.query.contains('\n') {
                return Err(invalid("content search is a single line"));
            }
            let found = repo
                .grep(
                    &adapter_git(tree),
                    &GrepQuery {
                        text: query.query.clone(),
                        ignore_case: true,
                        max_results: limit,
                    },
                )
                .map_err(git_rejection)?;
            let results = found
                .matches
                .iter()
                .take(limit)
                .map(|m| SearchResult {
                    kind: SearchResultKind::Content,
                    preview: preview(&String::from_utf8_lossy(&m.preview), 1024),
                    objective_id: query.objective_id.clone(),
                    path: Some(wire_path(m.path.as_bytes())),
                    line: Some(Counter::new(m.line)),
                    byte_start: m.byte_start.map(Counter::new),
                    column: Some(Counter::new(m.column)),
                    byte_end: m.byte_end.map(Counter::new),
                })
                .collect();
            (results, found.truncated)
        }
        SearchScope::Objectives => (Vec::new(), false),
    };
    let scope = match query.scope {
        SearchScope::Files => "file paths",
        _ => "file contents",
    };
    let mut page = SearchResults {
        scope_label: preview(&format!("{scope} in {label}"), 256),
        results: Vec::new(),
        truncated,
    };
    let mut remaining = MAX_PAGE_JSON.saturating_sub(json_len(&page));
    for result in results {
        let cost = json_len(&result).saturating_add(1);
        if cost > remaining {
            page.truncated = true;
            break;
        }
        remaining -= cost;
        page.results.push(result);
    }
    Ok(ReplyData::Search(page))
}

impl Service {
    fn resolve(&mut self, oid: ObjectiveId, basis: &Basis) -> Result<Basis2, Rejection> {
        let st = self.obj(oid)?.clone();
        let record = st.objective().record();
        let branch = record.integration_target.as_str().to_string();
        let candidate = |id: &lade_protocol::types::Id| -> Result<_, Rejection> {
            let id = CandidateId::new(parse_uuid(id)?);
            st.review
                .candidate(id)
                .cloned()
                .ok_or_else(|| not_found("candidate"))
        };
        Ok(Basis2::Ready(Box::new(match basis {
            Basis::Working => match (&st.capture, &st.row.worktree) {
                (Some(capture), _) => Resolved {
                    old: capture.target_commit.clone(),
                    new: capture.tree.clone(),
                    label: capture_label(capture),
                    target: TargetRef {
                        branch,
                        commit: wire_git(&capture.target_commit),
                    },
                    capture_id: Some(capture.id),
                    candidate_id: None,
                },
                (None, Some(_)) => return Ok(Basis2::NeedsCapture),
                (None, None) => {
                    let target = self
                        .fresh_target(&st)
                        .ok_or_else(|| not_found("integration target branch"))?;
                    Resolved {
                        old: target.clone(),
                        new: record.start_commit.clone(),
                        label: "start commit (no agent work yet)".into(),
                        target: TargetRef {
                            branch,
                            commit: wire_git(&target),
                        },
                        capture_id: None,
                        candidate_id: None,
                    }
                }
            },
            Basis::Candidate { candidate_id } => {
                let c = candidate(candidate_id)?;
                Resolved {
                    old: c.base_target_commit.clone(),
                    new: c.tree.clone(),
                    label: format!("candidate {}", c.ordinal),
                    target: TargetRef {
                        branch,
                        commit: wire_git(&c.base_target_commit),
                    },
                    capture_id: None,
                    candidate_id: Some(c.id),
                }
            }
            Basis::Since {
                from_candidate_id,
                to_candidate_id,
            } => {
                let from = candidate(from_candidate_id)?;
                let to = candidate(to_candidate_id)?;
                let caveat = if from.base_target_commit == to.base_target_commit {
                    ""
                } else {
                    " (target refreshed between them)"
                };
                Resolved {
                    old: from.tree.clone(),
                    new: to.tree.clone(),
                    label: format!("candidate {} to {}{caveat}", from.ordinal, to.ordinal),
                    target: TargetRef {
                        branch,
                        commit: wire_git(&to.base_target_commit),
                    },
                    capture_id: None,
                    candidate_id: Some(to.id),
                }
            }
        })))
    }

    fn repo_of(&self, oid: ObjectiveId) -> Result<Repository, Rejection> {
        let st = self.obj(oid)?;
        Ok(self.ws_of(st)?.repo.clone())
    }

    fn query_on_basis(
        &mut self,
        ctx: ReqCtx,
        oid: Result<ObjectiveId, Rejection>,
        basis: &Basis,
        retry: Request,
        work: impl FnOnce(Repository, Arc<ArtifactStore>, Resolved) -> Result<ReplyData, Rejection>
        + Send
        + 'static,
    ) -> Flow {
        let resolved = oid.and_then(|oid| Ok((oid, self.resolve(oid, basis)?, self.repo_of(oid)?)));
        match resolved {
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                Flow::Done
            }
            Ok((oid, Basis2::NeedsCapture, _)) => {
                let st = &self.objectives[&oid];
                let input = st
                    .row
                    .worktree
                    .clone()
                    .ok_or_else(|| not_found("worktree"))
                    .and_then(|w| self.capture_input(st, w, Self::unsent_references(st)));
                match input {
                    Ok(input) => {
                        self.spawn(move || Completed::Capture {
                            oid,
                            purpose: CapturePurpose::Retry(ctx, Box::new(retry)),
                            result: jobs::capture(&input),
                        });
                        Flow::Deferred
                    }
                    Err(rejection) => {
                        self.finish(ctx, Err(rejection));
                        Flow::Done
                    }
                }
            }
            Ok((_, Basis2::Ready(resolved), repo)) => {
                let artifacts = self.artifacts.clone();
                self.spawn(move || Completed::Query {
                    ctx,
                    result: work(repo, artifacts, *resolved),
                });
                Flow::Deferred
            }
        }
    }

    pub(crate) fn changes_list(&mut self, ctx: ReqCtx, query: ChangesListQuery) -> Flow {
        let oid = match self.objective_id(&query.objective_id) {
            Ok(oid) => oid,
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                return Flow::Done;
            }
        };
        let st = &self.objectives[&oid];
        if let (Basis::Working, Some(worktree)) = (&query.basis, st.row.worktree.clone()) {
            match self.capture_input(st, worktree, Self::unsent_references(st)) {
                Ok(input) => {
                    self.spawn(move || Completed::Capture {
                        oid,
                        purpose: CapturePurpose::Changes(ctx),
                        result: jobs::capture(&input),
                    });
                    return Flow::Deferred;
                }
                Err(rejection) => {
                    self.finish(ctx, Err(rejection));
                    return Flow::Done;
                }
            }
        }
        let basis = query.basis.clone();
        self.query_on_basis(
            ctx,
            Ok(oid),
            &basis,
            Request::ChangesList(query),
            |repo, _, resolved| {
                let new = adapter_git(&resolved.new);
                let files = repo
                    .diff_numstat(&adapter_git(&resolved.old), &new)
                    .map_err(git_rejection)?;
                let filtered = jobs::filtered_paths(&repo, &new, &files)?;
                let notices = content_notices(&[], &filtered);
                Ok(ReplyData::ChangesList(change_list(
                    &resolved, &files, &filtered, notices,
                )))
            },
        )
    }

    pub(crate) fn working_change_list(
        &mut self,
        oid: ObjectiveId,
        out: &CaptureOut,
        row: &CaptureRow,
    ) -> Result<ReplyData, Rejection> {
        let st = self.obj(oid)?;
        let resolved = Resolved {
            old: out.target.clone(),
            new: out.tree.clone(),
            label: capture_label(row),
            target: TargetRef {
                branch: st
                    .objective()
                    .record()
                    .integration_target
                    .as_str()
                    .to_string(),
                commit: wire_git(&out.target),
            },
            capture_id: Some(row.id),
            candidate_id: None,
        };
        let notices = content_notices(&out.skipped_nested, &out.filtered);
        Ok(ReplyData::ChangesList(change_list(
            &resolved,
            &out.files,
            &out.filtered,
            notices,
        )))
    }

    pub(crate) fn file_read(&mut self, ctx: ReqCtx, query: FileReadQuery) -> Flow {
        let oid = self.objective_id(&query.objective_id);
        let basis = query.basis.clone();
        let q = query.clone();
        let large = Arc::clone(&self.large_blobs);
        self.query_on_basis(
            ctx,
            oid,
            &basis,
            Request::FileRead(query),
            move |repo, artifacts, resolved| read_file(&repo, &artifacts, &large, &resolved, &q),
        )
    }

    pub(crate) fn diff_get(&mut self, ctx: ReqCtx, query: DiffGetQuery) -> Flow {
        let oid = self.objective_id(&query.objective_id);
        let basis = query.basis.clone();
        let q = query.clone();
        self.query_on_basis(
            ctx,
            oid,
            &basis,
            Request::DiffGet(query),
            move |repo, artifacts, resolved| read_diff(&repo, &artifacts, &resolved, &q),
        )
    }

    pub(crate) fn search(&mut self, ctx: ReqCtx, query: SearchQuery) -> Flow {
        let workspace = match parse_uuid(&query.workspace_id) {
            Ok(uuid) => lade_core::domain::ids::WorkspaceId::new(uuid),
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                return Flow::Done;
            }
        };
        let Some(ws) = self.workspaces.get(&workspace).cloned() else {
            self.finish(ctx, Err(not_found("workspace")));
            return Flow::Done;
        };
        if query.scope == SearchScope::Objectives {
            let needle = query.query.to_lowercase();
            let limit = query.limit.value() as usize;
            let mut results: Vec<SearchResult> = self
                .objectives
                .values()
                .filter(|st| st.objective().record().workspace_id == workspace)
                .filter(|st| {
                    let r = st.objective().record();
                    r.title.as_str().to_lowercase().contains(&needle)
                        || r.intent.outcome().to_lowercase().contains(&needle)
                })
                .take(limit + 1)
                .map(|st| SearchResult {
                    kind: SearchResultKind::Objective,
                    preview: preview(st.objective().record().title.as_str(), 1024),
                    objective_id: Some(wire_id(st.objective().id().uuid())),
                    path: None,
                    line: None,
                    byte_start: None,
                    column: None,
                    byte_end: None,
                })
                .collect();
            let truncated = results.len() > limit;
            results.truncate(limit);
            let data = ReplyData::Search(SearchResults {
                scope_label: preview(&format!("objectives in {}", ws.name), 256),
                results,
                truncated,
            });
            self.finish(ctx, Ok(data));
            return Flow::Done;
        }
        if let (Some(objective_id), basis) = (&query.objective_id, &query.basis) {
            let oid = self.objective_id(objective_id);
            let basis = basis.clone().unwrap_or(Basis::Working);
            let q = query.clone();
            self.query_on_basis(
                ctx,
                oid,
                &basis,
                Request::Search(query),
                move |repo, _, resolved| search_tree(&repo, &resolved.new, &q, &resolved.label),
            )
        } else {
            let branch = ws.default_branch.clone().unwrap_or_else(|| "main".into());
            let Some(tree) = self.resolve_target(workspace, &branch) else {
                self.finish(ctx, Err(not_found("default branch")));
                return Flow::Done;
            };
            let label = format!("branch {branch} at {}", &tree.hex()[..12]);
            let repo = ws.repo.clone();
            self.spawn(move || Completed::Query {
                ctx,
                result: search_tree(&repo, &tree, &query, &label),
            });
            Flow::Deferred
        }
    }

    pub(crate) fn activity_page(&self, query: &ActivityPageQuery) -> Result<ReplyData, Rejection> {
        let oid = self.objective_id(&query.objective_id)?;
        let (rows, mut has_more) = sql(storage::activity_page(
            self.store.conn(),
            oid,
            query.before.as_ref().map(Counter::value),
            query.limit.value(),
        ))?;
        let mut budget = MAX_ACTIVITY_BYTES;
        let mut items = Vec::new();
        let mut rows = rows.into_iter().rev().peekable();
        while let Some(mut row) = rows.next() {
            if let Some((text, truncated)) = self.live_text(&row.item_id) {
                row.text = text.to_string();
                row.truncated = truncated;
            }
            let Some(item) = wire_activity(&row) else {
                continue;
            };
            let cost = json_len(&item).saturating_add(1);
            if cost > budget {
                if items.is_empty() {
                    let page = ActivityPage {
                        items: vec![item],
                        has_more: has_more || rows.peek().is_some(),
                    };
                    encode(
                        &ClientEnvelope::new(ClientBody::Reply(Reply::ok(
                            query.objective_id.clone(),
                            ReplyData::ActivityPage(page.clone()),
                        ))),
                        Framing::Client,
                    )
                    .map_err(|_| {
                        Rejection::new(
                            ErrorCode::LimitReached,
                            SafeAction::None,
                            "This activity item exceeds the reply limit and cannot be shown.",
                        )
                    })?;
                    return Ok(ReplyData::ActivityPage(page));
                }
                has_more = true;
                break;
            }
            budget -= cost;
            items.push(item);
        }
        items.reverse();
        Ok(ReplyData::ActivityPage(ActivityPage { items, has_more }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::adapters_support;
    use lade_core::domain::ids::GitAlgorithm;
    use lade_protocol::client::{ClientBody, ClientEnvelope, Reply};
    use lade_protocol::types::{Id, PageLimit};

    #[test]
    fn change_lists_with_long_renamed_paths_fit_the_wire_frame() {
        let target = GitId::new(GitAlgorithm::Sha1, &"a".repeat(40)).unwrap();
        let resolved = Resolved {
            old: target.clone(),
            new: target.clone(),
            label: "candidate 1".into(),
            target: TargetRef {
                branch: "main".into(),
                commit: wire_git(&target),
            },
            capture_id: None,
            candidate_id: None,
        };
        for component in [
            b"long-directory".to_vec(),
            [b'"', b'\\', b'\n', 0xff].repeat(3),
        ] {
            let mut prefix = component;
            prefix.push(b'/');
            let prefix = prefix.repeat(200);
            let path = |name: String| {
                let mut bytes = prefix.clone();
                bytes.extend_from_slice(name.as_bytes());
                git::RepoPath::new(bytes).unwrap()
            };
            let files: Vec<git::ChangedFile> = (0..MAX_FILES)
                .map(|index| git::ChangedFile {
                    path: path(format!("new-{index}")),
                    old_path: Some(path(format!("old-{index}"))),
                    status: git::ChangeStatus::Renamed { similarity: 100 },
                    old_mode: 0o100_644,
                    new_mode: 0o100_644,
                    old_id: None,
                    new_id: None,
                    added: 2,
                    removed: 1,
                    binary: false,
                })
                .collect();
            let notices = content_notices(
                &files
                    .iter()
                    .take(64)
                    .map(|file| file.path.clone())
                    .collect::<Vec<_>>(),
                &[],
            );
            for notices in [Vec::new(), notices] {
                let list = change_list(&resolved, &files, &[], notices);
                let reply = ClientEnvelope::new(ClientBody::Reply(Reply::ok(
                    Id::from_random_bytes([1; 16]),
                    ReplyData::ChangesList(list.clone()),
                )));
                lade_protocol::encode(&reply, lade_protocol::Framing::Client)
                    .expect("long paths must yield a bounded change list");
                assert!(!list.files.is_empty());
                assert!(list.truncated);
                assert_eq!(list.total_added.value(), (MAX_FILES * 2) as u64);
                assert_eq!(list.total_removed.value(), MAX_FILES as u64);
            }
        }
    }

    #[test]
    fn file_and_content_searches_with_long_paths_fit_the_wire_frame() {
        let tmp = adapters_support::TempDir::new("search-long-paths");
        let dir = adapters_support::init_repo(tmp.path(), "sha1");
        let prefix = "long-directory/".repeat(200);
        std::fs::create_dir_all(dir.join(&prefix)).unwrap();
        for index in 0..200 {
            std::fs::write(dir.join(format!("{prefix}file-{index:03}")), "needle\n").unwrap();
        }
        let commit = adapters_support::commit_all(&dir, "long paths");
        let repo = Repository::open(&dir).unwrap();
        let tree = GitId::new(GitAlgorithm::Sha1, &commit).unwrap();
        for (scope, query) in [
            (SearchScope::Files, "file"),
            (SearchScope::Content, "needle"),
        ] {
            let query = SearchQuery {
                workspace_id: Id::from_random_bytes([1; 16]),
                scope,
                query: query.into(),
                limit: PageLimit::parse("200").unwrap(),
                objective_id: None,
                basis: None,
            };
            let data = search_tree(&repo, &tree, &query, "start commit").unwrap();
            let reply = ClientEnvelope::new(ClientBody::Reply(Reply::ok(
                Id::from_random_bytes([2; 16]),
                data.clone(),
            )));
            lade_protocol::encode(&reply, lade_protocol::Framing::Client)
                .expect("long paths must yield bounded search results");
            let ReplyData::Search(found) = data else {
                panic!("expected search results")
            };
            assert!(!found.results.is_empty());
            assert!(found.truncated);
            assert_eq!(
                found.results[0].path.as_ref().unwrap().as_bytes(),
                format!("{prefix}file-000").as_bytes()
            );
        }
    }

    #[test]
    fn file_search_preserves_non_ascii_query_bytes() {
        let tmp = adapters_support::TempDir::new("search-non-ascii");
        let dir = adapters_support::init_repo(tmp.path(), "sha1");
        std::fs::write(dir.join("ÉCOLE.rs"), "text\n").unwrap();
        let commit = adapters_support::commit_all(&dir, "unicode file name");
        let repo = Repository::open(&dir).unwrap();
        let tree = GitId::new(GitAlgorithm::Sha1, &commit).unwrap();
        let query = SearchQuery {
            workspace_id: Id::from_random_bytes([1; 16]),
            scope: SearchScope::Files,
            query: "École.RS".into(),
            limit: PageLimit::parse("10").unwrap(),
            objective_id: None,
            basis: None,
        };
        let ReplyData::Search(found) = search_tree(&repo, &tree, &query, "start commit").unwrap()
        else {
            panic!("expected search results")
        };
        assert_eq!(found.results.len(), 1);
        assert_eq!(
            found.results[0].path.as_ref().unwrap().as_bytes(),
            "ÉCOLE.rs".as_bytes()
        );
        assert!(!found.truncated);
    }
}
