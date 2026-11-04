//! Agent Session control on top of a running worker: session commands (tree, navigate, fork,
//! import, export, compact, reload, configure), extension dialogs, model and thinking-level
//! selection, and the session reports the worker sends back.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use lade_core::domain::ids::ObjectiveId;
use lade_protocol::client::{Rejection, SessionState};
use lade_protocol::types::{
    Counter, Id, Model, SessionChangeReason, ThinkingLevel, TreeEntry, UiPrompt,
};

use crate::storage;

use super::convert::{preview, wire_id};
use super::handlers::sql;
use super::{ReqCtx, Service, StartError};
use crate::support::now_ms;

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

/// A worker's answer to a session command, as it arrived in `command.result`.
struct CommandResult {
    command_id: Id,
    ok: bool,
    message: String,
    entries: Option<Vec<TreeEntry>>,
    leaf_id: Option<String>,
    truncated: Option<bool>,
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

mod commands;
mod dialogs;
mod reports;
#[cfg(test)]
mod tests;

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
            generation: Counter::new(pi.as_ref().map_or(0, |p| p.generation)),
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
                .map(|s| Counter::new(s.association_revision.value())),
            pi_session_id: pi
                .as_ref()
                .map(|p| preview(&p.pi_session_id, 256))
                .filter(|p| !p.is_empty()),
            leaf_id: pi.and_then(|p| p.leaf_id),
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
