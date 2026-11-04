use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use lade_core::domain::execution::ExecutionState;
use lade_core::domain::ids::{
    ApprovalId, CandidateId, GitId, ObjectiveId, Timestamp, VerificationId, WorkspaceId,
};
use lade_core::domain::inspection::ArtifactDigest;
use lade_core::domain::objectives::Lifecycle;
use lade_core::domain::review::{
    ApproveCandidate, IntegrationPlan, NewCandidate, NewVerification, RefreshOutcome, RefreshResult,
};
use lade_protocol::client::{
    CandidateCommand, ErrorCode, Event, NoticeCode, NoticeLevel, ObjectiveCleanupCommand,
    ObjectiveHistoryDeleteCommand, Rejection, ReplyData, ReviewApproveCommand,
    ReviewIntegrateCommand, ReviewVerifyCommand, SafeAction,
};
use lade_protocol::types::Id;

use crate::adapters::git::{self, GitError, SquashRequest};
use crate::storage::{self, JournalRow, JournalState, OperationState};

use super::convert::{
    adapter_git, conflict, domain_rejection, durability, from_wire_git, git_rejection, invalid,
    not_found, parse_uuid,
};
use super::convert::{wire_approval, wire_candidate, wire_verification};
use super::handlers::sql;
use super::jobs::{self, FreezeInput, RefreshInput, VerifyInput};
use super::project::{delete_history_blocker, freeze_blocker};
use super::{Completed, Flow, ReqCtx, Service};
use crate::support::{new_uuid, now_ms};

fn ts_now() -> Timestamp {
    Timestamp::from_unix_ms(now_ms())
}

/// Integrations in flight, keyed by workspace and target branch: requests queued behind the
/// running one, and the last target commit seen per key.
#[derive(Default)]
pub(crate) struct Integrations {
    pub queued: HashMap<(WorkspaceId, String), VecDeque<(ReqCtx, ReviewIntegrateCommand)>>,
    pub targets: HashMap<(WorkspaceId, String), GitId>,
}

impl Service {
    fn candidate_id(id: &Id) -> Result<CandidateId, Rejection> {
        Ok(CandidateId::new(parse_uuid(id)?))
    }

    pub(crate) fn review_freeze(&mut self, mut ctx: ReqCtx, id: &Id) -> Flow {
        let prepared = (|| {
            let oid = self.objective_id(id)?;
            self.check_idle(oid)?;
            if let Some(reason) = freeze_blocker(self.obj(oid)?) {
                return Err(domain_rejection(lade_core::DomainError::InvalidLifecycle(
                    reason,
                )));
            }
            Ok(oid)
        })();
        let oid = match prepared {
            Ok(oid) => oid,
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                return Flow::Done;
            }
        };
        self.rt(oid).busy = Some("freeze");
        self.exclude_terminal_writers(oid);
        let live = self.objectives[&oid]
            .objective()
            .execution()
            .state()
            .is_live();
        if !live {
            self.start_freeze(oid, ctx);
            return Flow::Deferred;
        }
        let mut st = self.objectives[&oid].clone();
        if st.objective().execution().state() != &ExecutionState::Stopping
            && let Err(e) = st.objective_mut().request_stop(ts_now())
        {
            self.rt(oid).busy = None;
            self.finish(ctx, Err(domain_rejection(e)));
            return Flow::Done;
        }
        if let Err(rejection) = self.commit_obj(None, None, st) {
            self.rt(oid).busy = None;
            self.finish(ctx, Err(rejection));
            return Flow::Done;
        }
        ctx.recorded = false;
        self.rt(oid).waiting_freeze = Some((ctx, Instant::now()));
        self.stop_worker(oid);
        self.emit_objective(oid);
        Flow::Deferred
    }

    pub(crate) fn continue_freeze(&mut self, oid: ObjectiveId) {
        let live = self
            .objectives
            .get(&oid)
            .is_some_and(|st| st.objective().execution().state().is_live());
        if live {
            return;
        }
        if let Some((ctx, _)) = self.rt(oid).waiting_freeze.take() {
            self.start_freeze(oid, ctx);
        }
    }

    fn start_freeze(&mut self, oid: ObjectiveId, ctx: ReqCtx) {
        let input = (|| {
            let st = self.obj(oid)?;
            let ws = self.ws_of(st)?;
            let record = st.objective().record();
            let candidate_id = CandidateId::new(new_uuid());
            let ordinal = st.review.latest_candidate().map_or(1, |c| c.ordinal + 1);
            Ok((
                candidate_id,
                FreezeInput {
                    repo: ws.repo.clone(),
                    artifacts: self.artifacts.clone(),
                    worktree: st
                        .row
                        .worktree
                        .clone()
                        .ok_or_else(|| invalid("no agent work to freeze yet"))?,
                    scratch: self.paths.tmp.clone(),
                    target_branch: record.integration_target.as_str().to_string(),
                    start_commit: record.start_commit.clone(),
                    git_branch: st
                        .row
                        .git_branch
                        .clone()
                        .unwrap_or_else(|| record.branch.as_str().to_string()),
                    candidate_ref: format!("candidate-{candidate_id}"),
                    message: format!(
                        "LADE candidate {ordinal}: {}\n\nObjective {oid}",
                        record.title.as_str()
                    ),
                },
            ))
        })();
        match input {
            Ok((candidate_id, input)) => self.spawn(move || Completed::Freeze {
                oid,
                ctx,
                candidate_id,
                result: jobs::freeze(&input),
            }),
            Err(rejection) => {
                self.rt(oid).busy = None;
                self.finish(ctx, Err(rejection));
                self.emit_objective(oid);
            }
        }
    }

    pub(crate) fn freeze_done(
        &mut self,
        oid: ObjectiveId,
        mut ctx: ReqCtx,
        candidate_id: CandidateId,
        result: Result<jobs::FreezeOut, Rejection>,
    ) {
        self.rt(oid).busy = None;
        let outcome = (|| {
            let out = result?;
            let mut st = self.obj(oid)?.clone();
            if st.objective().execution().state().is_live() {
                return Err(conflict(
                    "a writer restarted during freeze; nothing was frozen",
                ));
            }
            let now = ts_now();
            st.review
                .freeze(
                    &mut st.row.objective,
                    NewCandidate {
                        id: candidate_id,
                        commit: out.commit,
                        tree: out.tree,
                        base_target_commit: out.target,
                        manifest_digest: ArtifactDigest::parse(&out.manifest_digest)
                            .map_err(domain_rejection)?,
                        changes: out.counts,
                    },
                    now,
                )
                .map_err(domain_rejection)?;
            if out.changed {
                st.review
                    .invalidate_candidate(&mut st.row.objective, now)
                    .map_err(domain_rejection)?;
            }
            st.row.integration_conflict = false;
            let candidate = st
                .review
                .candidate(candidate_id)
                .ok_or_else(|| not_found("candidate"))?;
            let data = ReplyData::ReviewFreeze(wire_candidate(candidate));
            self.commit_obj(Some(&mut ctx), Some(&data), st)?;
            crate::fault::point("freeze.recorded");
            Ok((data, out.changed))
        })();
        match outcome {
            Ok((data, changed)) => {
                self.finish(ctx, Ok(data));
                if changed {
                    self.notice(
                        NoticeLevel::Warning,
                        NoticeCode::CandidateInvalidated,
                        "the worktree changed while freezing; the candidate lost review authority",
                    );
                }
            }
            Err(rejection) => self.finish(ctx, Err(rejection)),
        }
        self.emit_objective(oid);
    }

    pub(crate) fn review_return(
        &mut self,
        ctx: &mut ReqCtx,
        cmd: &CandidateCommand,
    ) -> Result<ReplyData, Rejection> {
        let oid = self.objective_id(&cmd.objective_id)?;
        self.check_idle(oid)?;
        let candidate = Self::candidate_id(&cmd.candidate_id)?;
        let mut st = self.obj(oid)?.clone();
        st.review
            .return_for_revision(&mut st.row.objective, candidate, ts_now())
            .map_err(domain_rejection)?;
        let data = ReplyData::ReviewReturn(Box::new(self.detail_of(&st)));
        self.commit_obj(Some(ctx), Some(&data), st)?;
        self.emit_objective(oid);
        Ok(data)
    }

    pub(crate) fn review_verify(
        &mut self,
        ctx: &mut ReqCtx,
        cmd: ReviewVerifyCommand,
    ) -> Result<ReplyData, Rejection> {
        let oid = self.objective_id(&cmd.objective_id)?;
        self.check_idle(oid)?;
        let candidate_id = Self::candidate_id(&cmd.candidate_id)?;
        if let Some(why) = self.storage_blocker() {
            return Err(Self::quota_rejection(&why));
        }
        let tools = self.sandbox_tools().ok_or_else(|| {
            Rejection::new(
                ErrorCode::ExecutionUnavailable,
                SafeAction::None,
                super::project::VERIFICATION_SANDBOX_UNAVAILABLE,
            )
        })?;
        let mut st = self.obj(oid)?.clone();
        let candidate = st
            .review
            .candidate(candidate_id)
            .ok_or_else(|| not_found("candidate"))?
            .clone();
        let verification_id = VerificationId::new(new_uuid());
        st.review
            .start_verification(
                &st.row.objective,
                NewVerification {
                    id: verification_id,
                    candidate_id,
                    argv: cmd.command.clone(),
                    cwd_label: format!("isolated copy of candidate {}", candidate.ordinal),
                },
                ts_now(),
            )
            .map_err(domain_rejection)?;
        let verification = st
            .review
            .verifications()
            .iter()
            .find(|v| v.id == verification_id)
            .ok_or_else(|| not_found("verification"))?;
        let summary = wire_verification(&st, verification);
        let data = ReplyData::ReviewVerify(summary.clone());
        let ws = self.ws_of(&st)?;
        let input = VerifyInput {
            repo: ws.repo.clone(),
            artifacts: self.artifacts.clone(),
            tools: Some(tools),
            copy_dir: self.paths.verify.join(verification_id.to_string()),
            branch: format!("lade/verify/{verification_id}"),
            tree: candidate.tree.clone(),
            base_target: candidate.base_target_commit.clone(),
            argv: cmd.command,
            objective: oid.to_string(),
            marker: crate::supervision::sweep::sandbox_marker(&self.paths.run),
            cgroups: self.cgroups.clone(),
        };
        self.commit_obj(Some(ctx), Some(&data), st)?;
        self.emit(Event::VerificationChanged(summary));
        self.emit_objective(oid);
        self.spawn_verify(move || Completed::Verify {
            oid,
            verification_id,
            out: jobs::verify(&input),
        });
        Ok(data)
    }

    pub(crate) fn verify_done(
        &mut self,
        oid: ObjectiveId,
        verification_id: VerificationId,
        out: &jobs::VerifyOut,
    ) {
        let Some(mut st) = self.objectives.get(&oid).cloned() else {
            return;
        };
        let artifact = out
            .artifact
            .as_deref()
            .and_then(|hex| ArtifactDigest::parse(hex).ok());
        if st
            .review
            .finish_verification(verification_id, out.end, artifact, out.truncated, ts_now())
            .is_err()
        {
            return;
        }
        st.tails.insert(verification_id, out.tail.clone());
        let committed = self.commit(None, None, |tx| {
            Self::save_tx(tx, &st)?;
            storage::set_verification_tail(tx, verification_id, &out.tail)
        });
        if committed.is_err() {
            eprintln!("laded: could not record verification result objective={oid}");
            return;
        }
        let summary = st
            .review
            .verifications()
            .iter()
            .find(|v| v.id == verification_id)
            .map(|v| wire_verification(&st, v));
        self.objectives.insert(oid, st);
        if let Some(summary) = summary {
            self.emit(Event::VerificationChanged(summary));
        }
        self.emit_objective(oid);
    }

    pub(crate) fn review_approve(
        &mut self,
        ctx: &mut ReqCtx,
        cmd: ReviewApproveCommand,
    ) -> Result<ReplyData, Rejection> {
        let oid = self.objective_id(&cmd.objective_id)?;
        self.check_idle(oid)?;
        let candidate_id = Self::candidate_id(&cmd.candidate_id)?;
        let seen = from_wire_git(&cmd.target_commit)?;
        let st = self.obj(oid)?.clone();
        let current = self
            .fresh_target(&st)
            .ok_or_else(|| not_found("integration target branch"))?;
        let mut st = st;
        let approval_id = ApprovalId::new(new_uuid());
        st.review
            .approve(
                &mut st.row.objective,
                ApproveCandidate {
                    approval_id,
                    candidate_id,
                    target_commit: seen,
                    override_reason: cmd.override_reason,
                },
                &current,
                ts_now(),
            )
            .map_err(domain_rejection)?;
        let approval = st
            .review
            .valid_approval()
            .ok_or_else(|| not_found("approval"))?;
        let data = ReplyData::ReviewApprove(wire_approval(approval));
        self.commit_obj(Some(ctx), Some(&data), st)?;
        self.emit_objective(oid);
        Ok(data)
    }

    pub(crate) fn review_refresh(&mut self, ctx: ReqCtx, cmd: &CandidateCommand) -> Flow {
        let prepared = (|| {
            let oid = self.objective_id(&cmd.objective_id)?;
            self.check_idle(oid)?;
            let candidate_id = Self::candidate_id(&cmd.candidate_id)?;
            let st = self.obj(oid)?.clone();
            if !matches!(
                st.objective().lifecycle(),
                Lifecycle::Frozen | Lifecycle::Approved
            ) {
                return Err(domain_rejection(lade_core::DomainError::InvalidLifecycle(
                    "refresh requires a frozen candidate",
                )));
            }
            let candidate = st
                .review
                .current_candidate()
                .filter(|c| c.id == candidate_id)
                .ok_or_else(|| domain_rejection(lade_core::DomainError::StaleCandidate))?
                .clone();
            let target = self
                .fresh_target(&st)
                .ok_or_else(|| not_found("integration target branch"))?;
            if target == candidate.base_target_commit {
                return Err(conflict("target has not moved"));
            }
            let ws = self.ws_of(&st)?;
            let new_id = CandidateId::new(new_uuid());
            Ok((
                oid,
                candidate_id,
                new_id,
                RefreshInput {
                    repo: ws.repo.clone(),
                    artifacts: self.artifacts.clone(),
                    target_branch: st
                        .objective()
                        .record()
                        .integration_target
                        .as_str()
                        .to_string(),
                    candidate_commit: candidate.commit,
                    retain_ref: format!("candidate-{new_id}-tree"),
                },
            ))
        })();
        match prepared {
            Ok((oid, candidate_id, new_id, input)) => {
                self.rt(oid).busy = Some("refresh");
                self.spawn(move || Completed::Refresh {
                    oid,
                    ctx,
                    candidate_id,
                    new_id,
                    result: jobs::refresh(&input),
                });
                Flow::Deferred
            }
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                Flow::Done
            }
        }
    }

    pub(crate) fn refresh_done(
        &mut self,
        oid: ObjectiveId,
        mut ctx: ReqCtx,
        candidate_id: CandidateId,
        new_id: CandidateId,
        result: Result<jobs::RefreshOut, Rejection>,
    ) {
        self.rt(oid).busy = None;
        let outcome = (|| {
            let out = result?;
            let mut st = self.obj(oid)?.clone();
            let candidate = st
                .review
                .candidate(candidate_id)
                .ok_or_else(|| not_found("candidate"))?
                .clone();
            let now = ts_now();
            match out {
                jobs::RefreshOut::Rebuilt {
                    tree,
                    target,
                    counts,
                    manifest_digest,
                } => {
                    let result = st
                        .review
                        .refresh(
                            &mut st.row.objective,
                            candidate_id,
                            &target,
                            RefreshOutcome::Rebuilt(NewCandidate {
                                id: new_id,
                                commit: candidate.commit,
                                tree,
                                base_target_commit: target.clone(),
                                manifest_digest: ArtifactDigest::parse(&manifest_digest)
                                    .map_err(domain_rejection)?,
                                changes: counts,
                            }),
                            now,
                        )
                        .map_err(domain_rejection)?;
                    let RefreshResult::Refreshed { candidate_id } = result else {
                        return Err(conflict("refresh did not produce a candidate"));
                    };
                    let fresh = st
                        .review
                        .candidate(candidate_id)
                        .ok_or_else(|| not_found("candidate"))?;
                    let data = ReplyData::ReviewRefresh(wire_candidate(fresh));
                    st.row.integration_conflict = false;
                    self.commit_obj(Some(&mut ctx), Some(&data), st)?;
                    Ok(data)
                }
                jobs::RefreshOut::Conflict { target } => {
                    st.review
                        .refresh(
                            &mut st.row.objective,
                            candidate_id,
                            &target,
                            RefreshOutcome::Conflict,
                            now,
                        )
                        .map_err(domain_rejection)?;
                    st.row.integration_conflict = true;
                    self.commit_obj(None, None, st)?;
                    Err(Rejection::new(
                        ErrorCode::StaleTarget,
                        SafeAction::Refresh,
                        "refresh conflicted with the moved target; the objective was returned for revision",
                    ))
                }
            }
        })();
        self.finish(ctx, outcome);
        self.emit_objective(oid);
    }

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
        let queued = self
            .objective_id(&cmd.objective_id)
            .ok()
            .and_then(|oid| Some((oid, self.integration_key(oid)?)))
            .filter(|(_, key)| self.integrations.queued.contains_key(key));
        if let Some((oid, key)) = queued {
            if let Err(rejection) = self.check_idle(oid) {
                self.finish(ctx, Err(rejection));
                return Flow::Done;
            }
            self.rt(oid).busy = Some("integrate");
            if let Some(waiting) = self.integrations.queued.get_mut(&key) {
                waiting.push_back((ctx, cmd.clone()));
            }
            self.emit_objective(oid);
            return Flow::Deferred;
        }
        let prepared = (|| {
            let oid = self.objective_id(&cmd.objective_id)?;
            self.check_idle(oid)?;
            let approval_id = ApprovalId::new(parse_uuid(&cmd.approval_id)?);
            let st = self.obj(oid)?.clone();
            let current = self
                .fresh_target(&st)
                .ok_or_else(|| not_found("integration target branch"))?;
            match st
                .review
                .check_integration(st.objective(), approval_id, &current)
            {
                Ok(plan) => Ok((oid, st, plan)),
                Err(lade_core::DomainError::StaleTarget) => {
                    let mut st = st;
                    st.review
                        .observe_target(&mut st.row.objective, &current, ts_now())
                        .map_err(domain_rejection)?;
                    if st.review.valid_approval().is_some() {
                        st.review
                            .invalidate_approval(
                                &mut st.row.objective,
                                lade_core::domain::review::InvalidationReason::TargetMoved,
                                ts_now(),
                            )
                            .map_err(domain_rejection)?;
                    }
                    self.commit_obj(None, None, st)?;
                    self.emit_objective(oid);
                    Err(domain_rejection(lade_core::DomainError::StaleTarget))
                }
                Err(e) => Err(domain_rejection(e)),
            }
        })();
        let (oid, st, plan) = match prepared {
            Ok(prepared) => prepared,
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                return Flow::Done;
            }
        };
        let ws = match self.ws_of(&st) {
            Ok(ws) => ws.repo.clone(),
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                return Flow::Done;
            }
        };
        let record = st.objective().record();
        let journal = JournalRow {
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
        };
        let op = ctx.op.clone();
        let journaled = self.commit(None, None, |tx| {
            storage::insert_journal(tx, &journal, now_ms())?;
            if let Some(op) = &op {
                storage::record_operation(
                    tx,
                    op.key(ctx.name),
                    OperationState::Pending,
                    None,
                    now_ms(),
                )?;
            }
            Ok(())
        });
        if let Err(rejection) = journaled {
            self.finish(ctx, Err(rejection));
            return Flow::Done;
        }
        crate::fault::point("integrate.intent");
        let request = SquashRequest {
            target_branch: journal.target_branch.clone(),
            expected_target: adapter_git(&plan.expected_target),
            candidate_tree: adapter_git(&plan.candidate_tree),
            message: journal.message,
            timestamp: Some(journal.commit_time),
        };
        self.rt(oid).busy = Some("integrate");
        if let Some(key) = self.integration_key(oid) {
            self.integrations.queued.entry(key).or_default();
        }
        self.spawn(move || Completed::Integrate {
            oid,
            ctx,
            plan,
            journal_id: journal.id,
            result: ws.integrate_squash(&request),
        });
        Flow::Deferred
    }

    pub(crate) fn integrate_done(
        &mut self,
        oid: ObjectiveId,
        mut ctx: ReqCtx,
        plan: &IntegrationPlan,
        journal_id: lade_core::domain::ids::Uuid,
        result: Result<crate::adapters::git::SquashOutcome, GitError>,
    ) {
        self.rt(oid).busy = None;
        let now = now_ms();
        let outcome = match result {
            Ok(squash) => (|| {
                crate::fault::point("integrate.applied");
                let commit = super::convert::core_git(&squash.commit);
                let mut st = self.obj(oid)?.clone();
                st.review
                    .complete_integration(&mut st.row.objective, plan, ts_now())
                    .map_err(domain_rejection)?;
                st.row.integration_conflict = false;
                let data = ReplyData::ReviewIntegrate(Box::new(self.detail_of(&st)));
                self.commit(Some(&mut ctx), Some(&data), |tx| {
                    Self::save_tx(tx, &st)?;
                    storage::update_journal(
                        tx,
                        journal_id,
                        JournalState::Done,
                        Some(&commit),
                        None,
                        now,
                    )
                })?;
                self.objectives.insert(oid, st);
                Ok(data)
            })(),
            Err(error) => {
                let _ = storage::update_journal(
                    self.store.conn(),
                    journal_id,
                    JournalState::Failed,
                    None,
                    Some(error.code()),
                    now,
                );
                eprintln!(
                    "laded: integration refused objective={oid} code={}",
                    error.code()
                );
                if let Ok(mut st) = self.obj(oid).cloned() {
                    match &error {
                        GitError::TargetMoved { .. } => {
                            if let Some(current) = self.fresh_target(&st) {
                                let _ = st.review.observe_target(
                                    &mut st.row.objective,
                                    &current,
                                    ts_now(),
                                );
                            }
                        }
                        GitError::CheckoutFailed { .. } => st.row.integration_conflict = true,
                        _ => {}
                    }
                    let _ = self.commit_obj(None, None, st);
                }
                Err(git_rejection(error))
            }
        };
        self.finish(ctx, outcome);
        self.emit_objective(oid);
        let siblings: Vec<ObjectiveId> = self
            .objectives
            .iter()
            .filter(|(other, st)| {
                **other != oid
                    && self.objectives.get(&oid).is_some_and(|me| {
                        me.objective().record().integration_target
                            == st.objective().record().integration_target
                            && me.objective().record().workspace_id
                                == st.objective().record().workspace_id
                    })
            })
            .map(|(id, _)| *id)
            .collect();
        for sibling in siblings {
            self.emit_objective(sibling);
        }
        let waiting = self
            .integration_key(oid)
            .and_then(|key| self.integrations.queued.remove(&key))
            .unwrap_or_default();
        for (ctx, cmd) in waiting {
            if let Ok(next) = self.objective_id(&cmd.objective_id) {
                self.rt(next).busy = None;
            }
            let _ = self.review_integrate(ctx, &cmd);
        }
    }

    pub(crate) fn objective_complete(&mut self, ctx: ReqCtx, id: &Id) -> Flow {
        let prepared = (|| {
            let oid = self.objective_id(id)?;
            self.check_idle(oid)?;
            let st = self.obj(oid)?;
            if !matches!(
                st.objective().lifecycle(),
                Lifecycle::Defined | Lifecycle::Active | Lifecycle::Frozen
            ) {
                return Err(domain_rejection(lade_core::DomainError::InvalidLifecycle(
                    "objective is approved or closed",
                )));
            }
            if st.objective().execution().state().is_live() {
                return Err(domain_rejection(lade_core::DomainError::InvalidLifecycle(
                    "stop execution first",
                )));
            }
            let ws = self.ws_of(st)?;
            let record = st.objective().record();
            Ok((
                oid,
                ws.repo.clone(),
                st.row.worktree.clone(),
                record.integration_target.as_str().to_string(),
                record.start_commit.clone(),
            ))
        })();
        match prepared {
            Ok((oid, repo, worktree, target, start)) => {
                self.rt(oid).busy = Some("complete");
                let scratch = self.paths.tmp.clone();
                self.spawn(move || Completed::Complete {
                    oid,
                    ctx,
                    result: jobs::change_set_empty(
                        &repo,
                        worktree.as_deref(),
                        &scratch,
                        &target,
                        &start,
                    ),
                });
                Flow::Deferred
            }
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                Flow::Done
            }
        }
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
            let data = ReplyData::ObjectiveComplete(Box::new(self.detail_of(&st)));
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
        let data = ReplyData::ObjectiveDiscard(Box::new(self.detail_of(&st)));
        self.commit_obj(Some(ctx), Some(&data), st)?;
        self.emit_objective(oid);
        Ok(data)
    }

    pub(crate) fn objective_cleanup(&mut self, ctx: ReqCtx, cmd: &ObjectiveCleanupCommand) -> Flow {
        let prepared = (|| {
            let oid = self.objective_id(&cmd.objective_id)?;
            self.check_idle(oid)?;
            let st = self.obj(oid)?;
            if st.objective().execution().state().is_live() {
                return Err(domain_rejection(lade_core::DomainError::InvalidLifecycle(
                    "stop execution first",
                )));
            }
            let mut probe = st.clone();
            probe
                .objective_mut()
                .cleanup(st.unsent_drafts(), cmd.confirm_data_loss, ts_now())
                .map_err(domain_rejection)?;
            let ws = self.ws_of(st)?;
            Ok((
                oid,
                ws.repo.clone(),
                st.row.worktree.clone(),
                st.row.git_branch.clone(),
            ))
        })();
        match prepared {
            Ok((oid, repo, worktree, branch)) => {
                let confirm = cmd.confirm_data_loss;
                let scratch = self.paths.tmp.clone();
                self.rt(oid).busy = Some("cleanup");
                self.spawn(move || Completed::Cleanup {
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
                });
                Flow::Deferred
            }
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                Flow::Done
            }
        }
    }

    pub(crate) fn objective_delete_history(
        &mut self,
        ctx: ReqCtx,
        cmd: &ObjectiveHistoryDeleteCommand,
    ) -> Flow {
        let prepared = (|| {
            let oid = self.objective_id(&cmd.objective_id)?;
            self.check_idle(oid)?;
            let st = self.obj(oid)?;
            if let Some(reason) = delete_history_blocker(st) {
                return Err(domain_rejection(lade_core::DomainError::InvalidLifecycle(
                    reason,
                )));
            }
            if !cmd.confirm_history_loss {
                return Err(super::convert::invalid(
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
            Ok((oid, self.ws_of(st)?.repo.clone(), refs))
        })();
        match prepared {
            Ok((oid, repo, refs)) => {
                self.rt(oid).busy = Some("delete_history");
                self.spawn(move || Completed::DeleteHistory {
                    oid,
                    ctx,
                    result: jobs::delete_refs(&repo, &refs),
                });
                Flow::Deferred
            }
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                Flow::Done
            }
        }
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
            let data = ReplyData::ObjectiveDeleteHistory(Box::new(self.detail_of(&st)));
            self.commit_obj(Some(&mut ctx), Some(&data), st)
                .map_err(|_| durability("history deleted but not recorded; query the objective"))?;
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
            crate::fault::point("cleanup.removed");
        }
        let outcome = (|| {
            result?;
            let mut st = self.obj(oid)?.clone();
            let unsent = st.unsent_drafts();
            st.objective_mut()
                .cleanup(unsent, confirm, ts_now())
                .map_err(domain_rejection)?;
            st.row.worktree = None;
            let data = ReplyData::ObjectiveCleanup(Box::new(self.detail_of(&st)));
            self.commit_obj(Some(&mut ctx), Some(&data), st)
                .map_err(|_| durability("cleanup done but not recorded; query the objective"))?;
            Ok(data)
        })();
        self.finish(ctx, outcome);
        self.emit_objective(oid);
    }
}
