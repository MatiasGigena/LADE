//! The connection's protocol state, without I/O: request/reply correlation, operation identity
//! retention and recovery, event-sequence tracking and reconnect re-queries.
//!
//! Every mutating user action gets one `operation_id` when the intent arrives, kept with the
//! request until a reply settles it. A reply lost to a disconnect is never answered by sending a
//! fresh operation: after reconnect the core asks `operation.get` for that same id.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use lade_protocol::client::{
    Event, EventFrame, OperationGetQuery, OperationState, OperationStatus, Outcome, Rejection,
    Reply, ReplyData, ReplyResult, Request, RequestFrame,
};
use lade_protocol::types::Id;

use super::convert;
use super::requests::{self, Context, Effects};
use crate::presentation::intents::Intent;
use crate::presentation::model::{ConnectionState, NoticeLevel, ObjectiveId, ServiceNotice};
use crate::presentation::store::StoreUpdate;

/// Queries remembered while disconnected (re-sent on reconnect; oldest dropped beyond this).
const MAX_DEFERRED_QUERIES: usize = 64;
/// How many times a still-running operation is re-checked (one check per [`Core::tick`]).
const MAX_RECHECKS: u32 = 60;

/// A mutating request with its retained identity.
#[derive(Clone, Debug, PartialEq)]
pub struct Operation {
    pub operation_id: Id,
    pub request: Request,
    pub context: Context,
    checks: u32,
}

#[derive(Clone, Debug)]
enum Pending {
    Query {
        intent: Option<Intent>,
        context: Context,
    },
    Command(Operation),
    /// `operation.get` for an operation whose reply was lost.
    Recover(Operation),
}

/// What the connection must do after an input: apply `updates`, write `frames` in order.
#[derive(Debug, Default)]
pub struct Step {
    pub updates: Vec<StoreUpdate>,
    pub frames: Vec<RequestFrame>,
}

impl Step {
    fn extend(&mut self, other: Step) {
        self.updates.extend(other.updates);
        self.frames.extend(other.frames);
    }
}

fn notice(level: NoticeLevel, code: &str, message: String) -> ServiceNotice {
    ServiceNotice {
        level,
        code: code.to_owned(),
        message,
    }
}

pub struct Core {
    new_id: Box<dyn FnMut() -> Id + Send>,
    connected: bool,
    ever_connected: bool,
    epoch: Option<Id>,
    /// Last applied event sequence; `None` until the snapshot reply sets the base.
    last_sequence: Option<u64>,
    snapshot_in_flight: bool,
    pending: HashMap<Id, Pending>,
    /// Operations whose reply was lost; queried with `operation.get` after reconnect.
    uncertain: Vec<Operation>,
    rechecks: Vec<Operation>,
    focused: Option<ObjectiveId>,
    activity_loaded: HashSet<ObjectiveId>,
    /// Objectives whose drafts were listed on the current connection (saves wait for this).
    drafts_reconciled: HashSet<ObjectiveId>,
    drafts_requested: HashSet<ObjectiveId>,
    deferred_saves: Vec<(ObjectiveId, Operation)>,
    deferred_queries: Vec<Intent>,
    open_on_connect: Option<PathBuf>,
}

impl Core {
    pub fn new(new_id: impl FnMut() -> Id + Send + 'static, open: Option<PathBuf>) -> Self {
        Self {
            new_id: Box::new(new_id),
            connected: false,
            ever_connected: false,
            epoch: None,
            last_sequence: None,
            snapshot_in_flight: false,
            pending: HashMap::new(),
            uncertain: Vec::new(),
            rechecks: Vec::new(),
            focused: None,
            activity_loaded: HashSet::new(),
            drafts_reconciled: HashSet::new(),
            drafts_requested: HashSet::new(),
            deferred_saves: Vec::new(),
            deferred_queries: Vec::new(),
            open_on_connect: open,
        }
    }

    #[cfg(test)]
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Operations waiting for their outcome to be recovered.
    #[cfg(test)]
    pub fn uncertain(&self) -> &[Operation] {
        &self.uncertain
    }

    fn frame(&mut self, operation_id: Option<Id>, request: Request) -> RequestFrame {
        let request_id = (self.new_id)();
        RequestFrame::new(request_id, operation_id, request)
            .expect("operation_id presence follows the request name")
    }

    fn query(
        &mut self,
        intent: Option<Intent>,
        request: Request,
        context: Context,
    ) -> RequestFrame {
        let frame = self.frame(None, request);
        self.pending
            .insert(frame.request_id.clone(), Pending::Query { intent, context });
        frame
    }

    fn send_command(&mut self, operation: Operation) -> RequestFrame {
        let frame = self.frame(
            Some(operation.operation_id.clone()),
            operation.request.clone(),
        );
        self.pending
            .insert(frame.request_id.clone(), Pending::Command(operation));
        frame
    }

    fn recover(&mut self, operation: Operation) -> RequestFrame {
        let request = Request::OperationGet(OperationGetQuery {
            operation_id: operation.operation_id.clone(),
        });
        let frame = self.frame(None, request);
        self.pending
            .insert(frame.request_id.clone(), Pending::Recover(operation));
        frame
    }

    fn snapshot(&mut self) -> Option<RequestFrame> {
        if self.snapshot_in_flight {
            return None;
        }
        self.snapshot_in_flight = true;
        self.last_sequence = None;
        Some(self.query(
            None,
            Request::Snapshot(lade_protocol::client::Empty {}),
            Context::Snapshot,
        ))
    }

    fn drafts_pending(&self, objective_id: &ObjectiveId) -> bool {
        self.pending.values().any(|p| {
            matches!(p, Pending::Query { context: Context::Drafts(o), .. } if o == objective_id)
        })
    }

    /// Accepts one user intent.
    pub fn submit(&mut self, intent: Intent) -> Step {
        let mut step = Step::default();
        let (request, context) = match requests::request(&intent) {
            Ok(Some(mapped)) => mapped,
            Ok(None) => return step,
            Err(invalid) => {
                let message = format!("{} was not sent: {}.", intent.label(), invalid.0);
                if let Intent::SaveDraft { draft_id, .. } = &intent {
                    step.updates.push(StoreUpdate::DraftSaveFailed {
                        draft_id: draft_id.clone(),
                        message: invalid.0.clone(),
                    });
                }
                step.updates.push(StoreUpdate::Notice(notice(
                    NoticeLevel::Warning,
                    "invalid_request",
                    message,
                )));
                return step;
            }
        };
        let mutating = request.name().is_mutating();
        if !self.connected {
            if mutating {
                let message = format!(
                    "{} was not sent: the window is not connected to the service.",
                    intent.label()
                );
                if let Intent::SaveDraft { draft_id, .. } = &intent {
                    step.updates.push(StoreUpdate::DraftSaveFailed {
                        draft_id: draft_id.clone(),
                        message: "not connected".into(),
                    });
                }
                step.updates.push(StoreUpdate::Notice(notice(
                    NoticeLevel::Warning,
                    "not_connected",
                    message,
                )));
            } else {
                self.remember_query(&intent);
            }
            return step;
        }
        if !mutating {
            match &intent {
                Intent::LoadObjective { objective_id } => self.focused = Some(objective_id.clone()),
                Intent::LoadActivity {
                    objective_id,
                    before: None,
                } => {
                    self.activity_loaded.insert(objective_id.clone());
                }
                Intent::LoadDrafts { objective_id } => {
                    self.drafts_requested.insert(objective_id.clone());
                }
                _ => {}
            }
            step.frames.push(self.query(Some(intent), request, context));
            return step;
        }
        let operation = Operation {
            operation_id: (self.new_id)(),
            request,
            context,
            checks: 0,
        };
        if let Intent::SaveDraft { objective_id, .. } = &intent
            && !self.drafts_reconciled.contains(objective_id)
        {
            // Never save before this connection has seen the durable drafts.
            if !self.drafts_pending(objective_id) {
                step.extend(self.submit(Intent::LoadDrafts {
                    objective_id: objective_id.clone(),
                }));
            }
            self.deferred_saves.push((objective_id.clone(), operation));
            return step;
        }
        step.frames.push(self.send_command(operation));
        step
    }

    fn remember_query(&mut self, intent: &Intent) {
        if self.deferred_queries.contains(intent) {
            return;
        }
        if self.deferred_queries.len() >= MAX_DEFERRED_QUERIES {
            self.deferred_queries.remove(0);
        }
        self.deferred_queries.push(intent.clone());
    }

    /// The link is authenticated with `service_epoch`.
    pub fn on_welcome(&mut self, service_epoch: Id) -> Step {
        let mut step = Step::default();
        let reconnect = self.ever_connected;
        self.connected = true;
        self.ever_connected = true;
        self.epoch = Some(service_epoch);
        self.drafts_reconciled.clear();
        self.snapshot_in_flight = false;
        step.updates
            .push(StoreUpdate::Connection(ConnectionState::Connected));
        step.frames.extend(self.snapshot());
        if let Some(path) = self.open_on_connect.take() {
            step.extend(self.submit(Intent::OpenWorkspace { path }));
        }
        let mut queries: Vec<Intent> = std::mem::take(&mut self.deferred_queries);
        if reconnect {
            queries.extend(self.focused_queries());
            if let Some(o) = &self.focused {
                // The inspected capture may have moved while away: flag it, never swap it.
                step.updates.push(StoreUpdate::WorktreeChanged {
                    objective_id: o.clone(),
                });
            }
        }
        let mut seen = Vec::new();
        for intent in queries {
            if !seen.contains(&intent) {
                seen.push(intent.clone());
                step.extend(self.submit(intent));
            }
        }
        for operation in std::mem::take(&mut self.uncertain) {
            if let Some(o) = operation.context.objective_id() {
                step.updates.push(StoreUpdate::CommandStatus {
                    objective_id: o.clone(),
                    status: Some(notice(
                        NoticeLevel::Info,
                        "query_operation",
                        format!(
                            "Checking outcome of “{}”… (its reply was lost when the connection dropped)",
                            operation.context.label()
                        ),
                    )),
                });
            }
            step.frames.push(self.recover(operation));
        }
        step
    }

    fn focused_queries(&self) -> Vec<Intent> {
        let Some(objective_id) = self.focused.clone() else {
            return Vec::new();
        };
        let mut queries = vec![
            Intent::LoadObjective {
                objective_id: objective_id.clone(),
            },
            Intent::LoadDrafts {
                objective_id: objective_id.clone(),
            },
            Intent::LoadInputs {
                objective_id: objective_id.clone(),
            },
        ];
        if self.activity_loaded.contains(&objective_id) {
            queries.push(Intent::LoadActivity {
                objective_id,
                before: None,
            });
        }
        queries
    }

    /// The link closed. Lost command replies become uncertain; lost queries are re-sent later.
    pub fn on_link_lost(&mut self) {
        self.connected = false;
        self.snapshot_in_flight = false;
        self.last_sequence = None;
        let mut pending: Vec<Pending> = self.pending.drain().map(|(_, p)| p).collect();
        pending.extend(self.rechecks.drain(..).map(Pending::Recover));
        for p in pending {
            match p {
                Pending::Command(operation) | Pending::Recover(operation) => {
                    self.uncertain.push(operation);
                }
                Pending::Query {
                    intent: Some(intent),
                    ..
                } => self.remember_query(&intent),
                Pending::Query { intent: None, .. } => {}
            }
        }
        // Deferred saves were never sent: their editors reconcile from drafts.list.
        self.deferred_saves.clear();
    }

    /// Applies one reply.
    pub fn on_reply(&mut self, reply: Reply) -> Step {
        let Some(pending) = self.pending.remove(&reply.request_id) else {
            return Step::default();
        };
        match pending {
            Pending::Query { context, .. } => self.settle(&context, reply.result),
            Pending::Command(operation) => self.settle(&operation.context, reply.result),
            Pending::Recover(operation) => self.recovered(operation, reply.result),
        }
    }

    fn settle(&mut self, context: &Context, result: ReplyResult) -> Step {
        let effects = match result {
            ReplyResult::Ok(data) => {
                let data = *data;
                if let ReplyData::Snapshot(snapshot) = &data {
                    self.snapshot_in_flight = false;
                    self.last_sequence = Some(snapshot.sequence.value());
                }
                requests::reply_effects(context, data)
            }
            ReplyResult::Rejected { rejection, .. } => {
                if matches!(context, Context::Snapshot) {
                    self.snapshot_in_flight = false;
                }
                requests::rejection_effects(context, &rejection)
            }
        };
        let mut step = self.apply_effects(effects);
        if let Context::Drafts(objective_id) = context {
            self.drafts_reconciled.insert(objective_id.clone());
            let (ready, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.deferred_saves)
                .into_iter()
                .partition(|(o, _)| o == objective_id);
            self.deferred_saves = waiting;
            for (_, operation) in ready {
                step.frames.push(self.send_command(operation));
            }
        }
        step
    }

    fn apply_effects(&mut self, effects: Effects) -> Step {
        let mut step = Step {
            updates: effects.updates,
            frames: Vec::new(),
        };
        for intent in effects.follow_ups {
            step.extend(self.submit(intent));
        }
        step
    }

    fn recovered(&mut self, mut operation: Operation, result: ReplyResult) -> Step {
        let label = operation.context.label();
        let objective = operation.context.objective_id().cloned();
        let (state, outcome, note) = match result {
            ReplyResult::Ok(data) => match *data {
                ReplyData::OperationGet(OperationStatus { state, outcome }) => {
                    (state, outcome, None)
                }
                _ => return Step::default(),
            },
            ReplyResult::Rejected { rejection, .. } => (
                OperationState::Unknown,
                None,
                Some(requests::rejection_message(
                    &format!("Checking the outcome of “{label}”"),
                    &rejection,
                )),
            ),
        };
        match outcome {
            Some(Outcome::Ok(data)) => {
                let mut step = self.settle(&operation.context, ReplyResult::Ok(data));
                step.updates.push(StoreUpdate::Notice(notice(
                    NoticeLevel::Info,
                    "operation_recovered",
                    format!("{label} had completed before the connection dropped."),
                )));
                return step;
            }
            Some(Outcome::Rejected { name, rejection }) => {
                return self.settle(
                    &operation.context,
                    ReplyResult::Rejected {
                        name: Some(name),
                        rejection,
                    },
                );
            }
            None => {}
        }
        let mut step = Step::default();
        let (level, code, message) = match state {
            OperationState::Accepted | OperationState::Pending
                if operation.checks < MAX_RECHECKS =>
            {
                operation.checks += 1;
                self.rechecks.push(operation.clone());
                (
                    NoticeLevel::Info,
                    "query_operation",
                    format!("Checking outcome of “{label}”… the service is still working on it."),
                )
            }
            OperationState::Accepted | OperationState::Pending | OperationState::Uncertain => (
                NoticeLevel::Warning,
                "uncertain",
                format!("The outcome of “{label}” is uncertain. Refresh before repeating it."),
            ),
            OperationState::Blocked => (
                NoticeLevel::Warning,
                "blocked",
                format!("“{label}” is blocked by the service."),
            ),
            OperationState::Completed => (
                NoticeLevel::Info,
                "operation_recovered",
                format!("{label} had completed before the connection dropped."),
            ),
            OperationState::Unknown => {
                if let Context::Command {
                    draft_id: Some(draft_id),
                    objective_id: Some(_),
                    ..
                } = &operation.context
                {
                    step.updates.push(StoreUpdate::DraftSaveFailed {
                        draft_id: draft_id.clone(),
                        message: "the save did not reach the service".into(),
                    });
                }
                (
                    NoticeLevel::Warning,
                    "unknown_operation",
                    note.unwrap_or_else(|| {
                        format!(
                            "{label} did not reach the service before the connection dropped; nothing was changed. Try again."
                        )
                    }),
                )
            }
        };
        let note = notice(level, code, message);
        if let Some(o) = &objective {
            step.updates.push(StoreUpdate::CommandStatus {
                objective_id: o.clone(),
                status: Some(note.clone()),
            });
            step.extend(self.submit(Intent::LoadObjective {
                objective_id: o.clone(),
            }));
        }
        if level != NoticeLevel::Info {
            step.updates.push(StoreUpdate::Notice(note));
        }
        step
    }

    /// A frame could not be encoded and never left: settle its request as refused locally.
    pub fn unsendable(&mut self, request_id: &Id, rejection: &Rejection) -> Step {
        let context = match self.pending.remove(request_id) {
            Some(Pending::Query { context, .. }) => context,
            Some(Pending::Command(operation) | Pending::Recover(operation)) => operation.context,
            None => return Step::default(),
        };
        Step {
            updates: requests::rejection_effects(&context, rejection).updates,
            frames: Vec::new(),
        }
    }

    /// Re-queries operations that were still running (call about once per second).
    pub fn tick(&mut self) -> Step {
        let mut step = Step::default();
        if !self.connected {
            return step;
        }
        for operation in std::mem::take(&mut self.rechecks) {
            step.frames.push(self.recover(operation));
        }
        step
    }

    /// Applies one event; a sequence gap or epoch change triggers a re-snapshot.
    pub fn on_event(&mut self, frame: EventFrame) -> Step {
        let mut step = Step::default();
        let sequence = frame.sequence.value();
        let same_epoch = self.epoch.as_ref() == Some(&frame.service_epoch);
        let resync = match self.last_sequence {
            _ if !same_epoch => true,
            None => false,
            Some(last) if sequence <= last => return step, // already in the snapshot
            Some(last) => sequence != last + 1,
        };
        if !resync && let Some(last) = self.last_sequence.as_mut() {
            *last = sequence;
        }
        step.updates.extend(event_updates(frame.event));
        if resync {
            step.extend(self.resync());
        }
        step
    }

    fn resync(&mut self) -> Step {
        let mut step = Step::default();
        step.frames.extend(self.snapshot());
        for intent in self.focused_queries() {
            step.extend(self.submit(intent));
        }
        if let Some(o) = &self.focused {
            step.updates.push(StoreUpdate::WorktreeChanged {
                objective_id: o.clone(),
            });
        }
        step
    }

    #[cfg(test)]
    pub fn last_sequence(&self) -> Option<u64> {
        self.last_sequence
    }

    #[cfg(test)]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

/// Maps one service event onto the store.
pub fn event_updates(event: Event) -> Vec<StoreUpdate> {
    if matches!(
        event,
        Event::SessionChanged(_)
            | Event::UiRequest(_)
            | Event::UiClosed(_)
            | Event::ResourcesChanged(_)
    ) {
        return Vec::new();
    }
    vec![match event {
        Event::ObjectiveChanged(s) => StoreUpdate::ObjectiveChanged(convert::summary(&s)),
        Event::ObjectiveDetail(d) => StoreUpdate::ObjectiveDetail(Box::new(convert::detail(&d))),
        Event::ActivityAppended(a) => StoreUpdate::ActivityAppended {
            objective_id: ObjectiveId::new(a.objective_id.as_str()),
            items: a.items.iter().map(convert::activity_item).collect(),
        },
        Event::ActivityDelta(d) => StoreUpdate::ActivityDelta {
            objective_id: ObjectiveId::new(d.objective_id.as_str()),
            item_id: crate::presentation::model::ActivityItemId::new(d.item_id.as_str()),
            append_text: d.append_text,
        },
        Event::DraftChanged(d) => StoreUpdate::DraftChanged(convert::draft(&d)),
        Event::InputChanged(i) => {
            let input = convert::input(&i);
            StoreUpdate::InputChanged {
                objective_id: input.objective_id.clone(),
                input,
            }
        }
        Event::WorktreeChanged(w) => StoreUpdate::WorktreeChanged {
            objective_id: ObjectiveId::new(w.objective_id.as_str()),
        },
        Event::VerificationChanged(v) => {
            StoreUpdate::VerificationChanged(convert::verification(&v))
        }
        Event::ServiceNotice(n) => StoreUpdate::Notice(convert::notice(&n)),
        Event::SessionChanged(_)
        | Event::UiRequest(_)
        | Event::UiClosed(_)
        | Event::ResourcesChanged(_) => return Vec::new(),
    }]
}

#[cfg(test)]
mod tests;
