//! The end of an Objective: completing it without changes, discarding it, cleaning up its
//! worktree and branch, and deleting its history.

use lade_core::DomainError;
use lade_core::domain::ids::ObjectiveId;
use lade_core::domain::objectives::Lifecycle;
use lade_protocol::client::{
    ObjectiveCleanupCommand, ObjectiveDeleteHistoryCommand, Rejection, ReplyData,
};
use lade_protocol::types::Id;

use crate::adapters::git;
use crate::fault;
use crate::service::convert::{domain_rejection, durability_failure, invalid};
use crate::service::handlers::sql;
use crate::service::jobs;
use crate::service::projection::actions::delete_history_blocker;
use crate::service::state::Busy;
use crate::service::{Completed, Flow, ReqCtx, Service};
use crate::storage;
use crate::support::ts_now;

impl Service {
    pub(crate) fn objective_complete(&mut self, ctx: ReqCtx, id: &Id) -> Flow {
        let prepared = self.prepare_complete(id);
        self.start_deferred(ctx, Busy::Complete, prepared)
    }

    /// Checks that the Objective can complete and returns the job that asks Git whether its
    /// change set is empty.
    fn prepare_complete(
        &self,
        id: &Id,
    ) -> Result<
        (
            ObjectiveId,
            impl FnOnce(ReqCtx) -> Completed + Send + 'static,
        ),
        Rejection,
    > {
        let oid = self.objective_id(id)?;
        self.check_idle(oid)?;
        let st = self.obj(oid)?;
        if !matches!(
            st.row.objective.lifecycle(),
            Lifecycle::Defined | Lifecycle::Active | Lifecycle::Frozen
        ) {
            return Err(domain_rejection(DomainError::InvalidLifecycle(
                "objective is approved or closed",
            )));
        }
        if st.row.objective.execution().state().is_live() {
            return Err(domain_rejection(DomainError::InvalidLifecycle(
                "stop execution first",
            )));
        }
        let repo = self.ws_of(st)?.repo.clone();
        let record = st.row.objective.record();
        let worktree = st.row.worktree.clone();
        let target = record.integration_target.as_str().to_string();
        let start = record.start_commit.clone();
        let scratch = self.paths.tmp.clone();
        Ok((oid, move |ctx| Completed::Complete {
            oid,
            ctx,
            result: jobs::change_set_empty(&repo, worktree.as_deref(), &scratch, &target, &start),
        }))
    }

    pub(crate) fn complete_done(
        &mut self,
        oid: ObjectiveId,
        mut ctx: ReqCtx,
        result: Result<bool, Rejection>,
    ) {
        self.rt(oid).busy = None;
        let outcome = (|| {
            let empty = result?;
            let mut st = self.obj(oid)?.clone();
            st.review
                .complete_without_changes(&mut st.row.objective, empty, ts_now())
                .map_err(domain_rejection)?;
            let data = ReplyData::ObjectiveComplete(Box::new(self.fresh_detail(&st)));
            self.commit_obj(Some(&mut ctx), Some(&data), st)?;
            Ok(data)
        })();
        self.finish(ctx, outcome);
        self.emit_objective(oid);
    }

    pub(crate) fn objective_discard(
        &mut self,
        ctx: &mut ReqCtx,
        id: &Id,
    ) -> Result<ReplyData, Rejection> {
        let oid = self.objective_id(id)?;
        self.check_idle(oid)?;
        let mut st = self.obj(oid)?.clone();
        st.review
            .discard(&mut st.row.objective, ts_now())
            .map_err(domain_rejection)?;
        let data = ReplyData::ObjectiveDiscard(Box::new(self.fresh_detail(&st)));
        self.commit_obj(Some(ctx), Some(&data), st)?;
        self.emit_objective(oid);
        Ok(data)
    }

    pub(crate) fn objective_cleanup(&mut self, ctx: ReqCtx, cmd: &ObjectiveCleanupCommand) -> Flow {
        let prepared = self.prepare_cleanup(cmd);
        self.start_deferred(ctx, Busy::Cleanup, prepared)
    }

    /// Checks that the Objective can be cleaned up and returns the job that removes its
    /// worktree and branch.
    fn prepare_cleanup(
        &self,
        cmd: &ObjectiveCleanupCommand,
    ) -> Result<
        (
            ObjectiveId,
            impl FnOnce(ReqCtx) -> Completed + Send + 'static,
        ),
        Rejection,
    > {
        let oid = self.objective_id(&cmd.objective_id)?;
        self.check_idle(oid)?;
        let st = self.obj(oid)?;
        if st.row.objective.execution().state().is_live() {
            return Err(domain_rejection(DomainError::InvalidLifecycle(
                "stop execution first",
            )));
        }
        let mut probe = st.clone();
        probe
            .row
            .objective
            .cleanup(st.unsent_drafts(), cmd.confirm_data_loss, ts_now())
            .map_err(domain_rejection)?;
        let repo = self.ws_of(st)?.repo.clone();
        let worktree = st.row.worktree.clone();
        let branch = st.row.git_branch.clone();
        let confirm = cmd.confirm_data_loss;
        let scratch = self.paths.tmp.clone();
        Ok((oid, move |ctx| Completed::Cleanup {
            oid,
            ctx,
            confirm,
            result: jobs::cleanup(
                &repo,
                worktree.as_deref(),
                branch.as_deref(),
                &scratch,
                confirm,
            ),
        }))
    }

    pub(crate) fn objective_delete_history(
        &mut self,
        ctx: ReqCtx,
        cmd: &ObjectiveDeleteHistoryCommand,
    ) -> Flow {
        let prepared = self.prepare_delete_history(cmd);
        self.start_deferred(ctx, Busy::DeleteHistory, prepared)
    }

    /// Checks that the Objective's history can be deleted and returns the job that deletes its
    /// retained refs.
    fn prepare_delete_history(
        &self,
        cmd: &ObjectiveDeleteHistoryCommand,
    ) -> Result<
        (
            ObjectiveId,
            impl FnOnce(ReqCtx) -> Completed + Send + 'static,
        ),
        Rejection,
    > {
        let oid = self.objective_id(&cmd.objective_id)?;
        self.check_idle(oid)?;
        let st = self.obj(oid)?;
        if let Some(reason) = delete_history_blocker(st) {
            return Err(domain_rejection(DomainError::InvalidLifecycle(reason)));
        }
        if !cmd.confirm_history_loss {
            return Err(invalid(
                "deleting history needs explicit confirmation (confirm_history_loss)",
            ));
        }
        let mut refs = sql(storage::capture_retention_refs(self.store.conn(), oid))?;
        for candidate in st.review.candidates() {
            let base = format!("{}candidate-{}", git::RETENTION_PREFIX, candidate.id);
            refs.push(format!("{base}-tree"));
            refs.push(base);
        }
        if let Some(branch) = &st.row.git_branch {
            refs.push(format!("refs/heads/{branch}"));
        }
        let repo = self.ws_of(st)?.repo.clone();
        Ok((oid, move |ctx| Completed::DeleteHistory {
            oid,
            ctx,
            result: jobs::delete_refs(&repo, &refs),
        }))
    }

    pub(crate) fn delete_history_done(
        &mut self,
        oid: ObjectiveId,
        mut ctx: ReqCtx,
        result: Result<(), Rejection>,
    ) {
        self.rt(oid).busy = None;
        let outcome = (|| {
            result?;
            let mut st = self.obj(oid)?.clone();
            st.row.history_deleted = true;
            let data = ReplyData::ObjectiveDeleteHistory(Box::new(self.fresh_detail(&st)));
            self.commit_obj(Some(&mut ctx), Some(&data), st)
                .map_err(|_| {
                    durability_failure("history deleted but not recorded; query the objective")
                })?;
            Ok(data)
        })();
        self.finish(ctx, outcome);
        self.emit_objective(oid);
    }

    pub(crate) fn cleanup_done(
        &mut self,
        oid: ObjectiveId,
        mut ctx: ReqCtx,
        confirm: bool,
        result: Result<(), Rejection>,
    ) {
        self.rt(oid).busy = None;
        if result.is_ok() {
            fault::point("cleanup.removed");
        }
        let outcome = (|| {
            result?;
            let mut st = self.obj(oid)?.clone();
            let unsent = st.unsent_drafts();
            st.row
                .objective
                .cleanup(unsent, confirm, ts_now())
                .map_err(domain_rejection)?;
            st.row.worktree = None;
            let data = ReplyData::ObjectiveCleanup(Box::new(self.fresh_detail(&st)));
            self.commit_obj(Some(&mut ctx), Some(&data), st)
                .map_err(|_| {
                    durability_failure("cleanup done but not recorded; query the objective")
                })?;
            Ok(data)
        })();
        self.finish(ctx, outcome);
        self.emit_objective(oid);
    }
}
