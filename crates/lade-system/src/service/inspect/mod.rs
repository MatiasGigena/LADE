use std::sync::Arc;

use lade_core::domain::ids::{CandidateId, GitId, ObjectiveId};
use lade_protocol::client::{
    ActivityPage, ActivityPageQuery, Basis, ChangesListQuery, ClientBody, ClientEnvelope,
    DiffGetQuery, ErrorCode, FileReadQuery, Rejection, Reply, ReplyData, Request, SafeAction,
    SearchQuery, SearchResult, SearchResultKind, SearchResults, SearchScope, TargetRef,
};
use lade_protocol::types::Counter;
use lade_protocol::{Framing, encode, json_len};

use crate::adapters::artifacts::ArtifactStore;
use crate::adapters::git::Repository;
use crate::storage::{self, CaptureRow};

use super::capture::CapturePurpose;
use super::convert::{
    adapter_git, git_rejection, not_found, parse_uuid, preview, wire_git, wire_id,
};
use super::execution::wire_activity;
use super::handlers::sql;
use super::jobs::{self, CaptureOut};
use super::{Completed, Flow, ReqCtx, Service};

mod pages;
#[cfg(test)]
mod tests;

use pages::{capture_label, change_list, content_notices, read_diff, read_file, search_tree};

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
