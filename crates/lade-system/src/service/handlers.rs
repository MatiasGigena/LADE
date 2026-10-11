//! Request routing, job-completion routing and the synchronous commands (workspace, objective
//! definition, intent, settings, drafts, direct messages) plus feedback submission.

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
    DraftDeleteCommand, DraftList, DraftSaveCommand, Empty, Event, FeedbackSubmitCommand,
    InputList, MessageSendCommand, ObjectiveCreateCommand, ObjectiveSelector, OperationGetQuery,
    OperationState, OperationStatus, Rejection, ReplyData, Request, SettingsUpdateCommand,
    SnapshotData, Start, UpdateIntentCommand, WorkspaceOpenCommand,
};
use lade_protocol::types::Id;

use crate::adapters::artifacts::ArtifactDigest;
use crate::adapters::git::Repository;
use crate::storage::{self, CaptureRow, ObjectiveRow, WorkspaceRow};

use super::convert::{
    conflict, core_reference, counter, domain_rejection, durability, from_wire_git, git_rejection,
    invalid, new_uuid, not_found, now_ms, parse_uuid, wire_id,
};
use super::jobs::{self, CaptureInput, CaptureOut};
use super::project::{core_mode, wire_draft, wire_input};
use super::state::{ObjState, Runtime, WsState};
use super::{Flow, JobOutput, ReqCtx, Service};

/// What a pool job's completion continues.
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
        journal_id: String,
    },
    Cleanup {
        oid: ObjectiveId,
        ctx: ReqCtx,
        confirm: bool,
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
}

/// Why a working capture was taken.
pub(crate) enum CapturePurpose {
    /// The worker went idle: refresh the working view and draft staleness.
    Idle,
    /// `changes.list` on the working basis.
    Changes(ReqCtx),
    /// Re-run a query once a capture exists.
    Retry(ReqCtx, Box<Request>),
    /// `feedback.submit`: stale check of every batch item against current content.
    Submit(ReqCtx, Box<FeedbackSubmitCommand>),
    /// Dispatch-time revalidation of a queued feedback input.
    DispatchCheck(InputId),
}

const SNAPSHOT_OBJECTIVES: usize = 400;
const SNAPSHOT_BRANCHES: usize = 64;
const REPLY_BRANCHES: usize = 256;

pub(crate) fn sql<T>(result: rusqlite::Result<T>) -> Result<T, Rejection> {
    result.map_err(|_| durability("state could not be recorded"))
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
            Some(what) => Err(conflict(&format!(
                "another operation is in progress: {what}"
            ))),
            None => Ok(()),
        }
    }

    /// Writes the Objective row and its review records.
    pub(crate) fn save_tx(tx: &rusqlite::Connection, st: &ObjState) -> rusqlite::Result<()> {
        storage::save_objective(tx, &st.row)?;
        storage::save_review(tx, &st.review)
    }

    /// Commits `st` (objective + review) with the request outcome, then swaps it in.
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

    // -----------------------------------------------------------------------------------------
    // Routing.

    pub(crate) fn dispatch(&mut self, mut ctx: ReqCtx, request: Request) -> Flow {
        let result = match request {
            Request::Snapshot(_) => Ok(self.snapshot()),
            Request::ObjectiveGet(ObjectiveSelector { objective_id }) => {
                self.objective_id(&objective_id).map(|oid| {
                    let st = self.objectives[&oid].clone();
                    ReplyData::ObjectiveGet(Box::new(self.detail_of(&st)))
                })
            }
            Request::DraftsList(ObjectiveSelector { objective_id }) => {
                self.objective_id(&objective_id).map(|oid| {
                    let st = &self.objectives[&oid];
                    let drafts = st
                        .drafts
                        .iter()
                        .rev()
                        .take(512)
                        .rev()
                        .map(|d| wire_draft(st, d))
                        .collect();
                    ReplyData::DraftsList(DraftList { drafts })
                })
            }
            Request::InputsList(ObjectiveSelector { objective_id }) => {
                self.objective_id(&objective_id).map(|oid| {
                    let st = &self.objectives[&oid];
                    let inputs = st
                        .inputs
                        .iter()
                        .rev()
                        .take(512)
                        .rev()
                        .map(|(_, i)| wire_input(i))
                        .collect();
                    ReplyData::InputsList(InputList { inputs })
                })
            }
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
            // IMP-011 pi compatibility (service/compat.rs).
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
            ) => self.integrate_done(oid, ctx, &plan, &journal_id, result),
            (Pending::Cleanup { oid, ctx, confirm }, JobOutput::Unit(result)) => {
                self.cleanup_done(oid, ctx, confirm, result);
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

    // -----------------------------------------------------------------------------------------
    // Queries.

    fn snapshot(&self) -> ReplyData {
        let workspaces = self
            .workspaces
            .values()
            .take(64)
            .map(|ws| self.wire_workspace(ws, SNAPSHOT_BRANCHES))
            .collect();
        let mut states: Vec<&ObjState> = self.objectives.values().collect();
        states.sort_by_key(|st| std::cmp::Reverse(st.objective().record().updated_at));
        let objectives = states
            .into_iter()
            .take(SNAPSHOT_OBJECTIVES)
            .map(|st| self.summary(st))
            .collect();
        ReplyData::Snapshot(SnapshotData {
            service_epoch: self.epoch.clone(),
            sequence: counter(self.sequence),
            workspaces,
            objectives,
            settings: self.settings.clone(),
        })
    }

    fn operation_get(&self, query: &OperationGetQuery) -> Result<ReplyData, Rejection> {
        let row = sql(storage::load_operation(
            self.store.conn(),
            query.operation_id.as_str(),
        ))?;
        let status = match row {
            None => OperationStatus {
                state: OperationState::Unknown,
                outcome: None,
            },
            Some(row) => OperationStatus {
                state: match row.state.as_str() {
                    "pending" => OperationState::Pending,
                    "uncertain" => OperationState::Uncertain,
                    _ => OperationState::Completed,
                },
                outcome: row.outcome,
            },
        };
        Ok(ReplyData::OperationGet(status))
    }

    // -----------------------------------------------------------------------------------------
    // Workspaces and objectives.

    fn workspace_open(&mut self, cmd: &WorkspaceOpenCommand) -> Result<ReplyData, Rejection> {
        let path = PathBuf::from(cmd.path.to_os_string());
        if !path.is_absolute() {
            return Err(invalid("workspace path must be absolute"));
        }
        let repo = Repository::open(&path).map_err(|e| git_rejection(&e))?;
        let top = repo.work_tree().to_path_buf();
        if let Some(existing) = self.workspaces.values().find(|ws| ws.path == top) {
            return Ok(ReplyData::WorkspaceOpen(
                self.wire_workspace(existing, REPLY_BRANCHES),
            ));
        }
        let default_branch = repo.head_branch().ok().flatten();
        let name = top.file_name().map_or_else(
            || "workspace".to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let row = WorkspaceRow {
            id: WorkspaceId::new(new_uuid()),
            path: top.clone(),
            name: name.clone(),
            default_branch: default_branch.clone(),
            opened_at: now_ms(),
        };
        // Recorded with the generic outcome journal in `finish`.
        sql(storage::insert_workspace(self.store.conn(), &row))?;
        let ws = WsState {
            id: row.id,
            repo,
            path: top,
            name,
            default_branch,
        };
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
        let domain = |e: lade_core::DomainError| domain_rejection(&e);
        let title = Title::new(&cmd.title).map_err(domain)?;
        let intent = Intent::new(
            &cmd.intent.outcome,
            &cmd.intent.constraints,
            &cmd.intent.acceptance,
        )
        .map_err(domain)?;
        let target = BranchName::new(&cmd.integration_target).map_err(domain)?;
        let target_commit = jobs::target_commit(&ws.repo, target.as_str())?;
        let start_commit = match &cmd.start {
            Start::Target => target_commit,
            Start::Commit { commit } => {
                let requested = from_wire_git(commit)?;
                let resolved = ws
                    .repo
                    .resolve_commit(requested.hex())
                    .map_err(|e| git_rejection(&e))?;
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
            },
            review: Review::new(oid),
            drafts: Vec::new(),
            draft_stale: HashMap::new(),
            inputs: Vec::new(),
            capture: None,
            tails: HashMap::new(),
        };
        let data = ReplyData::ObjectiveCreate(Box::new(self.detail_of(&st)));
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
        let domain = |e: lade_core::DomainError| domain_rejection(&e);
        let intent = Intent::new(
            &cmd.intent.outcome,
            &cmd.intent.constraints,
            &cmd.intent.acceptance,
        )
        .map_err(domain)?;
        let mut st = self.obj(oid)?.clone();
        st.objective_mut()
            .update_intent(
                Revision::new(cmd.expected_intent_revision.value()),
                intent,
                lade_core::domain::ids::Timestamp::from_unix_ms(now_ms()),
            )
            .map_err(domain)?;
        let data = ReplyData::ObjectiveUpdateIntent(Box::new(self.detail_of(&st)));
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
                .map_err(|e| domain_rejection(&e))?;
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
        self.commit(Some(ctx), Some(&data), |tx| {
            storage::save_settings(tx, &settings)
        })?;
        self.settings = settings;
        Ok(data)
    }

    // -----------------------------------------------------------------------------------------
    // Drafts.

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
            return Err(not_found("referenced artifact (read the file first)"));
        }
        let range = reference.byte_range();
        let page = self
            .artifacts
            .read_page(
                &digest,
                range.start(),
                usize::try_from(range.byte_len()).unwrap_or(usize::MAX),
            )
            .map_err(|_| durability("referenced artifact unreadable"))?;
        if page.offset != range.start() || page.data != reference.selected_text().as_bytes() {
            return Err(invalid("selected text does not match the referenced bytes"));
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
        .map_err(|e| domain_rejection(&e))?;
        let mut st = st.clone();
        match st.drafts.iter_mut().find(|d| d.id() == draft_id) {
            Some(slot) => *slot = draft.clone(),
            None => st.drafts.push(draft.clone()),
        }
        st.draft_stale.insert(draft_id, false);
        let wire = wire_draft(&st, &draft);
        let data = ReplyData::DraftSave(wire.clone());
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
            .map_err(|e| domain_rejection(&e))?;
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

    // -----------------------------------------------------------------------------------------
    // Direct messages and feedback.

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
                    domain_rejection(&lade_core::DomainError::ExecutionUnavailable)
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

    /// Persists a freshly accepted input (and submitted drafts) with the outcome, then replies and
    /// dispatches. `checked` means references were just validated against current content.
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
            .map_err(|e| domain_rejection(&e))?;
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
                .map_err(|e| domain_rejection(&e))?;
            if st.objective().lifecycle() != lade_core::domain::objectives::Lifecycle::Active {
                return Err(domain_rejection(&lade_core::DomainError::InvalidLifecycle(
                    "input requires an active objective; return a frozen candidate for revision first",
                )));
            }
            let mut drafts = Vec::new();
            for entry in &cmd.drafts {
                let id = DraftId::new(parse_uuid(&entry.draft_id)?);
                let draft = st.draft(id).ok_or_else(|| not_found("draft"))?;
                if draft.revision().value() != entry.revision.value() {
                    return Err(conflict("draft revision mismatch"));
                }
                if !draft.is_unsent() {
                    return Err(conflict("draft already submitted"));
                }
                drafts.push((id, draft.record().reference.clone()));
            }
            let worktree =
                st.row.worktree.clone().ok_or_else(|| {
                    domain_rejection(&lade_core::DomainError::ExecutionUnavailable)
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
            .map_err(|e| domain_rejection(&e))?;
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

    // -----------------------------------------------------------------------------------------
    // Captures.

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
            latest: st.capture.as_ref().map(|c| (c.id, c.tree.clone())),
            drafts,
        })
    }

    /// Unsent drafts whose staleness a capture should recompute.
    pub(crate) fn unsent_references(
        st: &ObjState,
    ) -> Vec<(DraftId, lade_core::domain::inspection::Reference)> {
        st.drafts
            .iter()
            .filter(|d| d.is_unsent())
            .map(|d| (d.id(), d.record().reference.clone()))
            .collect()
    }

    /// Starts an idle capture unless one is already running (then one more follows).
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

    /// Records a capture (unless reused), updates draft staleness and announces the change.
    pub(crate) fn register_capture(
        &mut self,
        oid: ObjectiveId,
        out: &CaptureOut,
    ) -> Result<CaptureRow, Rejection> {
        let st = self.obj(oid)?;
        let row = match (&st.capture, out.reused) {
            (Some(existing), true) => existing.clone(),
            _ => {
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
            }
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

/// Reply data for an accepted input (`message.send` or `feedback.submit`).
fn input_reply(
    name: lade_protocol::client::RequestName,
    summary: lade_protocol::client::InputSummary,
) -> ReplyData {
    match name {
        lade_protocol::client::RequestName::FeedbackSubmit => ReplyData::FeedbackSubmit(summary),
        _ => ReplyData::MessageSend(summary),
    }
}
