//! Events from a running worker: readiness, input receipts, turn state, errors and exit, and
//! the dispatch of queued input to it.

use lade_core::domain::execution::{ExecutionGeneration, ExecutionState};
use lade_core::domain::feedback::{DeliveryState, DispatchDecision};
use lade_core::domain::ids::{InputId, ObjectiveId};
use lade_protocol::client::{Event, NoticeCode, NoticeLevel};
use lade_protocol::worker::{TurnState, WorkerBody};

use crate::storage;
use crate::supervision::{SupervisorEvent, WorkerEvent};

use super::activity::ActivityUpdate;
use super::ts_now;
use crate::service::capture::CapturePurpose;
use crate::service::convert::{parse_uuid, wire_id, wire_input, wire_mode};
use crate::service::jobs;
use crate::service::{Completed, Service};
use crate::support::now_ms;

impl Service {
    pub(crate) fn on_supervisor(&mut self, event: SupervisorEvent) {
        let Ok(oid) = ObjectiveId::parse(&event.binding.objective_id) else {
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
                self.notice(
                    NoticeLevel::Error,
                    NoticeCode::WorkerProtocolViolation,
                    code,
                );
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
        generation: ExecutionGeneration,
        body: WorkerBody,
    ) {
        match body {
            WorkerBody::Ready {
                pi_session_id,
                session_file,
            } => self.worker_ready(oid, generation, &pi_session_id, &session_file),
            WorkerBody::Received { input_id } => {
                if let Ok(uuid) = parse_uuid(&input_id) {
                    self.input_received(oid, generation, InputId::new(uuid));
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
                &ActivityUpdate {
                    item_id: item_id.as_str(),
                    role,
                    title: &title,
                    text: &text,
                    is_error,
                    truncated,
                    tool_name: tool_name.as_deref(),
                },
            ),
            WorkerBody::ActivityDelta {
                item_id,
                append_text,
            } => {
                self.activity_delta(oid, item_id.as_str(), &append_text);
            }
            WorkerBody::TurnState { state } => {
                self.turn_changed(oid, generation, state == TurnState::Running);
            }
            WorkerBody::Error { code, message } => {
                let code = format!("{code:?}");
                eprintln!("laded: worker error objective={oid} code={code}");
                self.notice(
                    NoticeLevel::Warning,
                    NoticeCode::WorkerError,
                    &format!("worker reported {code}"),
                );
                self.system_item(oid, &format!("Worker error ({code})"), &message, true);
            }
            body @ (WorkerBody::SessionChanged { .. }
            | WorkerBody::CommandResult { .. }
            | WorkerBody::ResourcesLoaded { .. }
            | WorkerBody::UiRequest { .. }
            | WorkerBody::UiClosed { .. }) => self.on_agent_session_body(oid, generation, body),
            WorkerBody::Start { .. }
            | WorkerBody::Deliver { .. }
            | WorkerBody::Abort
            | WorkerBody::Shutdown
            | WorkerBody::Command { .. }
            | WorkerBody::UiRespond { .. } => {}
        }
    }

    /// Records that the worker of `generation` is ready, with the pi session it reported, then
    /// dispatches queued input to it.
    fn worker_ready(
        &mut self,
        oid: ObjectiveId,
        generation: ExecutionGeneration,
        pi_session_id: &str,
        session_file: &str,
    ) {
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
                storage::record_pi_session(tx, session, pi_session_id, session_file)?;
            }
            Ok(())
        });
        if Self::persisted("the ready worker", oid, committed) {
            self.objectives.insert(oid, st);
            self.emit_objective(oid);
            self.dispatch_queued(oid, None);
        }
    }

    /// Records the worker's receipt of input `id`.
    fn input_received(&mut self, oid: ObjectiveId, generation: ExecutionGeneration, id: InputId) {
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

    /// Records a turn starting or ending; an ended turn also captures the worktree.
    fn turn_changed(&mut self, oid: ObjectiveId, generation: ExecutionGeneration, running: bool) {
        let Some(mut st) = self.objectives.get(&oid).cloned() else {
            return;
        };
        if st
            .objective_mut()
            .observe_turn(generation, running, ts_now())
            .is_err()
        {
            return;
        }
        let recorded = self.commit_obj(None, None, st);
        if Self::persisted("the turn state", oid, recorded) {
            self.emit_objective(oid);
            if !running {
                self.request_capture(oid);
            }
            self.dispatch_queued(oid, None);
        }
    }

    fn on_worker_exit(&mut self, oid: ObjectiveId, code: Option<i32>, signal: Option<i32>) {
        self.rt(oid).control = None;
        self.end_agent_session(oid);
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
                self.notice(NoticeLevel::Error, NoticeCode::ExecutionFailed, &reason);
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
        if !Self::persisted("the worker exit", oid, committed) {
            return;
        }
        self.objectives.insert(oid, st);
        for input in &lost {
            self.emit(Event::InputChanged(wire_input(input)));
        }
        self.emit_objective(oid);
        self.continue_freeze(oid);
    }

    pub(crate) fn dispatch_queued(&mut self, oid: ObjectiveId, checked: Option<InputId>) {
        if self.rt(oid).dispatching || self.command_pending(oid) {
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
                if let Ok(capture) = self.capture_input(st, worktree, references) {
                    self.rt(oid).dispatching = true;
                    self.spawn(move || Completed::Capture {
                        oid,
                        purpose: CapturePurpose::DispatchCheck(id),
                        result: jobs::capture(&capture),
                    });
                } else {
                    self.dispatch_one(oid, id, true);
                    continue;
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

    fn dispatch_one(&mut self, oid: ObjectiveId, id: InputId, stale: bool) -> bool {
        let Some(mut st) = self.objectives.get(&oid).cloned() else {
            return false;
        };
        let objective = &st.row.objective;
        let Some(input) = st.inputs.iter_mut().map(|(_, i)| i).find(|i| i.id == id) else {
            return true;
        };
        let Ok(decision) = input.dispatch(objective, stale) else {
            return true;
        };
        if decision == DispatchDecision::Wait {
            return false;
        }
        let mut input = input.clone();
        let now = now_ms();
        let recorded = self.commit(None, None, |tx| {
            storage::save_delivery(tx, id, &input.delivery, now)
        });
        if !Self::persisted(&format!("the dispatch of input {id}"), oid, recorded) {
            return false;
        }
        if let DispatchDecision::Dispatch { .. } = decision {
            crate::fault::point("dispatch.recorded");
            let body = WorkerBody::Deliver {
                input_id: wire_id(id.uuid()),
                mode: wire_mode(input.mode),
                text: input.rendered_text.clone(),
            };
            let sent = self
                .rt(oid)
                .control
                .as_mut()
                .map(|control| control.send(body));
            if !matches!(sent, Some(Ok(()))) {
                input.mark_worker_lost();
                let recorded = self.commit(None, None, |tx| {
                    storage::save_delivery(tx, id, &input.delivery, now)
                });
                Self::persisted(&format!("the lost delivery of input {id}"), oid, recorded);
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
}
