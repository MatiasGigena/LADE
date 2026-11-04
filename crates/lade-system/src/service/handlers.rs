use std::collections::HashMap;
use std::path::PathBuf;

use lade_core::Revision;
use lade_core::domain::feedback::{
    Acceptance, BatchEntry, DraftState, SaveDraft, accept_feedback, accept_message, save_draft,
};
use lade_core::domain::ids::{
    CandidateId, CaptureId, DraftId, InputId, ObjectiveId, OperationId, SessionId, WorkspaceId,
};
use lade_core::domain::inspection::{
    ArtifactDigest as CoreDigest, CurrentContent, InspectedVersion,
};
use lade_core::domain::objectives::{BranchName, Intent, NewObjective, Objective, Title};
use lade_core::domain::review::{IntegrationPlan, Review};
use lade_protocol::client::{
    ClientBody, ClientEnvelope, Draft as WireDraft, DraftCursor, DraftDeleteCommand, DraftList,
    DraftSaveCommand, DraftState as WireDraftState, DraftsListQuery, Empty, ErrorCode, Event,
    EventFrame, FeedbackSubmitCommand, InputList, InputsListQuery, MessageSendCommand,
    ObjectiveCreateCommand, ObjectiveSelector, OperationGetQuery, OperationState, OperationStatus,
    Outcome, Rejection, Reply, ReplyData, Request, SafeAction, Settings, SettingsUpdateCommand,
    SnapshotData, Start, UpdateIntentCommand, WorkspaceOpenCommand,
};
use lade_protocol::types::{Counter, Id};
use lade_protocol::{Framing, MAX_FRAME, encode};

use crate::adapters::artifacts::ArtifactDigest;
use crate::adapters::git;
use crate::storage::{self, CaptureRow, ObjectiveRow, WorkspaceRow};

use super::convert::{
    conflict, core_reference, domain_rejection, durability, from_wire_git, git_rejection, invalid,
    not_found, parse_uuid, wire_id,
};
use super::convert::{core_mode, wire_draft, wire_input};
use super::jobs::{self, CaptureInput, CaptureOut, LatestCapture};
use super::state::{ObjState, Runtime, WsState};
use super::{Flow, JobOutput, ReqCtx, Service};
use crate::fault::{self, Fault};
use crate::support::{new_uuid, now_ms, wire_ts};

pub(crate) enum Pending {
    Launch {
        oid: ObjectiveId,
        generation: u64,
    },
    Capture {
        oid: ObjectiveId,
        purpose: CapturePurpose,
    },
    Freeze {
        oid: ObjectiveId,
        ctx: ReqCtx,
        candidate_id: CandidateId,
    },
    Refresh {
        oid: ObjectiveId,
        ctx: ReqCtx,
        candidate_id: CandidateId,
        new_id: CandidateId,
    },
    Verify {
        oid: ObjectiveId,
        verification_id: lade_core::domain::ids::VerificationId,
    },
    Integrate {
        oid: ObjectiveId,
        ctx: ReqCtx,
        plan: IntegrationPlan,
        journal_id: lade_core::domain::ids::Uuid,
    },
    Cleanup {
        oid: ObjectiveId,
        ctx: ReqCtx,
        confirm: bool,
    },
    DeleteHistory {
        oid: ObjectiveId,
        ctx: ReqCtx,
    },
    Complete {
        oid: ObjectiveId,
        ctx: ReqCtx,
    },
    Query {
        ctx: ReqCtx,
    },
    Catalog,
    ProfileImport {
        ctx: ReqCtx,
    },
    Reload {
        oid: ObjectiveId,
        ctx: ReqCtx,
    },
    TerminalOpen {
        oid: ObjectiveId,
        ctx: ReqCtx,
        terminal_id: Id,
        writable: bool,
        size: (u16, u16),
    },
    Storage,
    Ignore,
}

pub(crate) enum CapturePurpose {
    Idle,
    Changes(ReqCtx),
    Retry(ReqCtx, Box<Request>),
    Submit(ReqCtx, Box<FeedbackSubmitCommand>),
    DispatchCheck(InputId),
}

pub(super) const SNAPSHOT_OBJECTIVES: usize = 400;
const SNAPSHOT_BRANCHES: usize = 64;
const REPLY_BRANCHES: usize = 256;

fn reply_json_len(request_id: &Id, data: ReplyData) -> usize {
    serde_json::to_vec(&ClientEnvelope::new(ClientBody::Reply(Reply::ok(
        request_id.clone(),
        data,
    ))))
    .expect("reply fields are serializable")
    .len()
}

fn history_too_large() -> Rejection {
    Rejection::new(
        ErrorCode::LimitReached,
        SafeAction::None,
        "This saved record exceeds the reply limit and cannot be shown.",
    )
}

pub(crate) fn sql<T>(result: rusqlite::Result<T>) -> Result<T, Rejection> {
    result.map_err(|_| durability("LADE couldn't record this, so nothing was changed. Try again."))
}

impl Service {
    pub(crate) fn objective_id(&self, id: &Id) -> Result<ObjectiveId, Rejection> {
        let oid = ObjectiveId::new(parse_uuid(id)?);
        if self.objectives.contains_key(&oid) {
            Ok(oid)
        } else {
            Err(not_found("objective"))
        }
    }

    pub(crate) fn obj(&self, oid: ObjectiveId) -> Result<&ObjState, Rejection> {
        self.objectives
            .get(&oid)
            .ok_or_else(|| not_found("objective"))
    }

    pub(crate) fn ws_of(&self, st: &ObjState) -> Result<&WsState, Rejection> {
        self.workspaces
            .get(&st.objective().record().workspace_id)
            .ok_or_else(|| not_found("workspace"))
    }

    pub(crate) fn rt(&mut self, oid: ObjectiveId) -> &mut Runtime {
        self.runtime.entry(oid).or_default()
    }

    pub(crate) fn check_idle(&self, oid: ObjectiveId) -> Result<(), Rejection> {
        match self.runtime.get(&oid).and_then(|r| r.busy) {
            Some(_) => Err(conflict(super::project::BUSY)),
            None => Ok(()),
        }
    }

    pub(crate) fn save_tx(tx: &rusqlite::Connection, st: &ObjState) -> rusqlite::Result<()> {
        storage::save_objective(tx, &st.row)?;
        storage::save_review(tx, &st.review)
    }

    pub(crate) fn commit_obj(
        &mut self,
        ctx: Option<&mut ReqCtx>,
        outcome: Option<&ReplyData>,
        st: ObjState,
    ) -> Result<(), Rejection> {
        self.commit(ctx, outcome, |tx| Self::save_tx(tx, &st))?;
        self.objectives.insert(st.objective().id(), st);
        Ok(())
    }

    pub(crate) fn dispatch(&mut self, mut ctx: ReqCtx, request: Request) -> Flow {
        let result = match request {
            Request::Snapshot(_) => Ok(self.snapshot()),
            Request::ObjectiveGet(ObjectiveSelector { objective_id }) => {
                self.objective_id(&objective_id).map(|oid| {
                    let st = self.objectives[&oid].clone();
                    ReplyData::ObjectiveGet(Box::new(self.detail_of(&st)))
                })
            }
            Request::DraftsList(query) => self.drafts_list(&ctx.request_id, &query),
            Request::InputsList(query) => self.inputs_list(&ctx.request_id, &query),
            Request::OperationGet(query) => self.operation_get(&query),
            Request::ActivityPage(query) => self.activity_page(&query),
            Request::ChangesList(query) => return self.changes_list(ctx, query),
            Request::FileRead(query) => return self.file_read(ctx, query),
            Request::DiffGet(query) => return self.diff_get(ctx, query),
            Request::Search(query) => return self.search(ctx, query),
            Request::WorkspaceOpen(cmd) => self.workspace_open(&cmd),
            Request::ObjectiveCreate(cmd) => self.objective_create(&mut ctx, &cmd),
            Request::ObjectiveUpdateIntent(cmd) => self.update_intent(&mut ctx, &cmd),
            Request::SettingsUpdate(cmd) => self.settings_update(&mut ctx, cmd),
            Request::DraftSave(cmd) => self.draft_save(&mut ctx, &cmd),
            Request::DraftDelete(cmd) => self.draft_delete(&mut ctx, &cmd),
            Request::MessageSend(cmd) => return self.message_send(ctx, cmd),
            Request::FeedbackSubmit(cmd) => return self.feedback_submit(ctx, cmd),
            Request::ExecutionStart(cmd) => self.execution_start(&mut ctx, &cmd),
            Request::ExecutionCancel(ObjectiveSelector { objective_id }) => {
                self.execution_cancel(&mut ctx, &objective_id)
            }
            Request::ExecutionResume(ObjectiveSelector { objective_id }) => {
                self.execution_resume(&mut ctx, &objective_id)
            }
            Request::ReviewFreeze(ObjectiveSelector { objective_id }) => {
                return self.review_freeze(ctx, &objective_id);
            }
            Request::ReviewReturn(cmd) => self.review_return(&mut ctx, &cmd),
            Request::ReviewVerify(cmd) => self.review_verify(&mut ctx, cmd),
            Request::ReviewApprove(cmd) => self.review_approve(&mut ctx, cmd),
            Request::ReviewRefresh(cmd) => return self.review_refresh(ctx, &cmd),
            Request::ReviewIntegrate(cmd) => return self.review_integrate(ctx, &cmd),
            Request::ObjectiveComplete(ObjectiveSelector { objective_id }) => {
                return self.objective_complete(ctx, &objective_id);
            }
            Request::ObjectiveDiscard(ObjectiveSelector { objective_id }) => {
                self.objective_discard(&mut ctx, &objective_id)
            }
            Request::ObjectiveCleanup(cmd) => return self.objective_cleanup(ctx, &cmd),
            Request::ObjectiveDeleteHistory(cmd) => {
                return self.objective_delete_history(ctx, &cmd);
            }
            Request::ModelsList(query) => return self.models_list(ctx, query),
            Request::SessionTree(ObjectiveSelector { objective_id }) => {
                return self.session_tree(ctx, &objective_id);
            }
            Request::ResourcesList(_) => Ok(ReplyData::ResourcesList(self.inventory())),
            Request::SessionNavigate(cmd) => return self.session_navigate(ctx, cmd),
            Request::SessionFork(cmd) => return self.session_fork(ctx, cmd),
            Request::SessionImport(cmd) => return self.session_import(ctx, &cmd),
            Request::SessionExport(ObjectiveSelector { objective_id }) => {
                return self.session_export(ctx, &objective_id);
            }
            Request::SessionCompact(cmd) => return self.session_compact(ctx, cmd),
            Request::SessionReload(ObjectiveSelector { objective_id }) => {
                return self.session_reload(ctx, &objective_id);
            }
            Request::SessionConfigure(cmd) => return self.session_configure(ctx, cmd),
            Request::ResourceDecide(cmd) => self.resource_decide(&mut ctx, &cmd),
            Request::ProfileImport(cmd) => return self.profile_import(ctx, &cmd),
            Request::UiRespond(cmd) => self.ui_respond(&mut ctx, cmd),
            Request::TerminalOpen(cmd) => return self.terminal_open(ctx, &cmd),
            Request::TerminalInput(cmd) => self.terminal_input(&cmd),
            Request::TerminalResize(cmd) => self.terminal_resize(&cmd),
            Request::TerminalClose(cmd) => self.terminal_close(&cmd),
            Request::ServiceStop(cmd) => {
                eprintln!("laded: service.stop reason={:?}", cmd.reason);
                self.stopping = true;
                Ok(ReplyData::ServiceStop(Empty {}))
            }
        };
        self.finish(ctx, result);
        Flow::Done
    }

    pub(crate) fn on_job(&mut self, token: u64, output: JobOutput) {
        let Some(pending) = self.pending.remove(&token) else {
            return;
        };
        match (pending, output) {
            (Pending::Launch { oid, generation }, JobOutput::Launch(out)) => {
                self.launch_done(oid, generation, out);
            }
            (Pending::Storage, JobOutput::Storage(usage)) => self.storage_measured(usage),
            (Pending::Capture { oid, purpose }, JobOutput::Capture(result)) => {
                self.capture_done(oid, purpose, result);
            }
            (
                Pending::Freeze {
                    oid,
                    ctx,
                    candidate_id,
                },
                JobOutput::Freeze(result),
            ) => {
                self.freeze_done(oid, ctx, candidate_id, result);
            }
            (
                Pending::Refresh {
                    oid,
                    ctx,
                    candidate_id,
                    new_id,
                },
                JobOutput::Refresh(result),
            ) => self.refresh_done(oid, ctx, candidate_id, new_id, result),
            (
                Pending::Verify {
                    oid,
                    verification_id,
                },
                JobOutput::Verify(out),
            ) => {
                self.verify_done(oid, verification_id, out);
            }
            (
                Pending::Integrate {
                    oid,
                    ctx,
                    plan,
                    journal_id,
                },
                JobOutput::Integrate(result),
            ) => self.integrate_done(oid, ctx, &plan, journal_id, result),
            (Pending::Cleanup { oid, ctx, confirm }, JobOutput::Unit(result)) => {
                self.cleanup_done(oid, ctx, confirm, result);
            }
            (Pending::DeleteHistory { oid, ctx }, JobOutput::Unit(result)) => {
                self.delete_history_done(oid, ctx, result);
            }
            (Pending::Complete { oid, ctx }, JobOutput::Flag(result)) => {
                self.complete_done(oid, ctx, result);
            }
            (Pending::Query { ctx }, JobOutput::Query(result)) => self.finish(ctx, result),
            (Pending::Catalog, JobOutput::Catalog(result)) => self.catalog_done(result),
            (Pending::ProfileImport { ctx }, JobOutput::Profile(result)) => {
                self.profile_import_done(ctx, result);
            }
            (Pending::Reload { oid, ctx }, JobOutput::Snapshot(result)) => {
                self.reload_snapshot_done(oid, ctx, result);
            }
            (
                Pending::TerminalOpen {
                    oid,
                    ctx,
                    terminal_id,
                    writable,
                    size,
                },
                JobOutput::Terminal(result),
            ) => self.terminal_open_done(oid, ctx, (terminal_id, writable, size), result),
            _ => {}
        }
    }

    fn snapshot(&self) -> ReplyData {
        ReplyData::Snapshot(self.budget_snapshot(self.snapshot_data(
            &self.settings,
            None,
            None,
            SNAPSHOT_BRANCHES,
        )))
    }

    pub(crate) fn snapshot_data(
        &self,
        settings: &Settings,
        objective: Option<&ObjState>,
        workspace: Option<&WsState>,
        max_branches: usize,
    ) -> SnapshotData {
        let mut workspace_states: Vec<_> = self.workspaces.values().chain(workspace).collect();
        workspace_states.sort_by_key(|ws| ws.id);
        let workspaces = workspace_states
            .into_iter()
            .take(64)
            .map(|ws| self.wire_workspace(ws, max_branches))
            .collect();
        let mut states: Vec<&ObjState> = self.objectives.values().chain(objective).collect();
        states.sort_by_key(|st| {
            (
                std::cmp::Reverse(st.objective().record().updated_at),
                st.objective().id(),
            )
        });
        let objectives = states
            .into_iter()
            .take(SNAPSHOT_OBJECTIVES)
            .map(|st| self.summary(st))
            .collect();
        SnapshotData {
            service_epoch: self.epoch.clone(),
            sequence: Counter::new(self.sequence),
            workspaces,
            objectives,
            settings: settings.clone(),
        }
    }

    fn drafts_list(
        &self,
        request_id: &Id,
        query: &DraftsListQuery,
    ) -> Result<ReplyData, Rejection> {
        let st = self.obj(self.objective_id(&query.objective_id)?)?;
        let after = query
            .after
            .as_ref()
            .map(|cursor| {
                parse_uuid(&cursor.draft_id)
                    .map(|id| (cursor.created_at.unix_ms(), DraftId::new(id)))
            })
            .transpose()?;
        let mut eligible: Vec<_> = st
            .drafts
            .iter()
            .filter(|draft| {
                after.is_none_or(|key| (draft.record().created_at.unix_ms(), draft.id()) > key)
            })
            .collect();
        eligible.sort_by_key(|draft| (draft.record().created_at.unix_ms(), draft.id()));
        let mut drafts = Vec::new();
        let mut content_bytes = 0;
        let mut last = None;
        for (index, draft) in eligible
            .iter()
            .enumerate()
            .take(query.limit.value() as usize)
        {
            let wire = wire_draft(st, draft);
            let cursor = DraftCursor {
                created_at: wire_ts(draft.record().created_at.unix_ms()),
                draft_id: wire.draft_id.clone(),
            };
            let more = index + 1 < eligible.len();
            let overhead = reply_json_len(
                request_id,
                ReplyData::DraftsList(DraftList {
                    drafts: Vec::new(),
                    next_after: more.then_some(cursor.clone()),
                }),
            );
            let next_content = content_bytes
                + usize::from(!drafts.is_empty())
                + serde_json::to_vec(&wire)
                    .expect("draft fields are serializable")
                    .len();
            if overhead + next_content > MAX_FRAME {
                break;
            }
            drafts.push(wire);
            content_bytes = next_content;
            last = Some(cursor);
        }
        if drafts.is_empty() && !eligible.is_empty() {
            return Err(history_too_large());
        }
        let next_after = (drafts.len() < eligible.len()).then_some(last).flatten();
        Ok(ReplyData::DraftsList(DraftList { drafts, next_after }))
    }

    fn inputs_list(
        &self,
        request_id: &Id,
        query: &InputsListQuery,
    ) -> Result<ReplyData, Rejection> {
        let st = self.obj(self.objective_id(&query.objective_id)?)?;
        let after = query.after.as_ref().map_or(0, |cursor| cursor.value());
        let mut eligible: Vec<_> = st.inputs.iter().filter(|(seq, _)| *seq > after).collect();
        eligible.sort_by_key(|(seq, _)| *seq);
        let mut inputs = Vec::new();
        let mut content_bytes = 0;
        let mut last = None;
        for (index, (seq, input)) in eligible
            .iter()
            .enumerate()
            .take(query.limit.value() as usize)
        {
            let wire = wire_input(input);
            let cursor = Counter::new(*seq);
            let more = index + 1 < eligible.len();
            let overhead = reply_json_len(
                request_id,
                ReplyData::InputsList(InputList {
                    inputs: Vec::new(),
                    next_after: more.then_some(cursor.clone()),
                }),
            );
            let next_content = content_bytes
                + usize::from(!inputs.is_empty())
                + serde_json::to_vec(&wire)
                    .expect("input fields are serializable")
                    .len();
            if overhead + next_content > MAX_FRAME {
                break;
            }
            inputs.push(wire);
            content_bytes = next_content;
            last = Some(cursor);
        }
        if inputs.is_empty() && !eligible.is_empty() {
            return Err(history_too_large());
        }
        let next_after = (inputs.len() < eligible.len()).then_some(last).flatten();
        Ok(ReplyData::InputsList(InputList { inputs, next_after }))
    }

    fn check_draft_representation(
        &self,
        request_id: &Id,
        mut draft: WireDraft,
        cursor: DraftCursor,
    ) -> Result<(), Rejection> {
        draft.revision = Counter::new(u64::MAX);
        draft.state = WireDraftState::Submitted;
        draft.input_id = Some(request_id.clone());
        draft.stale = false;
        let fits_reply = |data| {
            encode(
                &ClientEnvelope::new(ClientBody::Reply(Reply::ok(request_id.clone(), data))),
                Framing::Client,
            )
            .is_ok()
        };
        let fits = fits_reply(ReplyData::DraftSave(draft.clone()))
            && fits_reply(ReplyData::DraftsList(DraftList {
                drafts: vec![draft.clone()],
                next_after: Some(cursor),
            }))
            && fits_reply(ReplyData::OperationGet(OperationStatus {
                state: OperationState::Completed,
                outcome: Some(Outcome::Ok(Box::new(ReplyData::DraftSave(draft.clone())))),
            }))
            && encode(
                &ClientEnvelope::new(ClientBody::Event(EventFrame {
                    service_epoch: self.epoch.clone(),
                    sequence: Counter::new(u64::MAX),
                    event: Event::DraftChanged(draft),
                })),
                Framing::Client,
            )
            .is_ok();
        if !fits {
            return Err(Rejection::new(
                ErrorCode::LimitReached,
                SafeAction::None,
                "This draft is too large to save. Shorten the comment or selection and try again.",
            ));
        }
        Ok(())
    }

    fn operation_get(&self, query: &OperationGetQuery) -> Result<ReplyData, Rejection> {
        let row = sql(storage::load_operation(
            self.store.conn(),
            &query.operation_id,
        ))?;
        let status = match row {
            None => OperationStatus {
                state: OperationState::Unknown,
                outcome: None,
            },
            Some(row) => OperationStatus {
                state: match row.state {
                    storage::OperationState::Pending => OperationState::Pending,
                    storage::OperationState::Uncertain => OperationState::Uncertain,
                    storage::OperationState::Completed => OperationState::Completed,
                },
                outcome: row.outcome,
            },
        };
        Ok(ReplyData::OperationGet(status))
    }

    fn workspace_open(&mut self, cmd: &WorkspaceOpenCommand) -> Result<ReplyData, Rejection> {
        let path = PathBuf::from(cmd.path.to_os_string());
        if !path.is_absolute() {
            return Err(invalid("workspace path must be absolute"));
        }
        let repo = jobs::open_repository(&path).map_err(git_rejection)?;
        let top = repo.work_tree().to_path_buf();
        if let Some(existing) = self.workspaces.values().find(|ws| ws.path == top) {
            return Ok(ReplyData::WorkspaceOpen(
                self.wire_workspace(existing, REPLY_BRANCHES),
            ));
        }
        let (row, fresh) = match sql(storage::workspace_by_path(self.store.conn(), &top))? {
            Some(row) => (row, false),
            None => {
                let row = WorkspaceRow {
                    id: WorkspaceId::new(new_uuid()),
                    path: top.clone(),
                    name: top.file_name().map_or_else(
                        || "workspace".to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    ),
                    default_branch: repo.head_branch().ok().flatten(),
                    opened_at: now_ms(),
                };
                (row, true)
            }
        };
        let ws = WsState {
            id: row.id,
            repo,
            path: top,
            name: row.name.clone(),
            default_branch: row.default_branch.clone(),
        };
        self.check_snapshot_representation(&self.settings, None, Some(&ws))?;
        if fresh {
            sql(storage::insert_workspace(self.store.conn(), &row))?;
        }
        let reply = self.wire_workspace(&ws, REPLY_BRANCHES);
        self.workspaces.insert(ws.id, ws);
        Ok(ReplyData::WorkspaceOpen(reply))
    }

    fn objective_create(
        &mut self,
        ctx: &mut ReqCtx,
        cmd: &ObjectiveCreateCommand,
    ) -> Result<ReplyData, Rejection> {
        let workspace = WorkspaceId::new(parse_uuid(&cmd.workspace_id)?);
        let ws = self
            .workspaces
            .get(&workspace)
            .ok_or_else(|| not_found("workspace"))?;
        let title = Title::new(&cmd.title).map_err(domain_rejection)?;
        let intent = Intent::new(
            &cmd.intent.outcome,
            &cmd.intent.constraints,
            &cmd.intent.acceptance,
        )
        .map_err(domain_rejection)?;
        if cmd.integration_target.contains("\\x")
            && git::unescape_branch_name(&cmd.integration_target)
                .is_some_and(|raw| std::str::from_utf8(&raw).is_err())
        {
            return Err(Rejection::new(
                ErrorCode::UnsupportedContent,
                SafeAction::None,
                "branch names that are not UTF-8 are listed but cannot be an integration target",
            ));
        }
        let target = BranchName::new(&cmd.integration_target).map_err(domain_rejection)?;
        let target_commit = jobs::target_commit(&ws.repo, target.as_str())?;
        let start_commit = match &cmd.start {
            Start::Target => target_commit,
            Start::Commit { commit } => {
                let requested = from_wire_git(commit)?;
                let resolved = ws
                    .repo
                    .resolve_commit(requested.hex())
                    .map_err(git_rejection)?;
                super::convert::core_git(&resolved)
            }
        };
        if start_commit.algorithm() != super::convert::core_algorithm(ws.repo.object_format()) {
            return Err(invalid(
                "start commit object format differs from the repository",
            ));
        }
        let oid = ObjectiveId::new(new_uuid());
        let objective = Objective::define(
            NewObjective {
                id: oid,
                workspace_id: workspace,
                title,
                intent,
                integration_target: target,
                start_commit,
            },
            lade_core::domain::ids::Timestamp::from_unix_ms(now_ms()),
        );
        let st = ObjState {
            row: ObjectiveRow {
                objective,
                git_branch: None,
                worktree: None,
                integration_conflict: false,
                history_deleted: false,
            },
            review: Review::new(oid),
            drafts: Vec::new(),
            draft_stale: HashMap::new(),
            inputs: Vec::new(),
            capture: None,
            tails: HashMap::new(),
        };
        let detail = self.detail_of(&st);
        self.check_intent_representation(&detail)?;
        self.check_snapshot_representation(&self.settings, Some(&st), None)?;
        let data = ReplyData::ObjectiveCreate(Box::new(detail));
        self.commit_obj(Some(ctx), Some(&data), st)?;
        self.emit_objective(oid);
        Ok(data)
    }

    fn update_intent(
        &mut self,
        ctx: &mut ReqCtx,
        cmd: &UpdateIntentCommand,
    ) -> Result<ReplyData, Rejection> {
        let oid = self.objective_id(&cmd.objective_id)?;
        self.check_idle(oid)?;
        let intent = Intent::new(
            &cmd.intent.outcome,
            &cmd.intent.constraints,
            &cmd.intent.acceptance,
        )
        .map_err(domain_rejection)?;
        let mut st = self.obj(oid)?.clone();
        st.objective_mut()
            .update_intent(
                Revision::new(cmd.expected_intent_revision.value()),
                intent,
                lade_core::domain::ids::Timestamp::from_unix_ms(now_ms()),
            )
            .map_err(domain_rejection)?;
        let detail = self.detail_of(&st);
        self.check_intent_representation(&detail)?;
        let data = ReplyData::ObjectiveUpdateIntent(Box::new(detail));
        self.commit_obj(Some(ctx), Some(&data), st)?;
        self.emit_objective(oid);
        Ok(data)
    }

    fn settings_update(
        &mut self,
        ctx: &mut ReqCtx,
        cmd: SettingsUpdateCommand,
    ) -> Result<ReplyData, Rejection> {
        let mut settings = self.settings.clone();
        if let Some(model) = cmd.default_model {
            lade_core::domain::execution::ModelRef::new(&model.provider, &model.id)
                .map_err(domain_rejection)?;
            settings.default_model = Some(model);
        }
        if let Some(commands) = cmd.verification_commands {
            let valid = commands.iter().all(|argv| {
                argv.first().is_some_and(|p| !p.is_empty())
                    && argv.iter().all(|a| !a.contains('\0'))
            });
            if !valid {
                return Err(invalid(
                    "verification commands need a program and no NUL bytes",
                ));
            }
            settings.verification_commands = commands;
        }
        let data = ReplyData::SettingsUpdate(settings.clone());
        self.check_snapshot_representation(&settings, None, None)?;
        self.commit(Some(ctx), Some(&data), |tx| {
            storage::save_settings(tx, &settings)
        })?;
        self.settings = settings;
        Ok(data)
    }

    fn draft_save(
        &mut self,
        ctx: &mut ReqCtx,
        cmd: &DraftSaveCommand,
    ) -> Result<ReplyData, Rejection> {
        let oid = self.objective_id(&cmd.objective_id)?;
        let draft_id = DraftId::new(parse_uuid(&cmd.draft_id)?);
        let reference = core_reference(&cmd.reference)?;
        let st = self.obj(oid)?;
        match reference.version() {
            InspectedVersion::Working { capture_id } => {
                let row = sql(storage::load_capture(self.store.conn(), capture_id))?;
                if row.is_none_or(|r| r.objective_id != oid) {
                    return Err(not_found("working capture"));
                }
            }
            InspectedVersion::Candidate { candidate_id } => {
                if st.review.candidate(candidate_id).is_none() {
                    return Err(not_found("candidate"));
                }
            }
        }
        let digest = ArtifactDigest::parse(reference.artifact_digest().hex())
            .map_err(|_| invalid("invalid artifact digest"))?;
        if !self.artifacts.contains(&digest) {
            return Err(Rejection::new(
                ErrorCode::NotFound,
                SafeAction::None,
                "The commented file version is no longer available. Open the file again.",
            ));
        }
        let range = reference.byte_range();
        let page = self
            .artifacts
            .read_page(
                &digest,
                range.start(),
                usize::try_from(range.byte_len()).unwrap_or(usize::MAX),
            )
            .map_err(|_| {
                durability("The commented file version couldn't be read. Open the file again.")
            })?;
        if page.offset != range.start() || page.data != reference.selected_text().as_bytes() {
            return Err(invalid(
                "The selected text no longer matches the file. Select it again.",
            ));
        }
        let existing = st.draft(draft_id).cloned();
        if existing.is_none()
            && self
                .objectives
                .values()
                .any(|other| other.draft(draft_id).is_some())
        {
            return Err(conflict("draft id belongs to another objective"));
        }
        let created = existing.is_none();
        let draft = save_draft(
            existing,
            st.objective(),
            SaveDraft {
                draft_id,
                objective_id: oid,
                expected_revision: Revision::new(cmd.expected_revision.value()),
                reference,
                body: cmd.body.clone(),
            },
            lade_core::domain::ids::Timestamp::from_unix_ms(now_ms()),
        )
        .map_err(domain_rejection)?;
        let mut st = st.clone();
        match st.drafts.iter_mut().find(|d| d.id() == draft_id) {
            Some(slot) => *slot = draft.clone(),
            None => st.drafts.push(draft.clone()),
        }
        st.draft_stale.insert(draft_id, false);
        let wire = wire_draft(&st, &draft);
        self.check_draft_representation(
            &ctx.request_id,
            wire.clone(),
            DraftCursor {
                created_at: wire_ts(draft.record().created_at.unix_ms()),
                draft_id: wire.draft_id.clone(),
            },
        )?;
        let data = ReplyData::DraftSave(wire.clone());
        match fault::operation("draft.save") {
            Some(Fault::Refuse) => {
                return Err(durability(
                    "The draft was not written (injected test fault).",
                ));
            }
            Some(Fault::Delay(delay)) => std::thread::sleep(delay),
            None => {}
        }
        self.commit(Some(ctx), Some(&data), |tx| {
            storage::save_draft(tx, &draft, true)
        })?;
        self.objectives.insert(oid, st);
        self.emit(Event::DraftChanged(wire));
        if created {
            self.emit_objective(oid);
        }
        Ok(data)
    }

    fn draft_delete(
        &mut self,
        ctx: &mut ReqCtx,
        cmd: &DraftDeleteCommand,
    ) -> Result<ReplyData, Rejection> {
        let draft_id = DraftId::new(parse_uuid(&cmd.draft_id)?);
        let (oid, draft) = self
            .objectives
            .iter()
            .find_map(|(oid, st)| st.draft(draft_id).map(|d| (*oid, d.clone())))
            .ok_or_else(|| not_found("draft"))?;
        draft
            .check_delete(Revision::new(cmd.expected_revision.value()))
            .map_err(domain_rejection)?;
        let data = ReplyData::DraftDelete(Empty {});
        let now = now_ms();
        self.commit(Some(ctx), Some(&data), |tx| {
            storage::delete_draft(tx, draft_id, now)
        })?;
        if let Some(st) = self.objectives.get_mut(&oid) {
            st.drafts.retain(|d| d.id() != draft_id);
            st.draft_stale.remove(&draft_id);
        }
        self.emit_objective(oid);
        Ok(data)
    }

    fn acceptance(
        st: &ObjState,
        ctx: &ReqCtx,
        mode: lade_protocol::types::DeliveryMode,
        session: Option<(SessionId, Revision)>,
    ) -> Result<Acceptance, Rejection> {
        let operation = ctx
            .op
            .as_ref()
            .ok_or_else(|| invalid("operation id required"))?;
        let (session_id, association_revision) = match session {
            Some(pair) => pair,
            None => {
                let current = st.objective().execution().session().ok_or_else(|| {
                    domain_rejection(lade_core::DomainError::ExecutionUnavailable)
                })?;
                (current.session_id, current.association_revision)
            }
        };
        Ok(Acceptance {
            input_id: InputId::new(new_uuid()),
            operation_id: OperationId::new(parse_uuid(&operation.id)?),
            session_id,
            association_revision,
            mode: core_mode(mode),
        })
    }

    fn accept_and_dispatch(
        &mut self,
        mut ctx: ReqCtx,
        mut st: ObjState,
        input: lade_core::domain::feedback::AcceptedInput,
        submitted: Vec<lade_core::domain::feedback::Draft>,
        checked: bool,
    ) {
        let oid = st.objective().id();
        let seq = self.input_seq + 1;
        let summary = wire_input(&input);
        let data = input_reply(ctx.name, summary.clone());
        let committed = self.commit(Some(&mut ctx), Some(&data), |tx| {
            storage::insert_input(tx, &input, seq)?;
            for draft in &submitted {
                storage::save_draft(tx, draft, false)?;
            }
            Ok(())
        });
        if let Err(rejection) = committed {
            self.finish(ctx, Err(rejection));
            return;
        }
        crate::fault::point("input.accepted");
        self.input_seq = seq;
        for draft in &submitted {
            if let Some(slot) = st.drafts.iter_mut().find(|d| d.id() == draft.id()) {
                *slot = draft.clone();
            }
        }
        st.inputs.push((seq, input.clone()));
        let drafts: Vec<_> = submitted.iter().map(|d| wire_draft(&st, d)).collect();
        self.objectives.insert(oid, st);
        self.finish(ctx, Ok(data));
        self.emit(Event::InputChanged(summary));
        for draft in drafts {
            self.emit(Event::DraftChanged(draft));
        }
        self.dispatch_queued(oid, checked.then_some(input.id));
        self.emit_objective(oid);
    }

    fn message_send(&mut self, ctx: ReqCtx, cmd: MessageSendCommand) -> Flow {
        let result = (|| {
            let oid = self.objective_id(&cmd.objective_id)?;
            let st = self.obj(oid)?;
            let acceptance = Self::acceptance(st, &ctx, cmd.mode, None)?;
            let input = accept_message(
                st.objective(),
                acceptance,
                cmd.text.clone(),
                lade_core::domain::ids::Timestamp::from_unix_ms(now_ms()),
            )
            .map_err(domain_rejection)?;
            Ok((st.clone(), input))
        })();
        match result {
            Ok((st, input)) => self.accept_and_dispatch(ctx, st, input, Vec::new(), true),
            Err(rejection) => self.finish(ctx, Err(rejection)),
        }
        Flow::Done
    }

    fn feedback_submit(&mut self, ctx: ReqCtx, cmd: FeedbackSubmitCommand) -> Flow {
        let prepared = (|| {
            let oid = self.objective_id(&cmd.objective_id)?;
            let st = self.obj(oid)?;
            let session = SessionId::new(parse_uuid(&cmd.destination_session_id)?);
            st.objective()
                .execution()
                .check_association(session, Revision::new(cmd.association_revision.value()))
                .map_err(domain_rejection)?;
            if st.objective().lifecycle() != lade_core::domain::objectives::Lifecycle::Active {
                return Err(domain_rejection(lade_core::DomainError::InvalidLifecycle(
                    lade_core::domain::feedback::input_refusal(st.objective().lifecycle()),
                )));
            }
            let mut drafts = Vec::new();
            for entry in &cmd.drafts {
                let id = DraftId::new(parse_uuid(&entry.draft_id)?);
                let draft = st.draft(id).ok_or_else(|| not_found("draft"))?;
                if draft.revision().value() != entry.revision.value() {
                    return Err(conflict(
                        "A draft changed after you reviewed it. Refresh and try again.",
                    ));
                }
                if !draft.is_unsent() {
                    return Err(conflict("A draft was already submitted."));
                }
                drafts.push((id, draft.record().reference.clone()));
            }
            let worktree =
                st.row.worktree.clone().ok_or_else(|| {
                    domain_rejection(lade_core::DomainError::ExecutionUnavailable)
                })?;
            Ok((oid, self.capture_input(st, worktree, drafts)?))
        })();
        match prepared {
            Ok((oid, input)) => {
                self.spawn(
                    Pending::Capture {
                        oid,
                        purpose: CapturePurpose::Submit(ctx, Box::new(cmd)),
                    },
                    move || JobOutput::Capture(jobs::capture(&input)),
                );
                Flow::Deferred
            }
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                Flow::Done
            }
        }
    }

    fn submit_after_capture(&mut self, ctx: ReqCtx, cmd: &FeedbackSubmitCommand, out: &CaptureOut) {
        let result = (|| {
            let oid = self.objective_id(&cmd.objective_id)?;
            let st = self.obj(oid)?;
            let session = SessionId::new(parse_uuid(&cmd.destination_session_id)?);
            let acceptance = Self::acceptance(
                st,
                &ctx,
                cmd.mode,
                Some((session, Revision::new(cmd.association_revision.value()))),
            )?;
            let stale: HashMap<DraftId, bool> = out.stale.iter().copied().collect();
            let mut drafts = Vec::new();
            let mut revisions = Vec::new();
            for entry in &cmd.drafts {
                let id = DraftId::new(parse_uuid(&entry.draft_id)?);
                drafts.push(st.draft(id).ok_or_else(|| not_found("draft"))?.clone());
                revisions.push(Revision::new(entry.revision.value()));
            }
            let digests: Vec<CoreDigest> = drafts
                .iter()
                .map(|d| d.record().reference.artifact_digest().clone())
                .collect();
            let mut batch: Vec<BatchEntry<'_>> = drafts
                .iter_mut()
                .zip(revisions)
                .zip(&digests)
                .map(|((draft, revision), digest)| {
                    let current = if stale.get(&draft.id()).copied().unwrap_or(true) {
                        CurrentContent::Missing
                    } else {
                        CurrentContent::Present { digest, bytes: &[] }
                    };
                    BatchEntry {
                        draft,
                        revision,
                        current,
                    }
                })
                .collect();
            let input = accept_feedback(
                st.objective(),
                acceptance,
                &mut batch,
                cmd.note.clone(),
                lade_core::domain::ids::Timestamp::from_unix_ms(now_ms()),
            )
            .map_err(domain_rejection)?;
            drop(batch);
            Ok((st.clone(), input, drafts))
        })();
        match result {
            Ok((st, input, drafts)) => {
                debug_assert!(
                    drafts
                        .iter()
                        .all(|d| matches!(d.record().state, DraftState::Submitted { .. }))
                );
                self.accept_and_dispatch(ctx, st, input, drafts, true);
            }
            Err(rejection) => self.finish(ctx, Err(rejection)),
        }
    }

    pub(crate) fn capture_input(
        &self,
        st: &ObjState,
        worktree: PathBuf,
        drafts: Vec<(DraftId, lade_core::domain::inspection::Reference)>,
    ) -> Result<CaptureInput, Rejection> {
        let ws = self.ws_of(st)?;
        Ok(CaptureInput {
            repo: ws.repo.clone(),
            worktree,
            scratch: self.paths.tmp.clone(),
            target_branch: st
                .objective()
                .record()
                .integration_target
                .as_str()
                .to_string(),
            capture_id: CaptureId::new(new_uuid()),
            latest: st.capture.as_ref().map(|c| LatestCapture {
                id: c.id,
                tree: c.tree.clone(),
                target: c.target_commit.clone(),
            }),
            drafts,
        })
    }

    pub(crate) fn unsent_references(
        st: &ObjState,
    ) -> Vec<(DraftId, lade_core::domain::inspection::Reference)> {
        st.drafts
            .iter()
            .filter(|d| d.is_unsent())
            .map(|d| (d.id(), d.record().reference.clone()))
            .collect()
    }

    pub(crate) fn request_capture(&mut self, oid: ObjectiveId) {
        let rt = self.rt(oid);
        if rt.capture_running {
            rt.recapture = true;
            return;
        }
        let Some(st) = self.objectives.get(&oid) else {
            return;
        };
        let Some(worktree) = st.row.worktree.clone() else {
            return;
        };
        let Ok(input) = self.capture_input(st, worktree, Self::unsent_references(st)) else {
            return;
        };
        self.rt(oid).capture_running = true;
        self.spawn(
            Pending::Capture {
                oid,
                purpose: CapturePurpose::Idle,
            },
            move || JobOutput::Capture(jobs::capture(&input)),
        );
    }

    pub(crate) fn register_capture(
        &mut self,
        oid: ObjectiveId,
        out: &CaptureOut,
    ) -> Result<CaptureRow, Rejection> {
        self.obj(oid)?;
        let row = if out.reused {
            sql(storage::load_capture(self.store.conn(), out.capture_id))?
                .filter(|row| row.objective_id == oid)
                .ok_or_else(|| not_found("capture"))?
        } else {
            let row = CaptureRow {
                id: out.capture_id,
                objective_id: oid,
                tree: out.tree.clone(),
                target_commit: out.target.clone(),
                counts: jobs::counts(&out.files),
                retention_ref: format!("refs/lade/retain/capture-{}", out.capture_id),
                created_at: now_ms(),
            };
            sql(storage::insert_capture(self.store.conn(), &row))?;
            row
        };
        let mut changed_drafts = Vec::new();
        let st = self
            .objectives
            .get_mut(&oid)
            .ok_or_else(|| not_found("objective"))?;
        for (id, stale) in &out.stale {
            if st.draft_stale.insert(*id, *stale) != Some(*stale) {
                changed_drafts.push(*id);
            }
        }
        let fresh = !out.reused;
        let counts_changed = st.capture.as_ref().map(|c| c.counts) != Some(row.counts);
        st.capture = Some(row.clone());
        let drafts: Vec<_> = changed_drafts
            .iter()
            .filter_map(|id| st.draft(*id).map(|d| wire_draft(st, d)))
            .collect();
        for draft in drafts {
            self.emit(Event::DraftChanged(draft));
        }
        if fresh {
            self.emit(Event::WorktreeChanged(
                lade_protocol::client::WorktreeChanged {
                    objective_id: wire_id(oid.uuid()),
                    capture_id: wire_id(row.id.uuid()),
                },
            ));
        }
        if fresh || counts_changed || !changed_drafts.is_empty() {
            self.emit_objective(oid);
        }
        Ok(row)
    }

    fn capture_done(
        &mut self,
        oid: ObjectiveId,
        purpose: CapturePurpose,
        result: Result<CaptureOut, Rejection>,
    ) {
        let registered =
            result.and_then(|out| self.register_capture(oid, &out).map(|row| (out, row)));
        match purpose {
            CapturePurpose::Idle => {
                let rt = self.rt(oid);
                rt.capture_running = false;
                if std::mem::take(&mut rt.recapture) {
                    self.request_capture(oid);
                }
            }
            CapturePurpose::Changes(ctx) => {
                let reply =
                    registered.and_then(|(out, row)| self.working_change_list(oid, &out, &row));
                self.finish(ctx, reply);
            }
            CapturePurpose::Retry(ctx, request) => match registered {
                Ok(_) => {
                    let _ = self.dispatch(ctx, *request);
                }
                Err(rejection) => self.finish(ctx, Err(rejection)),
            },
            CapturePurpose::Submit(ctx, cmd) => match registered {
                Ok((out, _)) => self.submit_after_capture(ctx, &cmd, &out),
                Err(rejection) => self.finish(ctx, Err(rejection)),
            },
            CapturePurpose::DispatchCheck(input_id) => {
                let stale = registered.map_or(true, |(out, _)| out.stale.iter().any(|(_, s)| *s));
                self.dispatch_checked(oid, input_id, stale);
            }
        }
    }
}

fn input_reply(
    name: lade_protocol::client::RequestName,
    summary: lade_protocol::client::InputSummary,
) -> ReplyData {
    match name {
        lade_protocol::client::RequestName::FeedbackSubmit => ReplyData::FeedbackSubmit(summary),
        _ => ReplyData::MessageSend(summary),
    }
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
