//! Agent Session control on top of a running worker: session commands (tree, navigate, fork,
//! import, export, compact, reload, configure), extension dialogs, model and thinking-level
//! selection, and the session reports the worker sends back.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{ErrorKind, Write as _};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lade_core::domain::execution::{ExecutionGeneration, ExecutionState, ModelRef};
use lade_core::domain::ids::{ObjectiveId, SessionId, Timestamp};
use lade_protocol::client::{
    Empty, ErrorCode, Event, NoticeLevel, Rejection, ReplyData, SafeAction, SessionCompactCommand,
    SessionConfigureCommand, SessionExport, SessionForkCommand, SessionImportCommand,
    SessionNavigateCommand, SessionState, SessionTree, UiClosed, UiRequest, UiRespondCommand,
};
use lade_protocol::types::{
    Id, Model, SessionChangeReason, ThinkingLevel, TreeEntry, UiCloseReason, UiPrompt, WirePath,
};
use lade_protocol::worker::{
    LoadStatus, LoadedKind, LoadedResource, ResourceSource, SessionCommand, WorkerBody,
    WorkerResources,
};
use serde_json::Value;

use crate::storage;
use crate::support::fs::ensure_private_dir;

use super::convert::{
    conflict, counter, domain_rejection, durability, invalid, not_found, preview, rejection,
    wire_id,
};
use super::handlers::{Pending, sql};
use super::resources::snapshot;
use super::{Flow, JobOutput, ReqCtx, Service, StartError};
use crate::support::{new_uuid, now_ms};

pub(crate) const COMMAND_TIMEOUT: Duration = Duration::from_secs(180);
pub(crate) const MAX_IMPORT_BYTES: u64 = 16 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    Tree,
    Navigate,
    Fork,
    Import,
    Export,
    Compact,
    Reload,
    Configure,
}

pub(crate) struct PendingCommand {
    pub id: String,
    pub ctx: ReqCtx,
    pub op: Op,
    pub since: Instant,
    pub export: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub(crate) struct PiState {
    pub generation: u64,
    pub reason: SessionChangeReason,
    pub pi_session_id: String,
    pub model: Model,
    pub thinking: ThinkingLevel,
    pub leaf_id: Option<String>,
}

pub(crate) struct OpenUi {
    pub generation: u64,
    pub prompt: UiPrompt,
}

#[derive(Default)]
pub(crate) struct LiveSession {
    pub command: Option<PendingCommand>,
    pub pi: Option<PiState>,
    pub ui: BTreeMap<String, OpenUi>,
    pub reported: Option<u64>,
}

/// Per-Objective Agent Session state: the persisted thinking-level preference and the live
/// pi session (pending command, open dialogs, last reported resources).
#[derive(Default)]
pub(crate) struct AgentSessions {
    pub thinking: HashMap<ObjectiveId, ThinkingLevel>,
    pub live: HashMap<ObjectiveId, LiveSession>,
}

fn wire_reason(reason: SessionChangeReason) -> &'static str {
    match reason {
        SessionChangeReason::Start => "start",
        SessionChangeReason::Navigate => "navigate",
        SessionChangeReason::Fork => "fork",
        SessionChangeReason::Import => "import",
        SessionChangeReason::Reload => "reload",
        SessionChangeReason::Compact => "compact",
        SessionChangeReason::Configure => "configure",
    }
}

impl Service {
    pub(crate) fn load_agent_sessions(&mut self) -> Result<(), StartError> {
        let thinking = storage::load_thinking(self.store.conn())?;
        self.agent_sessions.thinking.extend(thinking);
        Ok(())
    }

    fn live(&mut self, oid: ObjectiveId) -> &mut LiveSession {
        self.agent_sessions.live.entry(oid).or_default()
    }

    pub(crate) fn thinking_of(&self, oid: ObjectiveId) -> Option<ThinkingLevel> {
        self.agent_sessions.thinking.get(&oid).copied()
    }

    pub(crate) fn command_pending(&self, oid: ObjectiveId) -> bool {
        self.agent_sessions
            .live
            .get(&oid)
            .is_some_and(|l| l.command.is_some())
    }

    fn command_target(&mut self, oid: ObjectiveId, op: Op) -> Result<(), Rejection> {
        self.check_idle(oid)?;
        if self.command_pending(oid) {
            return Err(conflict(
                "Another session operation is in progress. Wait for it to finish.",
            ));
        }
        let st = self.obj(oid)?;
        let state = st.objective().execution().state();
        let ready = match op {
            Op::Tree | Op::Export => {
                matches!(state, ExecutionState::Idle | ExecutionState::Running)
            }
            _ => *state == ExecutionState::Idle,
        };
        if !ready || self.runtime.get(&oid).is_none_or(|rt| rt.control.is_none()) {
            return Err(rejection(
                ErrorCode::ExecutionUnavailable,
                SafeAction::None,
                if state.is_live() {
                    "Wait until the agent finishes its current turn."
                } else {
                    "Start or resume the agent first."
                },
            ));
        }
        Ok(())
    }

    fn send_command(
        &mut self,
        ctx: ReqCtx,
        oid: ObjectiveId,
        op: Op,
        command: SessionCommand,
        export: Option<PathBuf>,
    ) -> Flow {
        let id = new_uuid();
        let body = WorkerBody::Command {
            command_id: wire_id(id),
            command,
        };
        let sent = self
            .rt(oid)
            .control
            .as_mut()
            .map(|control| control.send(body));
        if !matches!(sent, Some(Ok(()))) {
            self.finish(
                ctx,
                Err(rejection(
                    ErrorCode::ExecutionUnavailable,
                    SafeAction::None,
                    "The agent stopped unexpectedly. Resume it and try again.",
                )),
            );
            return Flow::Done;
        }
        self.live(oid).command = Some(PendingCommand {
            id: id.to_string(),
            ctx,
            op,
            since: Instant::now(),
            export,
        });
        Flow::Deferred
    }

    fn command(&mut self, ctx: ReqCtx, id: &Id, op: Op, command: SessionCommand) -> Flow {
        let prepared = self
            .objective_id(id)
            .and_then(|oid| self.command_target(oid, op).map(|()| oid));
        match prepared {
            Ok(oid) => self.send_command(ctx, oid, op, command, None),
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                Flow::Done
            }
        }
    }

    pub(crate) fn session_tree(&mut self, ctx: ReqCtx, id: &Id) -> Flow {
        self.command(ctx, id, Op::Tree, SessionCommand::Tree)
    }

    pub(crate) fn session_navigate(&mut self, ctx: ReqCtx, cmd: SessionNavigateCommand) -> Flow {
        let command = SessionCommand::Navigate {
            entry_id: cmd.entry_id,
        };
        self.command(ctx, &cmd.objective_id, Op::Navigate, command)
    }

    pub(crate) fn session_fork(&mut self, ctx: ReqCtx, cmd: SessionForkCommand) -> Flow {
        let command = SessionCommand::Fork {
            entry_id: cmd.entry_id,
            position: cmd.position,
        };
        self.command(ctx, &cmd.objective_id, Op::Fork, command)
    }

    pub(crate) fn session_compact(&mut self, ctx: ReqCtx, cmd: SessionCompactCommand) -> Flow {
        let command = SessionCommand::Compact {
            instructions: cmd.instructions,
        };
        self.command(ctx, &cmd.objective_id, Op::Compact, command)
    }

    fn session_dir(&self, oid: ObjectiveId) -> PathBuf {
        self.paths.sessions.join(oid.to_string())
    }

    pub(crate) fn session_export(&mut self, ctx: ReqCtx, id: &Id) -> Flow {
        let prepared = self
            .objective_id(id)
            .and_then(|oid| self.command_target(oid, Op::Export).map(|()| oid));
        match prepared {
            Ok(oid) => {
                let path = self
                    .session_dir(oid)
                    .join("exports")
                    .join(format!("session-{}.jsonl", now_ms()));
                let command = SessionCommand::Export {
                    path: path.to_string_lossy().into_owned(),
                };
                self.send_command(ctx, oid, Op::Export, command, Some(path))
            }
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                Flow::Done
            }
        }
    }

    pub(crate) fn session_import(&mut self, ctx: ReqCtx, cmd: &SessionImportCommand) -> Flow {
        let prepared = (|| {
            let oid = self.objective_id(&cmd.objective_id)?;
            self.command_target(oid, Op::Import)?;
            let source = PathBuf::from(cmd.path.to_os_string());
            let copy = self
                .session_dir(oid)
                .join("imports")
                .join(format!("{}.jsonl", new_uuid()));
            copy_import(&source, &copy)?;
            Ok((oid, copy))
        })();
        match prepared {
            Ok((oid, copy)) => {
                let command = SessionCommand::Import {
                    path: copy.to_string_lossy().into_owned(),
                };
                self.send_command(ctx, oid, Op::Import, command, None)
            }
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                Flow::Done
            }
        }
    }

    pub(crate) fn session_reload(&mut self, ctx: ReqCtx, id: &Id) -> Flow {
        let prepared = self
            .objective_id(id)
            .and_then(|oid| self.command_target(oid, Op::Reload).map(|()| oid));
        let oid = match prepared {
            Ok(oid) => oid,
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                return Flow::Done;
            }
        };
        let source = self.profile_dir();
        let dest = self.session_dir(oid).join("profile");
        let approved = self.approved_extensions();
        self.rt(oid).busy = Some("session reload");
        self.spawn(Pending::Reload { oid, ctx }, move || {
            JobOutput::Snapshot(
                snapshot(&source, &dest, &approved)
                    .map(|resources| (resources, dest))
                    .map_err(|e| {
                        format!(
                            "The profile could not be prepared for the session ({}).",
                            e.kind()
                        )
                    }),
            )
        });
        Flow::Deferred
    }

    pub(crate) fn reload_snapshot_done(
        &mut self,
        oid: ObjectiveId,
        ctx: ReqCtx,
        result: Result<(WorkerResources, PathBuf), String>,
    ) {
        self.rt(oid).busy = None;
        match result
            .map_err(|why| rejection(ErrorCode::DurabilityFailure, SafeAction::Refresh, &why))
        {
            Ok((resources, _)) => {
                if let Err(rejection) = self.command_target(oid, Op::Reload) {
                    self.finish(ctx, Err(rejection));
                    return;
                }
                let _ = self.send_command(
                    ctx,
                    oid,
                    Op::Reload,
                    SessionCommand::Reload { resources },
                    None,
                );
            }
            Err(rejection) => self.finish(ctx, Err(rejection)),
        }
    }

    pub(crate) fn session_configure(
        &mut self,
        mut ctx: ReqCtx,
        cmd: SessionConfigureCommand,
    ) -> Flow {
        let prepared = (|| {
            let oid = self.objective_id(&cmd.objective_id)?;
            let model = ModelRef::new(&cmd.model.provider, &cmd.model.id)
                .map_err(|e| domain_rejection(&e))?;
            let st = self.obj(oid)?;
            let session = st
                .objective()
                .execution()
                .session()
                .cloned()
                .ok_or_else(|| domain_rejection(&lade_core::DomainError::ExecutionUnavailable))?;
            let live = st.objective().execution().state().is_live();
            if live && session.model.provider() != model.provider() {
                return Err(invalid(
                    "Changing the provider needs a restart: stop the agent, change the model, then resume it.",
                ));
            }
            Ok((oid, model, live))
        })();
        let (oid, model, live) = match prepared {
            Ok(found) => found,
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                return Flow::Done;
            }
        };
        if live {
            if let Err(rejection) = self.command_target(oid, Op::Configure) {
                self.finish(ctx, Err(rejection));
                return Flow::Done;
            }
            let command = SessionCommand::Configure {
                model: cmd.model,
                thinking: cmd.thinking,
            };
            return self.send_command(ctx, oid, Op::Configure, command, None);
        }
        if let Err(rejection) = self.preflight(model.provider()) {
            self.finish(ctx, Err(rejection));
            return Flow::Done;
        }
        let result = self.persist_selection(Some(&mut ctx), oid, model, cmd.thinking, |service| {
            service.session_state(oid, SessionChangeReason::Configure)
        });
        self.finish(ctx, result);
        Flow::Done
    }

    fn persist_selection(
        &mut self,
        ctx: Option<&mut ReqCtx>,
        oid: ObjectiveId,
        model: ModelRef,
        thinking: ThinkingLevel,
        reply: impl FnOnce(&Self) -> SessionState,
    ) -> Result<ReplyData, Rejection> {
        let mut st = self.obj(oid)?.clone();
        st.objective_mut()
            .select_model(model, Timestamp::from_unix_ms(now_ms()))
            .map_err(|e| domain_rejection(&e))?;
        let previous_model = self.objectives.insert(oid, st.clone());
        let previous = self.agent_sessions.thinking.insert(oid, thinking);
        let data = ReplyData::SessionConfigure(reply(self));
        let now = now_ms();
        let committed = self.commit(ctx, Some(&data), |tx| {
            Self::save_tx(tx, &st)?;
            storage::save_thinking(tx, oid, thinking, now)
        });
        if let Err(rejection) = committed {
            match previous {
                Some(level) => self.agent_sessions.thinking.insert(oid, level),
                None => self.agent_sessions.thinking.remove(&oid),
            };
            if let Some(old) = previous_model {
                self.objectives.insert(oid, old);
            }
            return Err(rejection);
        }
        self.emit_objective(oid);
        Ok(data)
    }

    pub(crate) fn session_state(
        &self,
        oid: ObjectiveId,
        reason: SessionChangeReason,
    ) -> SessionState {
        let st = self.objectives.get(&oid);
        let session = st.and_then(|st| st.objective().execution().session().cloned());
        let pi = self
            .agent_sessions
            .live
            .get(&oid)
            .and_then(|l| l.pi.clone());
        let model = session
            .as_ref()
            .map(|s| Model {
                provider: preview(s.model.provider(), 128),
                id: preview(s.model.id(), 256),
            })
            .or_else(|| pi.as_ref().map(|p| p.model.clone()))
            .unwrap_or_else(|| Model {
                provider: String::new(),
                id: String::new(),
            });
        SessionState {
            objective_id: wire_id(oid.uuid()),
            generation: counter(pi.as_ref().map_or(0, |p| p.generation)),
            reason,
            model,
            thinking: pi
                .as_ref()
                .map(|p| p.thinking)
                .or_else(|| self.thinking_of(oid))
                .unwrap_or(ThinkingLevel::Off),
            session_id: session.as_ref().map(|s| wire_id(s.session_id.uuid())),
            association_revision: session
                .as_ref()
                .map(|s| counter(s.association_revision.value())),
            pi_session_id: pi
                .as_ref()
                .map(|p| preview(&p.pi_session_id, 256))
                .filter(|p| !p.is_empty()),
            leaf_id: pi.and_then(|p| p.leaf_id),
        }
    }

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
            return Err(rejection(
                ErrorCode::NotFound,
                SafeAction::None,
                "This dialog was already answered, closed or replaced.",
            ));
        };
        if open.generation != cmd.generation.value() || current != Some(open.generation) {
            return Err(rejection(
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
            return Err(rejection(
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
                        generation: counter(dialog.generation),
                        prompt: dialog.prompt.clone(),
                    });
                }
            }
        }
        for request in open {
            self.emit(Event::UiRequest(request));
        }
    }

    fn close_dialogs(
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
                PiState {
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
                command_id.as_str(),
                ok,
                &message,
                entries,
                leaf_id,
                truncated,
            ),
            WorkerBody::ResourcesLoaded {
                generation: pi_generation,
                resources,
            } => self.resources_loaded(oid, pi_generation.value(), resources),
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

    fn ui_closed(&mut self, oid: ObjectiveId, ui_id: Id, reason: UiCloseReason) {
        if self.live(oid).ui.remove(ui_id.as_str()).is_some() {
            self.emit(Event::UiClosed(UiClosed {
                objective_id: wire_id(oid.uuid()),
                ui_id,
                reason,
            }));
        }
    }

    fn session_changed(
        &mut self,
        oid: ObjectiveId,
        generation: ExecutionGeneration,
        pi: PiState,
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

    #[allow(clippy::too_many_arguments)]
    fn command_result(
        &mut self,
        oid: ObjectiveId,
        command_id: &str,
        ok: bool,
        message: &str,
        entries: Option<Vec<TreeEntry>>,
        leaf_id: Option<String>,
        truncated: Option<bool>,
    ) {
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
                        .unwrap_or_else(|| super::convert::wire_path(b"export")),
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
            Err(rejection(
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
        resources: Vec<LoadedResource>,
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
            text.push_str(&format!(
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
            ));
        }
        self.system_item(oid, "Session resources", &text, problem);
    }

    pub(crate) fn end_agent_session(&mut self, oid: ObjectiveId) {
        let pending = self.live(oid).command.take();
        if let Some(pending) = pending {
            self.finish(
                pending.ctx,
                Err(rejection(
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

    pub(crate) fn expire_session_commands(&mut self) {
        let expired: Vec<ObjectiveId> = self
            .agent_sessions
            .live
            .iter()
            .filter(|(_, l)| {
                l.command
                    .as_ref()
                    .is_some_and(|c| c.since.elapsed() > COMMAND_TIMEOUT)
            })
            .map(|(oid, _)| *oid)
            .collect();
        for oid in expired {
            if let Some(pending) = self.live(oid).command.take() {
                self.finish(
                    pending.ctx,
                    Err(rejection(
                        ErrorCode::Interrupted,
                        SafeAction::Refresh,
                        "The session operation didn't finish in time.",
                    )),
                );
                self.notice(
                    NoticeLevel::Warning,
                    "session_command_timeout",
                    "A session operation timed out.",
                );
                self.dispatch_queued(oid, None);
            }
        }
    }

    pub(crate) fn start_extras(&self, oid: ObjectiveId) -> (Option<ThinkingLevel>, Option<String>) {
        let session = self
            .objectives
            .get(&oid)
            .and_then(|st| st.objective().execution().session().map(|s| s.session_id));
        let file = session
            .and_then(|s| {
                storage::pi_session_file(self.store.conn(), s)
                    .ok()
                    .flatten()
            })
            .filter(|f| !f.is_empty());
        (self.thinking_of(oid), file)
    }

    pub(crate) fn record_start_thinking(
        &mut self,
        oid: ObjectiveId,
        thinking: ThinkingLevel,
    ) -> Result<(), Rejection> {
        let now = now_ms();
        sql(storage::save_thinking(
            self.store.conn(),
            oid,
            thinking,
            now,
        ))?;
        self.agent_sessions.thinking.insert(oid, thinking);
        Ok(())
    }
}

fn copy_import(source: &Path, copy: &Path) -> Result<(), Rejection> {
    if !source.is_absolute() {
        return Err(invalid("Choose the session file by its full path."));
    }
    let unreadable = |e: std::io::Error| match e.kind() {
        ErrorKind::NotFound => not_found("session file"),
        ErrorKind::PermissionDenied => rejection(
            ErrorCode::PermissionDenied,
            SafeAction::None,
            "LADE is not allowed to read this session file.",
        ),
        kind => {
            eprintln!("laded: session import source unreadable: {kind}");
            invalid("The session file could not be read.")
        }
    };
    let not_copied = |e: std::io::Error| {
        eprintln!("laded: session import copy failed: {}", e.kind());
        durability("The session could not be copied for import. Try again.")
    };
    let meta = fs::metadata(source).map_err(unreadable)?;
    if !meta.is_file() || meta.len() > MAX_IMPORT_BYTES {
        return Err(invalid(
            "The session file must be a regular file of at most 16 MiB.",
        ));
    }
    let bytes = fs::read(source).map_err(unreadable)?;
    let header = bytes.split(|b| *b == b'\n').next().unwrap_or_default();
    let is_session = serde_json::from_slice::<Value>(header)
        .ok()
        .is_some_and(|v| v.get("type").and_then(Value::as_str) == Some("session"));
    if !is_session {
        return Err(invalid(
            "This isn't a pi session file (its first line must be a session header).",
        ));
    }
    let dir = copy.parent().ok_or_else(|| invalid("import directory"))?;
    ensure_private_dir(dir).map_err(not_copied)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(copy)
        .map_err(not_copied)?;
    file.write_all(&bytes).map_err(not_copied)
}
