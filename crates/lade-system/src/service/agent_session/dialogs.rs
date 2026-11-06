//! Extension dialogs the worker opens: answering them, re-announcing them to a new client,
//! and closing them when they are answered, replaced or aborted.

use lade_core::domain::ids::ObjectiveId;
use lade_protocol::client::{
    Empty, ErrorCode, Event, Rejection, ReplyData, SafeAction, UiClosed, UiRequest,
    UiRespondCommand,
};
use lade_protocol::types::{Counter, Id, UiCloseReason};
use lade_protocol::worker::WorkerBody;

use crate::service::convert::wire_id;
use crate::service::{ReqCtx, Service};

impl Service {
    pub(crate) fn ui_respond(
        &mut self,
        ctx: &mut ReqCtx,
        cmd: UiRespondCommand,
    ) -> Result<ReplyData, Rejection> {
        let oid = self.objective_id(&cmd.objective_id)?;
        let ui_id = cmd.ui_id.as_str().to_string();
        let live = self.live(oid);
        let current = live.pi.as_ref().map(|p| p.generation);
        let Some(open) = live.ui.get(&ui_id) else {
            return Err(Rejection::new(
                ErrorCode::NotFound,
                SafeAction::None,
                "This dialog was already answered, closed or replaced.",
            ));
        };
        if open.generation != cmd.generation.value() || current != Some(open.generation) {
            return Err(Rejection::new(
                ErrorCode::StaleExecution,
                SafeAction::Refresh,
                "This dialog belongs to an earlier session and can no longer be answered.",
            ));
        }
        let body = WorkerBody::UiRespond {
            ui_id: cmd.ui_id.clone(),
            generation: cmd.generation.clone(),
            answer: cmd.answer,
        };
        let sent = self
            .rt(oid)
            .control
            .as_mut()
            .map(|control| control.send(body));
        if !matches!(sent, Some(Ok(()))) {
            return Err(Rejection::new(
                ErrorCode::ExecutionUnavailable,
                SafeAction::None,
                "The agent stopped unexpectedly. Resume it and try again.",
            ));
        }
        let data = ReplyData::UiRespond(Empty {});
        self.commit(Some(ctx), Some(&data), |_| Ok(()))?;
        self.live(oid).ui.remove(&ui_id);
        self.emit(Event::UiClosed(UiClosed {
            objective_id: wire_id(oid.uuid()),
            ui_id: cmd.ui_id,
            reason: UiCloseReason::Answered,
        }));
        Ok(data)
    }

    pub(crate) fn announce_dialogs(&mut self) {
        let mut open = Vec::new();
        for (oid, live) in &self.agent_sessions.live {
            for (ui_id, dialog) in &live.ui {
                if let Ok(id) = Id::parse(ui_id.clone()) {
                    open.push(UiRequest {
                        objective_id: wire_id(oid.uuid()),
                        ui_id: id,
                        generation: Counter::new(dialog.generation),
                        prompt: dialog.prompt.clone(),
                    });
                }
            }
        }
        for request in open {
            self.emit(Event::UiRequest(request));
        }
    }

    pub(super) fn close_dialogs(
        &mut self,
        oid: ObjectiveId,
        keep: impl Fn(u64) -> bool,
        reason: UiCloseReason,
    ) {
        let live = self.live(oid);
        let closed: Vec<String> = live
            .ui
            .iter()
            .filter(|(_, d)| !keep(d.generation))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &closed {
            live.ui.remove(id);
        }
        for id in closed {
            if let Ok(ui_id) = Id::parse(id) {
                self.emit(Event::UiClosed(UiClosed {
                    objective_id: wire_id(oid.uuid()),
                    ui_id,
                    reason,
                }));
            }
        }
    }

    pub(super) fn ui_closed(&mut self, oid: ObjectiveId, ui_id: Id, reason: UiCloseReason) {
        if self.live(oid).ui.remove(ui_id.as_str()).is_some() {
            self.emit(Event::UiClosed(UiClosed {
                objective_id: wire_id(oid.uuid()),
                ui_id,
                reason,
            }));
        }
    }
}
