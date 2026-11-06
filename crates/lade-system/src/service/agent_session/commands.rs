//! Session commands sent to a running worker (tree, navigate, fork, import, export, compact,
//! reload, configure), their preparation and timeout, and the model and thinking-level
//! selection they persist.

use std::fs;
use std::io::{ErrorKind, Write as _};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use lade_core::domain::execution::{ExecutionState, ModelRef};
use lade_core::domain::ids::ObjectiveId;
use lade_protocol::client::{
    ErrorCode, NoticeCode, NoticeLevel, Rejection, ReplyData, SafeAction, SessionCompactCommand,
    SessionConfigureCommand, SessionForkCommand, SessionImportCommand, SessionNavigateCommand,
    SessionState,
};
use lade_protocol::types::{Id, SessionChangeReason, ThinkingLevel};
use lade_protocol::worker::{SessionCommand, WorkerBody, WorkerResources};
use serde_json::Value;

use crate::storage;
use crate::support::fs::ensure_private_dir;

use super::{COMMAND_TIMEOUT, MAX_IMPORT_BYTES, Op, PendingCommand};
use crate::service::convert::{
    conflict, domain_rejection, durability_failure, invalid, io_rejection, not_found, wire_id,
};
use crate::service::resources::snapshot;
use crate::service::state::Busy;
use crate::service::{Completed, Flow, ReqCtx, Service};
use crate::support::{new_uuid, now_ms, ts_now};

impl Service {
    fn command_target(&mut self, oid: ObjectiveId, op: Op) -> Result<(), Rejection> {
        self.check_idle(oid)?;
        if self.command_pending(oid) {
            return Err(conflict(
                "Another session operation is in progress. Wait for it to finish.",
            ));
        }
        let st = self.obj(oid)?;
        let state = st.row.objective.execution().state();
        let ready = match op {
            Op::Tree | Op::Export => {
                matches!(state, ExecutionState::Idle | ExecutionState::Running)
            }
            _ => *state == ExecutionState::Idle,
        };
        if !ready || self.runtime.get(&oid).is_none_or(|rt| rt.control.is_none()) {
            return Err(Rejection::new(
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
                Err(Rejection::new(
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
            Err(rejection) => self.reject(ctx, rejection),
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
            Err(rejection) => self.reject(ctx, rejection),
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
            Err(rejection) => self.reject(ctx, rejection),
        }
    }

    pub(crate) fn session_reload(&mut self, ctx: ReqCtx, id: &Id) -> Flow {
        let prepared = self
            .objective_id(id)
            .and_then(|oid| self.command_target(oid, Op::Reload).map(|()| oid));
        let oid = match prepared {
            Ok(oid) => oid,
            Err(rejection) => return self.reject(ctx, rejection),
        };
        let source = self.profile_dir();
        let dest = self.session_dir(oid).join("profile");
        let approved = self.approved_extensions();
        self.rt(oid).busy = Some(Busy::SessionReload);
        self.spawn(move || Completed::Reload {
            oid,
            ctx,
            result: snapshot(&source, &dest, &approved)
                .map(|resources| (resources, dest))
                .map_err(io_rejection(
                    "The profile could not be prepared for the session",
                )),
        });
        Flow::Deferred
    }

    pub(crate) fn reload_snapshot_done(
        &mut self,
        oid: ObjectiveId,
        ctx: ReqCtx,
        result: Result<(WorkerResources, PathBuf), Rejection>,
    ) {
        self.rt(oid).busy = None;
        match result {
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
            let model =
                ModelRef::new(&cmd.model.provider, &cmd.model.id).map_err(domain_rejection)?;
            let st = self.obj(oid)?;
            let session = st
                .row
                .objective
                .execution()
                .session()
                .cloned()
                .ok_or_else(|| domain_rejection(lade_core::DomainError::ExecutionUnavailable))?;
            let live = st.row.objective.execution().state().is_live();
            if live && session.model.provider() != model.provider() {
                return Err(invalid(
                    "Changing the provider needs a restart: stop the agent, change the model, then resume it.",
                ));
            }
            Ok((oid, model, live))
        })();
        let (oid, model, live) = match prepared {
            Ok(found) => found,
            Err(rejection) => return self.reject(ctx, rejection),
        };
        if live {
            if let Err(rejection) = self.command_target(oid, Op::Configure) {
                return self.reject(ctx, rejection);
            }
            let command = SessionCommand::Configure {
                model: cmd.model,
                thinking: cmd.thinking,
            };
            return self.send_command(ctx, oid, Op::Configure, command, None);
        }
        if let Err(rejection) = self.preflight(model.provider()) {
            return self.reject(ctx, rejection);
        }
        let result = self.persist_selection(Some(&mut ctx), oid, model, cmd.thinking, |service| {
            service.session_state(oid, SessionChangeReason::Configure)
        });
        self.finish(ctx, result);
        Flow::Done
    }

    pub(super) fn persist_selection(
        &mut self,
        ctx: Option<&mut ReqCtx>,
        oid: ObjectiveId,
        model: ModelRef,
        thinking: ThinkingLevel,
        reply: impl FnOnce(&Self) -> SessionState,
    ) -> Result<ReplyData, Rejection> {
        let mut st = self.obj(oid)?.clone();
        st.row
            .objective
            .select_model(model, ts_now())
            .map_err(domain_rejection)?;
        // `reply` reads the selected model and thinking level back from the service, and its
        // data is journaled in the same commit, so both change first and are restored if the
        // commit fails.
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
                    Err(Rejection::new(
                        ErrorCode::Interrupted,
                        SafeAction::Refresh,
                        "The session operation didn't finish in time.",
                    )),
                );
                self.notice(
                    NoticeLevel::Warning,
                    NoticeCode::SessionCommandTimeout,
                    "A session operation timed out.",
                );
                self.dispatch_queued(oid, None);
            }
        }
    }
}

fn copy_import(source: &Path, copy: &Path) -> Result<(), Rejection> {
    if !source.is_absolute() {
        return Err(invalid("Choose the session file by its full path."));
    }
    let unreadable = |e: std::io::Error| match e.kind() {
        ErrorKind::NotFound => not_found("session file"),
        ErrorKind::PermissionDenied => Rejection::new(
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
        durability_failure("The session could not be copied for import. Try again.")
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
        .is_ok_and(|v| v.get("type").and_then(Value::as_str) == Some("session"));
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
