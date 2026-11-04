//! Execution of an Objective's agent: starting, resuming and cancelling the worker, its
//! launch, and the stop timeout. Worker events, activity and the writable-storage limit live
//! in the submodules.

mod activity;
#[cfg(test)]
mod tests;
mod worker;
mod writable;

use std::time::Duration;

use lade_core::domain::execution::{ExecutionGeneration, ExecutionState, ModelRef};
use lade_core::domain::feedback::{BlockedReason, DeliveryState};
use lade_core::domain::ids::{ObjectiveId, SessionId, Timestamp};
use lade_protocol::client::{
    ErrorCode, Event, ExecutionStartCommand, NoticeCode, NoticeLevel, Rejection, ReplyData,
    SafeAction,
};
use lade_protocol::types::{Id, Model};
use lade_protocol::worker::WorkerBody;

use crate::adapters::providers::FIXTURE_PROVIDER;
use crate::storage;
use crate::supervision::credentials::FIXTURE_REFUSED;
use crate::supervision::{LaunchBinding, LaunchError, LaunchRequest};

use super::convert::wire_input;
use super::convert::{domain_rejection, preview, wire_id};
use super::jobs::{self, LaunchInput, ProfileSnapshot, Provision};
use super::{Completed, ReqCtx, Service};
use crate::support::{new_uuid, now_ms};

pub(crate) use activity::{LiveActivity, wire_activity};
pub(crate) use writable::{LIMIT_REACHED, StorageUsage};

const STOP_TIMEOUT: Duration = Duration::from_secs(30);
pub const AGENT_SANDBOX_UNAVAILABLE: &str =
    "LADE can't run agents on this computer because the sandbox check failed.";

fn ts_now() -> Timestamp {
    Timestamp::from_unix_ms(now_ms())
}

impl Service {
    pub(crate) fn execution_start(
        &mut self,
        ctx: &mut ReqCtx,
        cmd: &ExecutionStartCommand,
    ) -> Result<ReplyData, Rejection> {
        let oid = self.objective_id(&cmd.objective_id)?;
        self.check_idle(oid)?;
        let model = ModelRef::new(&cmd.model.provider, &cmd.model.id).map_err(domain_rejection)?;
        let mut st = self.obj(oid)?.clone();
        self.ws_of(&st)?;
        let session = SessionId::new(new_uuid());
        let generation = st
            .objective_mut()
            .start_execution(session, model, ts_now())
            .map_err(domain_rejection)?;
        if let Some(why) = self.worker_blocker(oid) {
            return Err(Self::quota_rejection(&why));
        }
        self.preflight(&cmd.model.provider)?;
        if let Some(thinking) = cmd.thinking {
            self.record_start_thinking(oid, thinking)?;
        }
        let data = ReplyData::ExecutionStart(Box::new(self.detail_of(&st)));
        let association = st.objective().execution().session().cloned();
        let now = now_ms();
        self.commit(Some(ctx), Some(&data), |tx| {
            Self::save_tx(tx, &st)?;
            if let Some(association) = &association {
                storage::save_session(tx, oid, association, generation.value(), now)?;
            }
            Ok(())
        })?;
        self.objectives.insert(oid, st);
        self.launch(oid, false);
        self.emit_objective(oid);
        Ok(data)
    }

    pub(crate) fn preflight(&self, provider: &str) -> Result<(), Rejection> {
        if provider == FIXTURE_PROVIDER && !self.test_fixture_provider {
            return Err(Rejection::new(
                ErrorCode::ExecutionUnavailable,
                SafeAction::None,
                FIXTURE_REFUSED,
            ));
        }
        self.launcher.preflight(provider).map_err(|e| {
            let detail = match &e {
                LaunchError::SandboxUnavailable(_) => {
                    eprintln!("laded: agent start refused: {}", e.detail());
                    AGENT_SANDBOX_UNAVAILABLE.to_owned()
                }
                LaunchError::ProviderUnavailable(_) | LaunchError::Io(_) => e.detail(),
            };
            Rejection::new(ErrorCode::ExecutionUnavailable, SafeAction::None, &detail)
        })
    }

    pub(crate) fn execution_cancel(
        &mut self,
        ctx: &mut ReqCtx,
        id: &Id,
    ) -> Result<ReplyData, Rejection> {
        let oid = self.objective_id(id)?;
        let mut st = self.obj(oid)?.clone();
        st.objective_mut()
            .request_stop(ts_now())
            .map_err(domain_rejection)?;
        let data = ReplyData::ExecutionCancel(Box::new(self.detail_of(&st)));
        self.commit_obj(Some(ctx), Some(&data), st)?;
        self.stop_worker(oid);
        self.emit_objective(oid);
        Ok(data)
    }

    pub(crate) fn execution_resume(
        &mut self,
        ctx: &mut ReqCtx,
        id: &Id,
    ) -> Result<ReplyData, Rejection> {
        let oid = self.objective_id(id)?;
        self.check_idle(oid)?;
        let mut st = self.obj(oid)?.clone();
        self.ws_of(&st)?;
        let generation = st
            .objective_mut()
            .resume_execution(ts_now())
            .map_err(domain_rejection)?;
        if let Some(why) = self.worker_blocker(oid) {
            return Err(Self::quota_rejection(&why));
        }
        if let Some(session) = st.objective().execution().session() {
            self.preflight(session.model.provider())?;
        }
        let association = st.objective().execution().session().cloned();
        let mut requeued = Vec::new();
        for (_, input) in &mut st.inputs {
            let same = association.as_ref().is_some_and(|a| {
                a.session_id == input.session_id
                    && a.association_revision == input.association_revision
            });
            if same
                && input.delivery
                    == (DeliveryState::Blocked {
                        reason: BlockedReason::ExecutionUnavailable,
                    })
                && input.requeue().is_ok()
            {
                requeued.push(input.clone());
            }
        }
        let data = ReplyData::ExecutionResume(Box::new(self.detail_of(&st)));
        let now = now_ms();
        self.commit(Some(ctx), Some(&data), |tx| {
            Self::save_tx(tx, &st)?;
            if let Some(association) = &association {
                storage::save_session(tx, oid, association, generation.value(), now)?;
            }
            for input in &requeued {
                storage::save_delivery(tx, input.id, &input.delivery, now)?;
            }
            Ok(())
        })?;
        self.objectives.insert(oid, st);
        for input in &requeued {
            self.emit(Event::InputChanged(wire_input(input)));
        }
        self.launch(oid, true);
        self.emit_objective(oid);
        Ok(data)
    }

    fn launch(&mut self, oid: ObjectiveId, resume: bool) {
        let Some(st) = self.objectives.get(&oid) else {
            return;
        };
        let Ok(ws) = self.ws_of(st) else {
            return;
        };
        let record = st.objective().record();
        let generation = record.execution.generation().value();
        let Some(session) = record.execution.session() else {
            return;
        };
        let worktree = st
            .row
            .worktree
            .clone()
            .unwrap_or_else(|| self.paths.worktrees.join(oid.to_string()));
        let provision = st.row.worktree.is_none().then(|| Provision {
            base_branch: record.branch.as_str().to_string(),
            start: record.start_commit.clone(),
        });
        let session_dir = self.paths.sessions.join(oid.to_string());
        let (thinking, session_file) = self.start_extras(oid);
        let model = Model {
            provider: session.model.provider().to_string(),
            id: session.model.id().to_string(),
        };
        let start = WorkerBody::Start {
            objective_id: wire_id(oid.uuid()),
            session_dir: session_dir.to_string_lossy().into_owned(),
            cwd: worktree.to_string_lossy().into_owned(),
            model: model.clone(),
            resume,
            system_note: format!(
                "LADE Objective: {}. Work only inside the current directory; the developer reviews and integrates the result.",
                preview(record.title.as_str(), 200)
            ),
            thinking,
            resources: None,
            session_file: if resume { session_file } else { None },
        };
        let input = LaunchInput {
            repo: ws.repo.clone(),
            launcher: self.launcher.clone(),
            events: self.supervisor_tx.clone(),
            request: LaunchRequest {
                binding: LaunchBinding {
                    objective_id: oid.to_string(),
                    execution_generation: generation,
                },
                worktree,
                repo_common_dir: ws.repo.common_dir().to_path_buf(),
                session_dir,
                provider: model.provider,
                start,
            },
            provision,
            profile: Some(ProfileSnapshot {
                source: self.profile_dir(),
                approved: self.approved_extensions(),
            }),
        };
        self.rt(oid).launching = Some(generation);
        self.spawn(move || Completed::Launch {
            oid,
            generation,
            out: jobs::launch(input),
        });
    }

    pub(crate) fn launch_done(&mut self, oid: ObjectiveId, generation: u64, out: jobs::LaunchOut) {
        self.rt(oid).launching = None;
        let Some(mut st) = self.objectives.get(&oid).cloned() else {
            return;
        };
        if let Some(branch) = &out.provisioned_branch {
            st.row.git_branch = Some(branch.clone());
            st.row.worktree = Some(self.paths.worktrees.join(oid.to_string()));
        }
        let current = st.objective().execution().generation().value() == generation;
        let state = st.objective().execution().state().clone();
        match out.result {
            Ok(control) => {
                if out.provisioned_branch.is_some() {
                    let recorded = self.commit_obj(None, None, st);
                    Self::persisted("the provisioned worktree", oid, recorded);
                }
                if current && state.is_live() {
                    self.rt(oid).control = Some(control);
                    if state == ExecutionState::Stopping {
                        self.stop_worker(oid);
                    }
                } else {
                    self.reap(move || {
                        let mut control = control;
                        control.terminate();
                    });
                }
            }
            Err(reason) => {
                eprintln!("laded: launch failed objective={oid} reason={reason}");
                if current && state.is_live() {
                    let _ = st.objective_mut().observe_failure(
                        ExecutionGeneration::new(generation),
                        &reason.to_string(),
                        ts_now(),
                    );
                    let recorded = self.commit_obj(None, None, st);
                    Self::persisted("the launch failure", oid, recorded);
                    self.notice(
                        NoticeLevel::Error,
                        NoticeCode::ExecutionFailed,
                        &format!("execution could not start: {reason}"),
                    );
                    self.system_item(
                        oid,
                        "Execution failed to start",
                        &format!("The sandboxed worker could not be launched ({reason})."),
                        true,
                    );
                    self.continue_freeze(oid);
                } else if out.provisioned_branch.is_some() {
                    let recorded = self.commit_obj(None, None, st);
                    Self::persisted("the provisioned worktree", oid, recorded);
                }
            }
        }
        self.emit_objective(oid);
    }

    pub(crate) fn stop_worker(&mut self, oid: ObjectiveId) {
        let rt = self.rt(oid);
        if let Some(mut control) = rt.control.take() {
            let _ = control.send(WorkerBody::Abort);
            self.reap(move || {
                control.terminate();
                drop(control);
            });
            return;
        }
        if rt.launching.is_some() {
            return;
        }
        let Some(mut st) = self.objectives.get(&oid).cloned() else {
            return;
        };
        let generation = st.objective().execution().generation();
        if st.objective().execution().state() == &ExecutionState::Stopping
            && st
                .objective_mut()
                .observe_stopped(generation, ts_now())
                .is_ok()
        {
            let recorded = self.commit_obj(None, None, st);
            if Self::persisted("the stopped execution", oid, recorded) {
                self.continue_freeze(oid);
            }
        }
    }

    pub(crate) fn check_timeouts(&mut self) {
        let expired: Vec<ObjectiveId> = self
            .runtime
            .iter()
            .filter(|(_, rt)| {
                rt.waiting_freeze
                    .as_ref()
                    .is_some_and(|(_, since)| since.elapsed() > STOP_TIMEOUT)
            })
            .map(|(oid, _)| *oid)
            .collect();
        for oid in expired {
            let rt = self.rt(oid);
            rt.busy = None;
            if let Some((ctx, _)) = rt.waiting_freeze.take() {
                self.finish(
                    ctx,
                    Err(Rejection::new(
                        ErrorCode::Interrupted,
                        SafeAction::Refresh,
                        "the worker did not stop in time; nothing was frozen",
                    )),
                );
            }
        }
    }
}
