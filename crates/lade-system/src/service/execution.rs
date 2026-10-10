//! Execution lifecycle (start/cancel/resume), supervisor events, ordered input dispatch and
//! activity persistence/broadcast.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use lade_core::domain::execution::{ExecutionState, ModelRef};
use lade_core::domain::feedback::{BlockedReason, DeliveryState, DispatchDecision};
use lade_core::domain::ids::{InputId, ObjectiveId, SessionId, Timestamp};
use lade_protocol::client::{
    ActivityAppended, ActivityDelta, ActivityItem, ErrorCode, Event, ExecutionStartCommand,
    NoticeLevel, Rejection, ReplyData, SafeAction,
};
use lade_protocol::types::{ActivityRole, Id, Model, truncate_utf8};
use lade_protocol::worker::{MAX_ITEM_TEXT_BYTES, TurnState, WorkerBody};

use crate::storage::{self, ActivityRow};
use crate::supervision::{LaunchBinding, LaunchRequest, SupervisorEvent, WorkerEvent};

use super::convert::{
    counter, domain_rejection, new_uuid, now_ms, parse_uuid, preview, rejection, wire_id, wire_ts,
};
use super::handlers::{CapturePurpose, Pending};
use super::jobs::{self, LaunchInput};
use super::project::wire_input;
use super::{JobOutput, ReqCtx, Service};

const STOP_TIMEOUT: Duration = Duration::from_secs(30);
const LIVE_ITEM_RETENTION: Duration = Duration::from_secs(60);

/// Streaming activity held in memory between periodic flushes.
#[derive(Default)]
pub(crate) struct LiveActivity {
    items: HashMap<String, LiveItem>,
    /// Pending coalesced deltas in arrival order: (objective, item, text).
    deltas: Vec<(ObjectiveId, String, String)>,
}

struct LiveItem {
    objective: ObjectiveId,
    row: ActivityRow,
    dirty: bool,
    touched: Instant,
}

fn ts_now() -> Timestamp {
    Timestamp::from_unix_ms(now_ms())
}

fn role_code(role: ActivityRole) -> &'static str {
    match role {
        ActivityRole::User => "user",
        ActivityRole::Assistant => "assistant",
        ActivityRole::Thinking => "thinking",
        ActivityRole::Tool => "tool",
        ActivityRole::System => "system",
    }
}

fn parse_role(code: &str) -> ActivityRole {
    match code {
        "user" => ActivityRole::User,
        "assistant" => ActivityRole::Assistant,
        "thinking" => ActivityRole::Thinking,
        "tool" => ActivityRole::Tool,
        _ => ActivityRole::System,
    }
}

pub(crate) fn wire_activity(row: &ActivityRow) -> Option<ActivityItem> {
    Some(ActivityItem {
        item_id: Id::parse(row.item_id.clone()).ok()?,
        seq: counter(row.seq),
        at: wire_ts(row.at),
        role: parse_role(&row.role),
        title: preview(&row.title, 1024),
        text: row.text.clone(),
        is_error: row.is_error,
        truncated: row.truncated,
        tool_name: row.tool_name.as_ref().map(|t| preview(t, 256)),
    })
}

impl Service {
    // -----------------------------------------------------------------------------------------
    // Commands.

    pub(crate) fn execution_start(
        &mut self,
        ctx: &mut ReqCtx,
        cmd: &ExecutionStartCommand,
    ) -> Result<ReplyData, Rejection> {
        let oid = self.objective_id(&cmd.objective_id)?;
        self.check_idle(oid)?;
        let model =
            ModelRef::new(&cmd.model.provider, &cmd.model.id).map_err(|e| domain_rejection(&e))?;
        let mut st = self.obj(oid)?.clone();
        let session = SessionId::new(new_uuid());
        let generation = st
            .objective_mut()
            .start_execution(session, model, ts_now())
            .map_err(|e| domain_rejection(&e))?;
        self.preflight(&cmd.model.provider)?;
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

    /// Refuses synchronously when the launcher can already tell a worker for `provider` could
    /// not start (sandbox, egress policy, missing credentials — names only, never values).
    fn preflight(&self, provider: &str) -> Result<(), Rejection> {
        self.launcher.preflight(provider).map_err(|e| {
            rejection(
                ErrorCode::ExecutionUnavailable,
                SafeAction::None,
                &e.detail(),
            )
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
            .map_err(|e| domain_rejection(&e))?;
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
        let generation = st
            .objective_mut()
            .resume_execution(ts_now())
            .map_err(|e| domain_rejection(&e))?;
        if let Some(session) = st.objective().execution().session() {
            self.preflight(session.model.provider())?;
        }
        // Resume is the explicit continuation for input blocked only by the stopped execution.
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

    /// Provisions (lazily) and launches the worker of the current generation on the pool.
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
        let provision = st.row.worktree.is_none().then(|| {
            (
                record.branch.as_str().to_string(),
                record.start_commit.clone(),
            )
        });
        let session_dir = self.paths.sessions.join(oid.to_string());
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
        };
        self.rt(oid).launching = Some(generation);
        self.spawn(Pending::Launch { oid, generation }, move || {
            JobOutput::Launch(jobs::launch(input))
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
                if out.provisioned_branch.is_some() && self.commit_obj(None, None, st).is_err() {
                    eprintln!("laded: could not record provisioned worktree objective={oid}");
                }
                if current && state.is_live() {
                    self.rt(oid).control = Some(control);
                    if state == ExecutionState::Stopping {
                        self.stop_worker(oid);
                    }
                } else {
                    self.spawn(Pending::Ignore, move || {
                        let mut control = control;
                        control.terminate();
                        JobOutput::Done
                    });
                }
            }
            Err(reason) => {
                eprintln!("laded: launch failed objective={oid} reason={reason}");
                if current && state.is_live() {
                    let _ = st.objective_mut().observe_failure(
                        lade_core::domain::execution::ExecutionGeneration::new(generation),
                        &reason,
                        ts_now(),
                    );
                    if self.commit_obj(None, None, st).is_err() {
                        eprintln!("laded: could not record launch failure objective={oid}");
                    }
                    self.notice(
                        NoticeLevel::Error,
                        "execution_failed",
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
                    let _ = self.commit_obj(None, None, st);
                }
            }
        }
        self.emit_objective(oid);
    }

    /// Sends `session.abort` then terminates the worker off the service thread; the exit is
    /// observed through the supervisor. Without a worker or launch, the stop completes at once.
    pub(crate) fn stop_worker(&mut self, oid: ObjectiveId) {
        let rt = self.rt(oid);
        if let Some(mut control) = rt.control.take() {
            let _ = control.send(WorkerBody::Abort);
            self.spawn(Pending::Ignore, move || {
                control.terminate();
                drop(control);
                JobOutput::Done
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
            let _ = self.commit_obj(None, None, st);
            self.continue_freeze(oid);
        }
    }

    // -----------------------------------------------------------------------------------------
    // Supervisor events.

    pub(crate) fn on_supervisor(&mut self, event: SupervisorEvent) {
        let Ok(oid) = lade_core::domain::ids::ObjectiveId::parse(&event.binding.objective_id)
        else {
            return;
        };
        let Some(st) = self.objectives.get(&oid) else {
            return;
        };
        let generation = st.objective().execution().generation();
        if generation.value() != event.binding.execution_generation {
            return;
        }
        match event.event {
            WorkerEvent::Body(body) => self.on_worker_body(oid, generation, body),
            WorkerEvent::ProtocolViolation(code) => {
                eprintln!("laded: worker protocol violation objective={oid} code={code}");
                self.notice(NoticeLevel::Error, "worker_protocol_violation", code);
                self.system_item(
                    oid,
                    "Worker link closed",
                    &format!("The worker violated the protocol ({code}); the link was closed."),
                    true,
                );
            }
            WorkerEvent::Exited { code, signal } => self.on_worker_exit(oid, code, signal),
        }
    }

    fn on_worker_body(
        &mut self,
        oid: ObjectiveId,
        generation: lade_core::domain::execution::ExecutionGeneration,
        body: WorkerBody,
    ) {
        match body {
            WorkerBody::Ready {
                pi_session_id,
                session_file,
            } => {
                let Some(mut st) = self.objectives.get(&oid).cloned() else {
                    return;
                };
                if st
                    .objective_mut()
                    .observe_worker_ready(generation, ts_now())
                    .is_err()
                {
                    return;
                }
                let session = st.objective().execution().session().map(|s| s.session_id);
                let committed = self.commit(None, None, |tx| {
                    Self::save_tx(tx, &st)?;
                    if let Some(session) = session {
                        storage::record_pi_session(tx, session, &pi_session_id, &session_file)?;
                    }
                    Ok(())
                });
                if committed.is_ok() {
                    self.objectives.insert(oid, st);
                    self.emit_objective(oid);
                    self.dispatch_queued(oid, None);
                }
            }
            WorkerBody::Received { input_id } => {
                let Ok(uuid) = parse_uuid(&input_id) else {
                    return;
                };
                let id = InputId::new(uuid);
                let Some(mut st) = self.objectives.get(&oid).cloned() else {
                    return;
                };
                let Some(input) = st.input_mut(id) else {
                    return;
                };
                if input.mark_received(generation).is_err() {
                    return;
                }
                let input = input.clone();
                let now = now_ms();
                if self
                    .commit(None, None, |tx| {
                        storage::save_delivery(tx, id, &input.delivery, now)
                    })
                    .is_ok()
                {
                    self.objectives.insert(oid, st);
                    self.emit(Event::InputChanged(wire_input(&input)));
                    self.emit_objective(oid);
                }
            }
            WorkerBody::ActivityItem {
                item_id,
                role,
                title,
                text,
                is_error,
                truncated,
                tool_name,
            } => self.activity_item(
                oid,
                item_id.as_str(),
                role,
                &title,
                &text,
                is_error,
                truncated,
                tool_name.as_deref(),
            ),
            WorkerBody::ActivityDelta {
                item_id,
                append_text,
            } => {
                self.activity_delta(oid, item_id.as_str(), &append_text);
            }
            WorkerBody::TurnState { state } => {
                let running = state == TurnState::Running;
                let Some(mut st) = self.objectives.get(&oid).cloned() else {
                    return;
                };
                if st
                    .objective_mut()
                    .observe_turn(generation, running, ts_now())
                    .is_ok()
                    && self.commit_obj(None, None, st).is_ok()
                {
                    self.emit_objective(oid);
                    if !running {
                        self.request_capture(oid);
                    }
                    self.dispatch_queued(oid, None);
                }
            }
            WorkerBody::Error { code, message } => {
                let code = format!("{code:?}");
                eprintln!("laded: worker error objective={oid} code={code}");
                self.notice(
                    NoticeLevel::Warning,
                    "worker_error",
                    &format!("worker reported {code}"),
                );
                self.system_item(oid, &format!("Worker error ({code})"), &message, true);
            }
            WorkerBody::Start { .. }
            | WorkerBody::Deliver { .. }
            | WorkerBody::Abort
            | WorkerBody::Shutdown => {}
        }
    }

    fn on_worker_exit(&mut self, oid: ObjectiveId, code: Option<i32>, signal: Option<i32>) {
        self.rt(oid).control = None;
        let Some(mut st) = self.objectives.get(&oid).cloned() else {
            return;
        };
        let generation = st.objective().execution().generation();
        let state = st.objective().execution().state().clone();
        let mut lost = Vec::new();
        match state {
            ExecutionState::Stopping => {
                let _ = st.objective_mut().observe_stopped(generation, ts_now());
            }
            state if state.is_live() => {
                let reason = match (code, signal) {
                    (Some(code), _) => format!("worker exited unexpectedly (status {code})"),
                    (None, Some(signal)) => format!("worker terminated by signal {signal}"),
                    _ => "worker exited unexpectedly".to_string(),
                };
                let _ = st
                    .objective_mut()
                    .observe_failure(generation, &reason, ts_now());
                self.notice(NoticeLevel::Error, "execution_failed", &reason);
            }
            _ => {}
        }
        for (_, input) in &mut st.inputs {
            if input.mark_worker_lost() {
                lost.push(input.clone());
            }
        }
        let now = now_ms();
        let committed = self.commit(None, None, |tx| {
            Self::save_tx(tx, &st)?;
            for input in &lost {
                storage::save_delivery(tx, input.id, &input.delivery, now)?;
            }
            Ok(())
        });
        if committed.is_err() {
            eprintln!("laded: could not record worker exit objective={oid}");
            return;
        }
        self.objectives.insert(oid, st);
        for input in &lost {
            self.emit(Event::InputChanged(wire_input(input)));
        }
        self.emit_objective(oid);
        self.continue_freeze(oid);
    }

    // -----------------------------------------------------------------------------------------
    // Dispatch.

    /// Dispatches queued inputs strictly in acceptance order. Feedback inputs are revalidated
    /// against current content first unless `checked` names an input validated a moment ago.
    pub(crate) fn dispatch_queued(&mut self, oid: ObjectiveId, checked: Option<InputId>) {
        if self.rt(oid).dispatching {
            return;
        }
        loop {
            let Some(st) = self.objectives.get(&oid) else {
                return;
            };
            let Some(input) = st
                .inputs
                .iter()
                .map(|(_, i)| i)
                .find(|i| i.delivery == DeliveryState::Queued)
            else {
                return;
            };
            let id = input.id;
            let state = st.objective().execution().state();
            if !state.accepts_dispatch() && *state == ExecutionState::Starting {
                return;
            }
            if !input.items.is_empty() && checked != Some(id) && state.accepts_dispatch() {
                let references = input
                    .items
                    .iter()
                    .map(|item| (item.draft_id, item.reference.clone()))
                    .collect();
                let Some(worktree) = st.row.worktree.clone() else {
                    self.dispatch_one(oid, id, true);
                    continue;
                };
                match self.capture_input(st, worktree, references) {
                    Ok(capture) => {
                        self.rt(oid).dispatching = true;
                        self.spawn(
                            Pending::Capture {
                                oid,
                                purpose: CapturePurpose::DispatchCheck(id),
                            },
                            move || JobOutput::Capture(jobs::capture(&capture)),
                        );
                    }
                    Err(_) => {
                        self.dispatch_one(oid, id, true);
                        continue;
                    }
                }
                return;
            }
            if !self.dispatch_one(oid, id, false) {
                return;
            }
        }
    }

    pub(crate) fn dispatch_checked(&mut self, oid: ObjectiveId, input: InputId, stale: bool) {
        self.rt(oid).dispatching = false;
        if self.dispatch_one(oid, input, stale) {
            self.dispatch_queued(oid, None);
        }
    }

    /// One dispatch decision; returns whether later inputs may proceed.
    fn dispatch_one(&mut self, oid: ObjectiveId, id: InputId, stale: bool) -> bool {
        let Some(mut st) = self.objectives.get(&oid).cloned() else {
            return false;
        };
        let objective = &st.row.objective;
        let Some(input) = st.inputs.iter_mut().map(|(_, i)| i).find(|i| i.id == id) else {
            return true;
        };
        let decision = match input.dispatch(objective, stale) {
            Ok(decision) => decision,
            Err(_) => return true,
        };
        if decision == DispatchDecision::Wait {
            return false;
        }
        let mut input = input.clone();
        let now = now_ms();
        let recorded = self.commit(None, None, |tx| {
            storage::save_delivery(tx, id, &input.delivery, now)
        });
        if recorded.is_err() {
            eprintln!("laded: could not record dispatch input={id}");
            return false;
        }
        if let DispatchDecision::Dispatch { .. } = decision {
            let body = WorkerBody::Deliver {
                input_id: wire_id(id.uuid()),
                mode: super::project::wire_mode(input.mode),
                text: input.rendered_text.clone(),
            };
            let sent = self
                .rt(oid)
                .control
                .as_mut()
                .map(|control| control.send(body));
            if !matches!(sent, Some(Ok(()))) {
                // Never resent automatically: the input is now uncertain.
                input.mark_worker_lost();
                let _ = self.commit(None, None, |tx| {
                    storage::save_delivery(tx, id, &input.delivery, now)
                });
            }
        }
        if let Some(slot) = st.input_mut(id) {
            *slot = input.clone();
        }
        self.objectives.insert(oid, st);
        self.emit(Event::InputChanged(wire_input(&input)));
        self.emit_objective(oid);
        true
    }

    // -----------------------------------------------------------------------------------------
    // Activity.

    fn next_activity_seq(&mut self, oid: ObjectiveId) -> u64 {
        let seq = self.activity_seq.entry(oid).or_insert(0);
        *seq += 1;
        *seq
    }

    #[allow(clippy::too_many_arguments)]
    fn activity_item(
        &mut self,
        oid: ObjectiveId,
        item_id: &str,
        role: ActivityRole,
        title: &str,
        text: &str,
        is_error: bool,
        truncated: bool,
        tool_name: Option<&str>,
    ) {
        let existing = match self.live.items.get(item_id) {
            Some(live) => Some((live.objective, live.row.seq, live.row.at)),
            None => storage::load_activity_item(self.store.conn(), item_id)
                .ok()
                .flatten()
                .map(|(o, row)| (o, row.seq, row.at)),
        };
        let (seq, at) = match existing {
            Some((owner, _, _)) if owner != oid => return,
            Some((_, seq, at)) => (seq, at),
            None => (self.next_activity_seq(oid), now_ms()),
        };
        let (bounded, cut) = truncate_utf8(text, MAX_ITEM_TEXT_BYTES);
        let row = ActivityRow {
            item_id: item_id.to_string(),
            seq,
            at,
            role: role_code(role).to_string(),
            title: preview(title, 1024),
            text: bounded.to_string(),
            tool_name: tool_name.map(|t| preview(t, 256)),
            is_error,
            truncated: truncated || cut,
        };
        if storage::upsert_activity(self.store.conn(), oid, &row).is_err() {
            eprintln!("laded: could not record activity objective={oid}");
        }
        self.live.deltas.retain(|(_, id, _)| id != item_id);
        let wire = wire_activity(&row);
        self.live.items.insert(
            item_id.to_string(),
            LiveItem {
                objective: oid,
                row,
                dirty: false,
                touched: Instant::now(),
            },
        );
        if let Some(item) = wire {
            self.emit(Event::ActivityAppended(ActivityAppended {
                objective_id: wire_id(oid.uuid()),
                items: vec![item],
            }));
        }
    }

    fn activity_delta(&mut self, oid: ObjectiveId, item_id: &str, append: &str) {
        let Some(live) = self.live.items.get_mut(item_id) else {
            return;
        };
        if live.objective != oid || live.row.truncated {
            return;
        }
        let room = MAX_ITEM_TEXT_BYTES.saturating_sub(live.row.text.len());
        let (accepted, cut) = truncate_utf8(append, room);
        live.row.text.push_str(accepted);
        live.row.truncated |= cut;
        live.dirty = true;
        live.touched = Instant::now();
        if accepted.is_empty() {
            return;
        }
        match self.live.deltas.last_mut() {
            Some((o, id, text)) if *o == oid && id == item_id => text.push_str(accepted),
            _ => self
                .live
                .deltas
                .push((oid, item_id.to_string(), accepted.to_string())),
        }
    }

    pub(crate) fn system_item(
        &mut self,
        oid: ObjectiveId,
        title: &str,
        text: &str,
        is_error: bool,
    ) {
        let id = new_uuid().to_string();
        self.activity_item(
            oid,
            &id,
            ActivityRole::System,
            title,
            text,
            is_error,
            false,
            None,
        );
    }

    pub(crate) fn flush_deltas(&mut self) {
        for (oid, item_id, text) in std::mem::take(&mut self.live.deltas) {
            let Ok(item) = Id::parse(item_id) else {
                continue;
            };
            self.emit(Event::ActivityDelta(ActivityDelta {
                objective_id: wire_id(oid.uuid()),
                item_id: item,
                append_text: text,
            }));
        }
    }

    /// Persists streamed text and forgets items that stopped changing.
    pub(crate) fn flush_activity(&mut self) {
        for live in self.live.items.values_mut().filter(|l| l.dirty) {
            if storage::upsert_activity(self.store.conn(), live.objective, &live.row).is_ok() {
                live.dirty = false;
            }
        }
        self.live
            .items
            .retain(|_, l| l.dirty || l.touched.elapsed() < LIVE_ITEM_RETENTION);
    }

    /// Current text of a live item (newer than the store between flushes).
    pub(crate) fn live_text(&self, item_id: &str) -> Option<(&str, bool)> {
        self.live
            .items
            .get(item_id)
            .map(|l| (l.row.text.as_str(), l.row.truncated))
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
                        lade_protocol::client::ErrorCode::Interrupted,
                        lade_protocol::client::SafeAction::Refresh,
                        "the worker did not stop in time; nothing was frozen",
                    )),
                );
            }
        }
    }
}
