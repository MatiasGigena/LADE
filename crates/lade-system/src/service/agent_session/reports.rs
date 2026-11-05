//! The session reports a worker sends back: session changes, command results and loaded
//! resources, and the end of the Agent Session when the worker exits.

use std::fmt::Write as _;

use lade_core::domain::execution::{ExecutionGeneration, ModelRef};
use lade_core::domain::ids::{ObjectiveId, SessionId, Timestamp};
use lade_protocol::client::{
    ErrorCode, Event, Rejection, ReplyData, SafeAction, SessionExport, SessionTree, UiRequest,
};
use lade_protocol::types::{SessionChangeReason, UiCloseReason, WirePath};
use lade_protocol::worker::{LoadStatus, LoadedKind, LoadedResource, ResourceSource, WorkerBody};

use crate::storage;

use super::{CommandResult, Op, OpenUi, PiState, wire_reason};
use crate::service::Service;
use crate::service::convert::{wire_id, wire_path};
use crate::support::{new_uuid, now_ms};

impl Service {
    pub(crate) fn on_agent_session_body(
        &mut self,
        oid: ObjectiveId,
        generation: ExecutionGeneration,
        body: WorkerBody,
    ) {
        match body {
            WorkerBody::SessionChanged {
                generation: pi_generation,
                reason,
                pi_session_id,
                session_file,
                model,
                thinking,
                leaf_id,
            } => self.session_changed(
                oid,
                generation,
                &PiState {
                    generation: pi_generation.value(),
                    reason,
                    pi_session_id,
                    model,
                    thinking,
                    leaf_id,
                },
                &session_file,
            ),
            WorkerBody::CommandResult {
                command_id,
                ok,
                message,
                entries,
                leaf_id,
                truncated,
            } => self.command_result(
                oid,
                CommandResult {
                    command_id,
                    ok,
                    message,
                    entries,
                    leaf_id,
                    truncated,
                },
            ),
            WorkerBody::ResourcesLoaded {
                generation: pi_generation,
                resources,
            } => self.resources_loaded(oid, pi_generation.value(), &resources),
            WorkerBody::UiRequest {
                ui_id,
                generation: pi_generation,
                prompt,
            } => {
                let current = self.live(oid).pi.as_ref().map(|p| p.generation);
                if current.is_some_and(|g| g != pi_generation.value()) {
                    return;
                }
                self.live(oid).ui.insert(
                    ui_id.as_str().to_string(),
                    OpenUi {
                        generation: pi_generation.value(),
                        prompt: prompt.clone(),
                    },
                );
                self.emit(Event::UiRequest(UiRequest {
                    objective_id: wire_id(oid.uuid()),
                    ui_id,
                    generation: pi_generation,
                    prompt,
                }));
            }
            WorkerBody::UiClosed { ui_id, reason } => self.ui_closed(oid, ui_id, reason),
            _ => {}
        }
    }

    fn session_changed(
        &mut self,
        oid: ObjectiveId,
        generation: ExecutionGeneration,
        pi: &PiState,
        session_file: &str,
    ) {
        let reason = pi.reason;
        let pi_generation = pi.generation;
        self.live(oid).pi = Some(pi.clone());
        self.close_dialogs(oid, |g| g >= pi_generation, UiCloseReason::Replaced);
        let Some(mut st) = self.objectives.get(&oid).cloned() else {
            return;
        };
        let now = now_ms();
        if matches!(
            reason,
            SessionChangeReason::Fork | SessionChangeReason::Import
        ) {
            let session = SessionId::new(new_uuid());
            if st
                .objective_mut()
                .rebind_live_session(generation, session, Timestamp::from_unix_ms(now))
                .is_err()
            {
                eprintln!("laded: session replacement refused objective={oid}");
                return;
            }
            let association = st.objective().execution().session().cloned();
            let committed = self.commit(None, None, |tx| {
                Self::save_tx(tx, &st)?;
                if let Some(association) = &association {
                    storage::save_session(tx, oid, association, generation.value(), now)?;
                    storage::record_pi_session(
                        tx,
                        association.session_id,
                        &pi.pi_session_id,
                        session_file,
                    )?;
                }
                Ok(())
            });
            if committed.is_err() {
                eprintln!("laded: could not record session replacement objective={oid}");
                return;
            }
            self.objectives.insert(oid, st);
            self.system_item(
                oid,
                &format!("Session replaced ({})", wire_reason(reason)),
                "The Agent Session now continues in a new pi session. Input and feedback addressed to the previous \
                 session are not delivered to it; no approval, verification or Git state was carried over.",
                false,
            );
            self.emit_objective(oid);
        } else if let Some(session) = st.objective().execution().session().map(|s| s.session_id) {
            let recorded = self.commit(None, None, |tx| {
                storage::record_pi_session(tx, session, &pi.pi_session_id, session_file)
            });
            if recorded.is_err() {
                eprintln!("laded: could not record pi session objective={oid}");
            }
        }
        if reason == SessionChangeReason::Configure
            && let Ok(model) = ModelRef::new(&pi.model.provider, &pi.model.id)
        {
            let _ = self.persist_selection(None, oid, model, pi.thinking, |service| {
                service.session_state(oid, SessionChangeReason::Configure)
            });
        }
        let state = self.session_state(oid, reason);
        self.emit(Event::SessionChanged(state));
    }

    fn command_result(&mut self, oid: ObjectiveId, reply: CommandResult) {
        let CommandResult {
            command_id,
            ok,
            message,
            entries,
            leaf_id,
            truncated,
        } = reply;
        let command_id = command_id.as_str();
        let live = self.live(oid);
        if live.command.as_ref().is_none_or(|c| c.id != command_id) {
            return;
        }
        let Some(pending) = live.command.take() else {
            return;
        };
        let result = if ok {
            Ok(match pending.op {
                Op::Tree => ReplyData::SessionTree(SessionTree {
                    objective_id: wire_id(oid.uuid()),
                    entries: entries.unwrap_or_default().into_iter().take(1024).collect(),
                    truncated: truncated.unwrap_or(false),
                    leaf_id,
                }),
                Op::Export => ReplyData::SessionExport(SessionExport {
                    objective_id: wire_id(oid.uuid()),
                    path: pending
                        .export
                        .as_deref()
                        .and_then(|p| WirePath::from_os_str(p.as_os_str()).ok())
                        .unwrap_or_else(|| wire_path(b"export")),
                }),
                Op::Navigate => ReplyData::SessionNavigate(
                    self.session_state(oid, SessionChangeReason::Navigate),
                ),
                Op::Fork => {
                    ReplyData::SessionFork(self.session_state(oid, SessionChangeReason::Fork))
                }
                Op::Import => {
                    ReplyData::SessionImport(self.session_state(oid, SessionChangeReason::Import))
                }
                Op::Compact => {
                    ReplyData::SessionCompact(self.session_state(oid, SessionChangeReason::Compact))
                }
                Op::Reload => {
                    ReplyData::SessionReload(self.session_state(oid, SessionChangeReason::Reload))
                }
                Op::Configure => ReplyData::SessionConfigure(
                    self.session_state(oid, SessionChangeReason::Configure),
                ),
            })
        } else {
            Err(Rejection::new(
                ErrorCode::Conflict,
                SafeAction::Refresh,
                &format!("The Agent Session refused this: {message}"),
            ))
        };
        self.finish(pending.ctx, result);
        self.dispatch_queued(oid, None);
    }

    fn resources_loaded(
        &mut self,
        oid: ObjectiveId,
        generation: u64,
        resources: &[LoadedResource],
    ) {
        if self.live(oid).reported == Some(generation) {
            return;
        }
        self.live(oid).reported = Some(generation);
        let describe = |kind: LoadedKind| {
            let names: Vec<String> = resources
                .iter()
                .filter(|r| r.kind == kind && r.status == LoadStatus::Loaded)
                .map(|r| match r.source {
                    ResourceSource::LadeProfile => r.name.clone(),
                    ResourceSource::Repository => format!("{} (repository)", r.name),
                })
                .collect();
            if names.is_empty() {
                "none".to_string()
            } else {
                names.join(", ")
            }
        };
        let mut text = format!(
            "Generation {generation}. Context: {}. Skills: {}. Prompt templates: {}. Extensions: {}.",
            describe(LoadedKind::Context),
            describe(LoadedKind::Skill),
            describe(LoadedKind::Prompt),
            describe(LoadedKind::Extension)
        );
        let mut problem = false;
        for refused in resources.iter().filter(|r| r.status != LoadStatus::Loaded) {
            problem = true;
            let _ = write!(
                text,
                "\n{} {} was {}: {}",
                match refused.kind {
                    LoadedKind::Extension => "Extension",
                    LoadedKind::Skill => "Skill",
                    LoadedKind::Prompt => "Prompt template",
                    LoadedKind::Context => "Context file",
                },
                refused.name,
                if refused.status == LoadStatus::Refused {
                    "refused"
                } else {
                    "not loaded"
                },
                refused.message.as_deref().unwrap_or("no detail")
            );
        }
        self.system_item(oid, "Session resources", &text, problem);
    }

    pub(crate) fn end_agent_session(&mut self, oid: ObjectiveId) {
        let pending = self.live(oid).command.take();
        if let Some(pending) = pending {
            self.finish(
                pending.ctx,
                Err(Rejection::new(
                    ErrorCode::Interrupted,
                    SafeAction::Refresh,
                    "The agent stopped before the session operation finished.",
                )),
            );
        }
        self.close_dialogs(oid, |_| false, UiCloseReason::Aborted);
        self.live(oid).pi = None;
        self.live(oid).reported = None;
    }
}
