//! Frozen review: freeze (writer exclusion + exact candidate), isolated verification, approval,
//! return for revision, refresh after a target move, journaled protected integration, completion
//! without changes, discard and cleanup.

use std::time::Instant;

use lade_core::domain::execution::ExecutionState;
use lade_core::domain::ids::{ApprovalId, CandidateId, ObjectiveId, Timestamp, VerificationId};
use lade_core::domain::inspection::ArtifactDigest;
use lade_core::domain::objectives::Lifecycle;
use lade_core::domain::review::{
    ApproveCandidate, IntegrationPlan, NewCandidate, NewVerification, RefreshOutcome, RefreshResult,
};
use lade_protocol::client::{
    CandidateCommand, ErrorCode, Event, NoticeLevel, ObjectiveCleanupCommand, Rejection, ReplyData,
    ReviewApproveCommand, ReviewIntegrateCommand, ReviewVerifyCommand, SafeAction,
};
use lade_protocol::types::Id;

use crate::adapters::git::{GitError, SquashRequest};
use crate::storage::{self, JournalRow};

use super::convert::{
    adapter_git, conflict, domain_rejection, durability, from_wire_git, git_rejection, invalid,
    new_uuid, not_found, now_ms, parse_uuid, rejection,
};
use super::handlers::{Pending, sql};
use super::jobs::{self, FreezeInput, RefreshInput, VerifyInput};
use super::project::{wire_approval, wire_candidate, wire_verification};
use super::{Flow, JobOutput, ReqCtx, Service};

fn ts_now() -> Timestamp {
    Timestamp::from_unix_ms(now_ms())
}

fn domain(error: lade_core::DomainError) -> Rejection {
    domain_rejection(&error)
}

impl Service {
    fn candidate_id(id: &Id) -> Result<CandidateId, Rejection> {
        Ok(CandidateId::new(parse_uuid(id)?))
    }

    // -----------------------------------------------------------------------------------------
    // Freeze.

    pub(crate) fn review_freeze(&mut self, mut ctx: ReqCtx, id: &Id) -> Flow {
        let prepared = (|| {
            let oid = self.objective_id(id)?;
            self.check_idle(oid)?;
            let st = self.obj(oid)?;
            if st.objective().lifecycle() != Lifecycle::Active {
                return Err(domain(lade_core::DomainError::InvalidLifecycle(
                    "freeze requires an active objective",
                )));
            }
            if st.row.worktree.is_none() {
                return Err(invalid("no agent work to freeze yet"));
            }
            // Same precondition as the projection; the job re-checks the exact content.
            if st.capture.as_ref().is_some_and(|c| c.counts.files == 0) {
                return Err(domain(lade_core::DomainError::InvalidLifecycle(
                    super::project::NO_CHANGES_TO_FREEZE,
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
        // Review writer exclusion: no terminal may write the worktree from here on.
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
        // Exclude the writer first: stop the execution and wait for the observed exit.
        let mut st = self.objectives[&oid].clone();
        if st.objective().execution().state() != &ExecutionState::Stopping
            && let Err(e) = st.objective_mut().request_stop(ts_now())
        {
            self.rt(oid).busy = None;
            self.finish(ctx, Err(domain(e)));
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

    /// Continues a freeze that was waiting for the worker to exit.
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
            Ok((candidate_id, input)) => self.spawn(
                Pending::Freeze {
                    oid,
                    ctx,
                    candidate_id,
                },
                move || JobOutput::Freeze(jobs::freeze(&input)),
            ),
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
                            .map_err(domain)?,
                        changes: out.counts,
                    },
                    now,
                )
                .map_err(domain)?;
            if out.changed {
                st.review
                    .invalidate_candidate(&mut st.row.objective, now)
                    .map_err(domain)?;
            }
            st.row.integration_conflict = false;
            let candidate = st
                .review
                .candidate(candidate_id)
                .ok_or_else(|| not_found("candidate"))?;
            let data = ReplyData::ReviewFreeze(wire_candidate(candidate));
            self.commit_obj(Some(&mut ctx), Some(&data), st)?;
            Ok((data, out.changed))
        })();
        match outcome {
            Ok((data, changed)) => {
                self.finish(ctx, Ok(data));
                if changed {
                    self.notice(
                        NoticeLevel::Warning,
                        "candidate_invalidated",
                        "the worktree changed while freezing; the candidate lost review authority",
                    );
                }
            }
            Err(rejection) => self.finish(ctx, Err(rejection)),
        }
        self.emit_objective(oid);
    }

    // -----------------------------------------------------------------------------------------
    // Return, verify, approve.

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
            .map_err(domain)?;
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
        let tools = self.sandbox_tools().ok_or_else(|| {
            rejection(
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
            .map_err(domain)?;
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
        self.spawn(
            Pending::Verify {
                oid,
                verification_id,
            },
            move || JobOutput::Verify(jobs::verify(input)),
        );
        Ok(data)
    }

    pub(crate) fn verify_done(
        &mut self,
        oid: ObjectiveId,
        verification_id: VerificationId,
        out: jobs::VerifyOut,
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
            .map_err(domain)?;
        let approval = st
            .review
            .valid_approval()
            .ok_or_else(|| not_found("approval"))?;
        let data = ReplyData::ReviewApprove(wire_approval(approval));
        self.commit_obj(Some(ctx), Some(&data), st)?;
        self.emit_objective(oid);
        Ok(data)
    }

    // -----------------------------------------------------------------------------------------
    // Refresh.

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
                return Err(domain(lade_core::DomainError::InvalidLifecycle(
                    "refresh requires a frozen candidate",
                )));
            }
            let candidate = st
                .review
                .current_candidate()
                .filter(|c| c.id == candidate_id)
                .ok_or_else(|| domain(lade_core::DomainError::StaleCandidate))?
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
                self.spawn(
                    Pending::Refresh {
                        oid,
                        ctx,
                        candidate_id,
                        new_id,
                    },
                    move || JobOutput::Refresh(jobs::refresh(&input)),
                );
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
                                    .map_err(domain)?,
                                changes: counts,
                            }),
                            now,
                        )
                        .map_err(domain)?;
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
                        .map_err(domain)?;
                    st.row.integration_conflict = true;
                    self.commit_obj(None, None, st)?;
                    Err(rejection(
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

    // -----------------------------------------------------------------------------------------
    // Integration.

    pub(crate) fn review_integrate(&mut self, ctx: ReqCtx, cmd: &ReviewIntegrateCommand) -> Flow {
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
                    // The approval no longer matches the target: it expires now, durably.
                    let mut st = st;
                    st.review
                        .observe_target(&mut st.row.objective, &current, ts_now())
                        .map_err(domain)?;
                    if st.review.valid_approval().is_some() {
                        st.review
                            .invalidate_approval(
                                &mut st.row.objective,
                                lade_core::domain::review::InvalidationReason::TargetMoved,
                                ts_now(),
                            )
                            .map_err(domain)?;
                    }
                    self.commit_obj(None, None, st)?;
                    self.emit_objective(oid);
                    Err(domain(lade_core::DomainError::StaleTarget))
                }
                Err(e) => Err(domain(e)),
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
            id: new_uuid().to_string(),
            objective_id: oid,
            approval_id: plan.approval_id,
            candidate_id: plan.candidate_id,
            target_branch: record.integration_target.as_str().to_string(),
            expected_target: plan.expected_target.clone(),
            candidate_tree: plan.candidate_tree.clone(),
            state: "intent".into(),
            result_commit: None,
        };
        let message = format!(
            "{}\n\nIntegrated by LADE from objective {oid} (candidate {}).",
            record.title.as_str(),
            plan.candidate_id
        );
        let commit_time = now_ms() / 1000;
        if let Err(rejection) = sql(storage::insert_journal(
            self.store.conn(),
            &journal,
            &message,
            commit_time,
            now_ms(),
        )) {
            self.finish(ctx, Err(rejection));
            return Flow::Done;
        }
        let request = SquashRequest {
            target_branch: journal.target_branch.clone(),
            expected_target: adapter_git(&plan.expected_target),
            candidate_tree: adapter_git(&plan.candidate_tree),
            message,
            timestamp: Some(commit_time),
        };
        self.rt(oid).busy = Some("integrate");
        self.spawn(
            Pending::Integrate {
                oid,
                ctx,
                plan,
                journal_id: journal.id,
            },
            move || JobOutput::Integrate(jobs::integrate(&ws, &request)),
        );
        Flow::Deferred
    }

    pub(crate) fn integrate_done(
        &mut self,
        oid: ObjectiveId,
        mut ctx: ReqCtx,
        plan: &IntegrationPlan,
        journal_id: &str,
        result: Result<crate::adapters::git::SquashOutcome, GitError>,
    ) {
        self.rt(oid).busy = None;
        let now = now_ms();
        let outcome = match result {
            Ok(squash) => (|| {
                let commit = super::convert::core_git(&squash.commit);
                let mut st = self.obj(oid)?.clone();
                st.review
                    .complete_integration(&mut st.row.objective, plan, ts_now())
                    .map_err(domain)?;
                st.row.integration_conflict = false;
                let data = ReplyData::ReviewIntegrate(Box::new(self.detail_of(&st)));
                self.commit(Some(&mut ctx), Some(&data), |tx| {
                    Self::save_tx(tx, &st)?;
                    storage::update_journal(tx, journal_id, "done", Some(&commit), None, now)
                })?;
                self.objectives.insert(oid, st);
                Ok(data)
            })(),
            Err(error) => {
                let _ = storage::update_journal(
                    self.store.conn(),
                    journal_id,
                    "failed",
                    None,
                    Some(error.code()),
                    now,
                );
                eprintln!(
                    "laded: integration refused objective={oid} code={}",
                    error.code()
                );
                let rejection = git_rejection(&error);
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
                Err(rejection)
            }
        };
        self.finish(ctx, outcome);
        self.emit_objective(oid);
        // Other Objectives targeting the same branch now see a moved target.
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
    }

    // -----------------------------------------------------------------------------------------
    // Completion, discard, cleanup.

    pub(crate) fn objective_complete(&mut self, ctx: ReqCtx, id: &Id) -> Flow {
        let prepared = (|| {
            let oid = self.objective_id(id)?;
            self.check_idle(oid)?;
            let st = self.obj(oid)?;
            if !matches!(
                st.objective().lifecycle(),
                Lifecycle::Defined | Lifecycle::Active | Lifecycle::Frozen
            ) {
                return Err(domain(lade_core::DomainError::InvalidLifecycle(
                    "objective is approved or closed",
                )));
            }
            if st.objective().execution().state().is_live() {
                return Err(domain(lade_core::DomainError::InvalidLifecycle(
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
                self.spawn(Pending::Complete { oid, ctx }, move || {
                    JobOutput::Flag(jobs::change_set_empty(
                        &repo,
                        worktree.as_deref(),
                        &scratch,
                        &target,
                        &start,
                    ))
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
                .map_err(domain)?;
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
            .map_err(domain)?;
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
                return Err(domain(lade_core::DomainError::InvalidLifecycle(
                    "stop execution first",
                )));
            }
            let mut probe = st.clone();
            probe
                .objective_mut()
                .cleanup(st.unsent_drafts(), cmd.confirm_data_loss, ts_now())
                .map_err(domain)?;
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
                self.spawn(Pending::Cleanup { oid, ctx, confirm }, move || {
                    JobOutput::Unit(jobs::cleanup(
                        &repo,
                        worktree.as_deref(),
                        branch.as_deref(),
                        &scratch,
                        confirm,
                    ))
                });
                Flow::Deferred
            }
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                Flow::Done
            }
        }
    }

    pub(crate) fn cleanup_done(
        &mut self,
        oid: ObjectiveId,
        mut ctx: ReqCtx,
        confirm: bool,
        result: Result<(), Rejection>,
    ) {
        self.rt(oid).busy = None;
        let outcome = (|| {
            result?;
            let mut st = self.obj(oid)?.clone();
            let unsent = st.unsent_drafts();
            st.objective_mut()
                .cleanup(unsent, confirm, ts_now())
                .map_err(domain)?;
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
