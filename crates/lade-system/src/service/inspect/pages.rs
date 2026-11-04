//! The bounded pages an inspection request returns, built off the service thread: change
//! lists, file content, diffs and search results, each sized to fit one wire frame.

use lade_core::domain::ids::GitId;
use lade_protocol::client::{
    ChangeList, ChangeStatus, ChangedFile, ContentNotice, ContentNoticeKind, DiffGetQuery,
    DiffHunk, DiffLine, DiffLineKind, FileContent, FileDiff, FileEncoding, FileReadQuery,
    Rejection, ReplyData, SearchQuery, SearchResult, SearchResultKind, SearchResults, SearchScope,
    Side,
};
use lade_protocol::json_len;
use lade_protocol::types::{Counter, Digest};

use crate::adapters::artifacts::{ArtifactDigest, ArtifactStore, MAX_ARTIFACT_PAGE};
use crate::adapters::git::{self, GrepQuery, LineKind, ObjectKind, Repository};
use crate::storage::CaptureRow;

use crate::service::convert::{
    adapter_git, git_rejection, invalid, not_found, preview, repo_path, wire_id, wire_path,
};
use crate::service::jobs::{self, FileBytes, LargeBlobs};

use super::{MAX_DIFF_LINES, MAX_DIFF_TEXT, MAX_FILES, MAX_PAGE_JSON, Resolved};

pub(super) fn wire_status(status: git::ChangeStatus) -> ChangeStatus {
    match status {
        git::ChangeStatus::Added | git::ChangeStatus::Copied { .. } => ChangeStatus::Added,
        git::ChangeStatus::Modified => ChangeStatus::Modified,
        git::ChangeStatus::Deleted => ChangeStatus::Deleted,
        git::ChangeStatus::Renamed { .. } => ChangeStatus::Renamed,
        git::ChangeStatus::TypeChanged => ChangeStatus::TypeChanged,
    }
}

pub(super) fn content_notices(
    skipped: &[git::RepoPath],
    filtered: &[git::RepoPath],
) -> Vec<ContentNotice> {
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

pub(super) fn change_list(
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

pub(super) fn capture_label(capture: &CaptureRow) -> String {
    format!("working capture {}", capture.id.uuid().short())
}

pub(super) fn json_cost(text: &str) -> usize {
    text.chars()
        .map(|c| match c {
            '"' | '\\' => 2,
            c if (c as u32) < 0x20 => 6,
            c => c.len_utf8(),
        })
        .sum()
}

pub(super) fn file_page(
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

pub(super) fn chunk_page(
    artifacts: &ArtifactStore,
    digest: &ArtifactDigest,
    query: &FileReadQuery,
    label: &str,
) -> Result<FileContent, Rejection> {
    let unreadable = |_| crate::service::convert::durability("retained content unreadable");
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

pub(super) fn retain(artifacts: &ArtifactStore, bytes: &[u8]) -> Result<Digest, Rejection> {
    let digest = artifacts
        .put(bytes)
        .map_err(|_| crate::service::convert::durability("content could not be retained"))?;
    Digest::parse(digest.to_hex()).map_err(|_| invalid("the content digest is not canonical"))
}

pub(super) fn read_file(
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

pub(super) fn read_diff(
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

pub(super) fn wire_hunk(hunk: &git::Hunk, lines: Vec<DiffLine>) -> DiffHunk {
    DiffHunk {
        old_start: Counter::new(hunk.old_start),
        old_lines: Counter::new(hunk.old_lines),
        new_start: Counter::new(hunk.new_start),
        new_lines: Counter::new(hunk.new_lines),
        lines,
    }
}

pub(super) fn search_tree(
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
