//! Request dispatch and the helpers every request handler shares: looking up Objectives and
//! workspaces, the busy check, committing an Objective, snapshots and operation status.

use lade_core::domain::ids::ObjectiveId;
use lade_protocol::client::{
    Empty, ObjectiveSelector, OperationGetQuery, OperationState, OperationStatus, Rejection,
    ReplyData, Request, Settings, SnapshotData,
};
use lade_protocol::types::{Counter, Id};
use rusqlite::Transaction;

use crate::storage;

use super::convert::{conflict, durability, not_found, parse_uuid};
use super::state::{ObjState, Runtime, WsState};
use super::{Completed, Flow, ReqCtx, Service};

pub(super) const SNAPSHOT_OBJECTIVES: usize = 400;
const SNAPSHOT_BRANCHES: usize = 64;

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
            Some(_) => Err(conflict(super::projection::actions::BUSY)),
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
        self.commit_obj_with(ctx, outcome, st, |_| Ok(()))
    }

    /// Saves `st` together with the rows `extra` writes in one transaction, then replaces the
    /// in-memory Objective, so the map changes only once the commit succeeds.
    pub(crate) fn commit_obj_with(
        &mut self,
        ctx: Option<&mut ReqCtx>,
        outcome: Option<&ReplyData>,
        st: ObjState,
        extra: impl FnOnce(&Transaction<'_>) -> rusqlite::Result<()>,
    ) -> Result<(), Rejection> {
        self.commit(ctx, outcome, |tx| {
            Self::save_tx(tx, &st)?;
            extra(tx)
        })?;
        self.objectives.insert(st.objective().id(), st);
        Ok(())
    }

    pub(crate) fn dispatch(&mut self, mut ctx: ReqCtx, request: Request) -> Flow {
        let result = match request {
            Request::Snapshot(_) => Ok(self.snapshot()),
            Request::ObjectiveGet(ObjectiveSelector { objective_id }) => {
                self.objective_id(&objective_id).map(|oid| {
                    let st = self.objectives[&oid].clone();
                    ReplyData::ObjectiveGet(Box::new(self.fresh_detail(&st)))
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
            Request::ObjectiveUpdateIntent(cmd) => self.objective_update_intent(&mut ctx, &cmd),
            Request::SettingsUpdate(cmd) => self.settings_update(&mut ctx, cmd),
            Request::DraftSave(cmd) => self.draft_save(&mut ctx, &cmd),
            Request::DraftDelete(cmd) => self.draft_delete(&mut ctx, &cmd),
            Request::MessageSend(cmd) => return self.message_send(ctx, &cmd),
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

    pub(crate) fn on_job(&mut self, completed: Completed) {
        match completed {
            Completed::Launch {
                oid,
                generation,
                out,
            } => self.launch_done(oid, generation, out),
            Completed::Storage { usage } => self.storage_measured(usage),
            Completed::Capture {
                oid,
                purpose,
                result,
            } => self.capture_done(oid, purpose, result),
            Completed::Freeze {
                oid,
                ctx,
                candidate_id,
                result,
            } => self.freeze_done(oid, ctx, candidate_id, result),
            Completed::Refresh {
                oid,
                ctx,
                candidate_id,
                new_id,
                result,
            } => self.refresh_done(oid, ctx, candidate_id, new_id, result),
            Completed::Verify {
                oid,
                verification_id,
                out,
            } => self.verify_done(oid, verification_id, &out),
            Completed::Integrate {
                oid,
                ctx,
                plan,
                journal_id,
                result,
            } => self.integrate_done(oid, ctx, &plan, journal_id, result),
            Completed::Cleanup {
                oid,
                ctx,
                confirm,
                result,
            } => self.cleanup_done(oid, ctx, confirm, result),
            Completed::DeleteHistory { oid, ctx, result } => {
                self.delete_history_done(oid, ctx, result);
            }
            Completed::Complete { oid, ctx, result } => self.complete_done(oid, ctx, result),
            Completed::Query { ctx, result } => self.finish(ctx, result),
            Completed::Catalog { result } => self.catalog_done(result),
            Completed::ProfileImport { ctx, result } => self.profile_import_done(ctx, result),
            Completed::Reload { oid, ctx, result } => self.reload_snapshot_done(oid, ctx, result),
            Completed::TerminalOpen {
                oid,
                ctx,
                terminal_id,
                writable,
                size,
                result,
            } => self.terminal_open_done(oid, ctx, (terminal_id, writable, size), result),
            Completed::Done => {}
        }
    }

    fn snapshot(&self) -> ReplyData {
        ReplyData::Snapshot(Self::budget_snapshot(self.snapshot_data(
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
            .map(|ws| Self::wire_workspace(ws, Self::workspace_branches(ws, max_branches)))
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
}

#[cfg(test)]
mod tests {
    use lade_protocol::client::{Empty, ServiceStopCommand, StopReason};

    use super::*;
    use crate::service::test_harness::Harness;

    #[test]
    fn snapshots_list_the_most_recently_updated_objectives_first() {
        let mut harness = Harness::new("handlers-snapshot");
        let (_, first) = harness.create_objective("First");
        let (_, second) = harness.create_objective("Second");
        let mut st = harness.service.objectives[&first].clone();
        st.row.objective = {
            let mut record = st.row.objective.clone().into_record();
            record.updated_at = lade_core::domain::ids::Timestamp::from_unix_ms(i64::MAX);
            lade_core::domain::objectives::Objective::restore(record).expect("objective")
        };
        harness.service.objectives.insert(first, st);
        let Ok(ReplyData::Snapshot(snapshot)) = harness.call(Request::Snapshot(Empty {})) else {
            panic!("snapshot");
        };
        assert_eq!(snapshot.workspaces.len(), 1);
        let order: Vec<_> = snapshot
            .objectives
            .iter()
            .map(|o| {
                harness
                    .service
                    .objective_id(&o.objective_id)
                    .expect("known")
            })
            .collect();
        assert_eq!(order, [first, second]);
        assert_eq!(snapshot.service_epoch, harness.service.epoch);
    }

    #[test]
    fn lookups_reject_unknown_and_malformed_objectives() {
        let mut harness = Harness::new("handlers-lookup");
        let unknown = harness
            .call(Request::ObjectiveGet(ObjectiveSelector {
                objective_id: crate::service::convert::wire_id(crate::support::new_uuid()),
            }))
            .expect_err("unknown");
        assert_eq!(unknown.code, lade_protocol::client::ErrorCode::NotFound);
        let (_, oid) = harness.create_objective("Busy");
        assert!(harness.service.check_idle(oid).is_ok());
        harness.service.rt(oid).busy = Some("freeze");
        assert_eq!(
            harness.service.check_idle(oid).expect_err("busy").detail,
            super::super::projection::actions::BUSY
        );
    }

    #[test]
    fn operation_status_reports_unknown_pending_and_completed_operations() {
        let mut harness = Harness::new("handlers-operation");
        let get = |id: &Id| {
            Request::OperationGet(OperationGetQuery {
                operation_id: id.clone(),
            })
        };
        let unknown = Id::from_random_bytes([1; 16]);
        let Ok(ReplyData::OperationGet(status)) = harness.call(get(&unknown)) else {
            panic!("operation");
        };
        assert_eq!(status.state, OperationState::Unknown);
        assert!(status.outcome.is_none());

        let ctx = Harness::ctx(lade_protocol::client::RequestName::ServiceStop);
        let op = ctx
            .op
            .clone()
            .expect("mutating requests carry an operation");
        harness.service.dispatch(
            ctx,
            Request::ServiceStop(ServiceStopCommand {
                reason: StopReason::Exit,
            }),
        );
        assert!(
            harness.service.stopping,
            "service.stop asks the loop to end"
        );
        let Ok(ReplyData::OperationGet(status)) = harness.call(get(&op.id)) else {
            panic!("operation");
        };
        assert_eq!(status.state, OperationState::Completed);
        assert!(matches!(
            status.outcome,
            Some(lade_protocol::client::Outcome::Ok(ref data)) if matches!(**data, ReplyData::ServiceStop(_))
        ));
    }

    #[test]
    fn sql_failures_reject_as_durability_failures() {
        let failure = sql::<()>(Err(rusqlite::Error::InvalidQuery)).expect_err("failure");
        assert_eq!(
            failure.code,
            lade_protocol::client::ErrorCode::DurabilityFailure
        );
        assert_eq!(sql(Ok(3)).expect("ok"), 3);
    }
}
