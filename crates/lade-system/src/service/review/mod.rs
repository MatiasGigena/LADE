//! The Review of an Objective's work: freezing a Candidate, returning it for revision,
//! verifying and approving it, and refreshing it onto a moved target. Integration and the end
//! of an Objective live in the submodules.

mod closing;
mod integrate;
#[cfg(test)]
mod tests;

use std::time::Instant;

use lade_core::DomainError;
use lade_core::domain::execution::ExecutionState;
use lade_core::domain::ids::{ApprovalId, CandidateId, ObjectiveId, VerificationId};
use lade_core::domain::inspection::ArtifactDigest;
use lade_core::domain::objectives::Lifecycle;
use lade_core::domain::review::{
    ApproveCandidate, NewCandidate, NewVerification, RefreshOutcome, RefreshResult,
};
use lade_protocol::client::{
    CandidateCommand, ErrorCode, Event, NoticeCode, NoticeLevel, Rejection, ReplyData,
    ReviewApproveCommand, ReviewVerifyCommand, SafeAction,
};
use lade_protocol::types::Id;

use crate::storage;

use super::convert::{
    conflict, domain_rejection, from_wire_git, invalid, limit_reached, not_found, parse_uuid,
};
use super::convert::{wire_approval, wire_candidate, wire_verification};
use super::jobs::{self, FreezeInput, RefreshInput, VerifyInput};
use super::projection::actions::{VERIFICATION_SANDBOX_UNAVAILABLE, freeze_blocker};
use super::{Completed, Flow, ReqCtx, Service};
use crate::fault;
use crate::service::state::Busy;
use crate::supervision::sweep::sandbox_marker;
use crate::support::{new_uuid, ts_now};

pub(crate) use integrate::Integrations;

impl Service {
    fn candidate_id(id: &Id) -> Result<CandidateId, Rejection> {
        Ok(CandidateId::new(parse_uuid(id)?))
    }

    pub(crate) fn review_freeze(&mut self, mut ctx: ReqCtx, id: &Id) -> Flow {
        let prepared = (|| {
            let oid = self.objective_id(id)?;
            self.check_idle(oid)?;
            if let Some(reason) = freeze_blocker(self.obj(oid)?) {
                return Err(domain_rejection(DomainError::InvalidLifecycle(reason)));
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
        self.rt(oid).busy = Some(Busy::Freeze);
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
            fault::point("freeze.recorded");
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
        let data = ReplyData::ReviewReturn(Box::new(self.fresh_detail(&st)));
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
            return Err(limit_reached(&why));
        }
        let tools = self.sandbox_tools().ok_or_else(|| {
            Rejection::new(
                ErrorCode::ExecutionUnavailable,
                SafeAction::None,
                VERIFICATION_SANDBOX_UNAVAILABLE,
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
            marker: sandbox_marker(&self.paths.run),
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
        if !Self::persisted("the verification result", oid, committed) {
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
                return Err(domain_rejection(DomainError::InvalidLifecycle(
                    "refresh requires a frozen candidate",
                )));
            }
            let candidate = st
                .review
                .current_candidate()
                .filter(|c| c.id == candidate_id)
                .ok_or_else(|| domain_rejection(DomainError::StaleCandidate))?
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
                self.rt(oid).busy = Some(Busy::Refresh);
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
}
