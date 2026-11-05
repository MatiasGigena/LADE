//! Integration of an approved Candidate: the write-ahead journal entry, the squash onto the
//! Integration Target off the service thread, and the queue of integrations waiting on the
//! same target.

use std::collections::{HashMap, VecDeque};

use lade_core::DomainError;
use lade_core::domain::ids::{ApprovalId, GitId, ObjectiveId, Uuid, WorkspaceId};
use lade_core::domain::review::{IntegrationPlan, InvalidationReason};
use lade_protocol::client::{Rejection, ReplyData, ReviewIntegrateCommand};

use crate::adapters::git::{GitError, Repository, SquashOutcome, SquashRequest};
use crate::service::convert::{domain_rejection, git_rejection, not_found, parse_uuid};
use crate::service::state::ObjState;
use crate::service::{Completed, Flow, ReqCtx, Service};
use crate::storage::{self, JournalRow, JournalState, OperationState};
use crate::support::{new_uuid, now_ms, ts_now};

/// Integrations in flight, keyed by workspace and target branch: requests queued behind the
/// running one, and the last target commit seen per key.
#[derive(Default)]
pub(crate) struct Integrations {
    pub queued: HashMap<(WorkspaceId, String), VecDeque<(ReqCtx, ReviewIntegrateCommand)>>,
    pub targets: HashMap<(WorkspaceId, String), GitId>,
}

impl Service {
    fn integration_key(&self, oid: ObjectiveId) -> Option<(WorkspaceId, String)> {
        self.objectives.get(&oid).map(|st| {
            let record = st.objective().record();
            (
                record.workspace_id,
                record.integration_target.as_str().to_string(),
            )
        })
    }

    pub(crate) fn review_integrate(&mut self, ctx: ReqCtx, cmd: &ReviewIntegrateCommand) -> Flow {
        let running = self
            .objective_id(&cmd.objective_id)
            .ok()
            .and_then(|oid| Some((oid, self.integration_key(oid)?)))
            .filter(|(_, key)| self.integrations.queued.contains_key(key));
        if let Some((oid, key)) = running {
            return self.queue_integration(ctx, cmd, oid, &key);
        }
        let prepared = self.prepare_integration(cmd).and_then(|(oid, st, plan)| {
            let repo = self.ws_of(&st)?.repo.clone();
            Ok((oid, st, plan, repo))
        });
        let (oid, st, plan, repo) = match prepared {
            Ok(prepared) => prepared,
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                return Flow::Done;
            }
        };
        let journal = integration_journal(oid, &st, &plan);
        if let Err(rejection) = self.record_integration_intent(&ctx, &journal) {
            self.finish(ctx, Err(rejection));
            return Flow::Done;
        }
        crate::fault::point("integrate.intent");
        self.start_squash(oid, ctx, plan, journal, repo);
        Flow::Deferred
    }

    /// Queues `cmd` behind the integration already running on the same target; it is retried
    /// when that one ends.
    fn queue_integration(
        &mut self,
        ctx: ReqCtx,
        cmd: &ReviewIntegrateCommand,
        oid: ObjectiveId,
        key: &(WorkspaceId, String),
    ) -> Flow {
        if let Err(rejection) = self.check_idle(oid) {
            self.finish(ctx, Err(rejection));
            return Flow::Done;
        }
        self.rt(oid).busy = Some("integrate");
        if let Some(waiting) = self.integrations.queued.get_mut(key) {
            waiting.push_back((ctx, cmd.clone()));
        }
        self.emit_objective(oid);
        Flow::Deferred
    }

    /// Checks that the approval still authorizes integrating onto the target as it is now. A
    /// target that moved is recorded, invalidating the approval, before the request is refused.
    fn prepare_integration(
        &mut self,
        cmd: &ReviewIntegrateCommand,
    ) -> Result<(ObjectiveId, ObjState, IntegrationPlan), Rejection> {
        let oid = self.objective_id(&cmd.objective_id)?;
        self.check_idle(oid)?;
        let approval_id = ApprovalId::new(parse_uuid(&cmd.approval_id)?);
        let mut st = self.obj(oid)?.clone();
        let current = self
            .fresh_target(&st)
            .ok_or_else(|| not_found("integration target branch"))?;
        match st
            .review
            .check_integration(st.objective(), approval_id, &current)
        {
            Ok(plan) => Ok((oid, st, plan)),
            Err(DomainError::StaleTarget) => {
                st.review
                    .observe_target(&mut st.row.objective, &current, ts_now())
                    .map_err(domain_rejection)?;
                if st.review.valid_approval().is_some() {
                    st.review
                        .invalidate_approval(
                            &mut st.row.objective,
                            InvalidationReason::TargetMoved,
                            ts_now(),
                        )
                        .map_err(domain_rejection)?;
                }
                self.commit_obj(None, None, st)?;
                self.emit_objective(oid);
                Err(domain_rejection(DomainError::StaleTarget))
            }
            Err(e) => Err(domain_rejection(e)),
        }
    }

    /// Journals the integration intent, together with the request's pending operation, before
    /// the squash runs.
    fn record_integration_intent(
        &mut self,
        ctx: &ReqCtx,
        journal: &JournalRow,
    ) -> Result<(), Rejection> {
        let op = ctx.op.clone();
        let name = ctx.name;
        self.commit(None, None, |tx| {
            storage::insert_journal(tx, journal, now_ms())?;
            if let Some(op) = &op {
                storage::record_operation(
                    tx,
                    op.key(name),
                    OperationState::Pending,
                    None,
                    now_ms(),
                )?;
            }
            Ok(())
        })
    }

    fn start_squash(
        &mut self,
        oid: ObjectiveId,
        ctx: ReqCtx,
        plan: IntegrationPlan,
        journal: JournalRow,
        repo: Repository,
    ) {
        let request = SquashRequest {
            target_branch: journal.target_branch,
            expected_target: plan.expected_target.clone(),
            candidate_tree: plan.candidate_tree.clone(),
            message: journal.message,
            timestamp: Some(journal.commit_time),
        };
        self.rt(oid).busy = Some("integrate");
        if let Some(key) = self.integration_key(oid) {
            self.integrations.queued.entry(key).or_default();
        }
        let journal_id = journal.id;
        self.spawn(move || Completed::Integrate {
            oid,
            ctx,
            plan,
            journal_id,
            result: repo.integrate_squash(&request),
        });
    }

    pub(crate) fn integrate_done(
        &mut self,
        oid: ObjectiveId,
        mut ctx: ReqCtx,
        plan: &IntegrationPlan,
        journal_id: Uuid,
        result: Result<SquashOutcome, GitError>,
    ) {
        self.rt(oid).busy = None;
        let outcome = match result {
            Ok(squash) => self.integration_applied(oid, &mut ctx, plan, journal_id, &squash),
            Err(error) => Err(self.integration_refused(oid, journal_id, error)),
        };
        self.finish(ctx, outcome);
        self.emit_objective(oid);
        self.release_integration_target(oid);
    }

    fn integration_applied(
        &mut self,
        oid: ObjectiveId,
        ctx: &mut ReqCtx,
        plan: &IntegrationPlan,
        journal_id: Uuid,
        squash: &SquashOutcome,
    ) -> Result<ReplyData, Rejection> {
        crate::fault::point("integrate.applied");
        let mut st = self.obj(oid)?.clone();
        st.review
            .complete_integration(&mut st.row.objective, plan, ts_now())
            .map_err(domain_rejection)?;
        st.row.integration_conflict = false;
        let data = ReplyData::ReviewIntegrate(Box::new(self.fresh_detail(&st)));
        let now = now_ms();
        self.commit_obj_with(Some(ctx), Some(&data), st, |tx| {
            storage::update_journal(
                tx,
                journal_id,
                JournalState::Done,
                Some(&squash.commit),
                None,
                now,
            )
        })?;
        Ok(data)
    }

    /// Records a refused squash in the journal, and on the Objective a moved target or a
    /// checkout conflict, then rejects with the Git error.
    fn integration_refused(
        &mut self,
        oid: ObjectiveId,
        journal_id: Uuid,
        error: GitError,
    ) -> Rejection {
        let _ = storage::update_journal(
            self.store.conn(),
            journal_id,
            JournalState::Failed,
            None,
            Some(error.code()),
            now_ms(),
        );
        eprintln!(
            "laded: integration refused objective={oid} code={}",
            error.code()
        );
        if let Ok(mut st) = self.obj(oid).cloned() {
            match &error {
                GitError::TargetMoved { .. } => {
                    if let Some(current) = self.fresh_target(&st) {
                        let _ = st
                            .review
                            .observe_target(&mut st.row.objective, &current, ts_now());
                    }
                }
                GitError::CheckoutFailed { .. } => st.row.integration_conflict = true,
                _ => {}
            }
            let _ = self.commit_obj(None, None, st);
        }
        git_rejection(error)
    }

    /// Refreshes the Objectives that share `oid`'s target, whose state the integration may
    /// have changed, and retries the integrations queued behind it.
    fn release_integration_target(&mut self, oid: ObjectiveId) {
        let key = self.integration_key(oid);
        let siblings: Vec<ObjectiveId> = self
            .objectives
            .iter()
            .filter(|(other, st)| {
                let record = st.objective().record();
                **other != oid
                    && key.as_ref().is_some_and(|(workspace, target)| {
                        record.workspace_id == *workspace
                            && record.integration_target.as_str() == target
                    })
            })
            .map(|(id, _)| *id)
            .collect();
        for sibling in siblings {
            self.emit_objective(sibling);
        }
        let waiting = key
            .and_then(|key| self.integrations.queued.remove(&key))
            .unwrap_or_default();
        for (ctx, cmd) in waiting {
            if let Ok(next) = self.objective_id(&cmd.objective_id) {
                self.rt(next).busy = None;
            }
            let _ = self.review_integrate(ctx, &cmd);
        }
    }
}

/// The journal entry for integrating `plan`, with the exact squash message and time so restart
/// reconciliation can rebuild the commit.
fn integration_journal(oid: ObjectiveId, st: &ObjState, plan: &IntegrationPlan) -> JournalRow {
    let record = st.objective().record();
    JournalRow {
        id: new_uuid(),
        objective_id: oid,
        approval_id: plan.approval_id,
        candidate_id: plan.candidate_id,
        target_branch: record.integration_target.as_str().to_string(),
        expected_target: plan.expected_target.clone(),
        candidate_tree: plan.candidate_tree.clone(),
        state: JournalState::Intent,
        result_commit: None,
        message: format!(
            "{}\n\nIntegrated by LADE from objective {oid} (candidate {}).",
            record.title.as_str(),
            plan.candidate_id
        ),
        commit_time: now_ms() / 1000,
    }
}
