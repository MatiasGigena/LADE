use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use lade_protocol::client::{
    DraftCursor, Event, EventFrame, Rejection, Reply, ReplyData, ReplyResult, Request, RequestFrame,
};
use lade_protocol::types::{Counter, Id};

use super::convert;
use super::requests::{self, Context, Effects};
use crate::presentation::controller::intents::Intent;
use crate::presentation::state::model::{
    ConnectionState, Draft, DraftId, InputId, InputSummary, NoticeLevel, NoticeScope, ObjectiveId,
    ServiceNotice,
};
use crate::presentation::state::store::StoreUpdate;

mod events;
mod pages;
mod paging;
mod recovery;

pub use events::event_updates;
use pages::Pages;

const MAX_DEFERRED_QUERIES: usize = 64;
const MAX_RECHECKS: u32 = 60;

#[derive(Clone, Debug, PartialEq)]
pub struct Operation {
    /// Stable across resends, so the service recognises a resent command.
    pub id: Id,
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
    Recover(Operation),
}

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
        scope: NoticeScope::General,
    }
}

/// Reports an intent that produced no request; a draft save also marks the draft as failed.
fn not_sent(intent: &Intent, code: &str, reason: &str, draft_message: String) -> Step {
    let mut updates = Vec::new();
    if let Intent::SaveDraft { draft_id, .. } = intent {
        updates.push(StoreUpdate::DraftSaveFailed {
            draft_id: draft_id.clone(),
            message: draft_message,
        });
    }
    updates.push(StoreUpdate::Notice(notice(
        NoticeLevel::Warning,
        code,
        format!("{} was not sent: {reason}.", intent.label()),
    )));
    Step {
        updates,
        frames: Vec::new(),
    }
}

pub struct Core {
    new_id: Box<dyn FnMut() -> Id + Send>,
    connected: bool,
    ever_connected: bool,
    epoch: Option<Id>,
    last_sequence: Option<u64>,
    snapshot_in_flight: bool,
    pending: HashMap<Id, Pending>,
    uncertain: Vec<Operation>,
    rechecks: Vec<Operation>,
    focused: Option<ObjectiveId>,
    activity_loaded: HashSet<ObjectiveId>,
    drafts_reconciled: HashSet<ObjectiveId>,
    drafts_requested: HashSet<ObjectiveId>,
    draft_pages: HashMap<ObjectiveId, Pages<DraftId, Draft, DraftCursor>>,
    input_pages: HashMap<ObjectiveId, Pages<InputId, InputSummary, Counter>>,
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
            draft_pages: HashMap::new(),
            input_pages: HashMap::new(),
            deferred_saves: Vec::new(),
            deferred_queries: Vec::new(),
            open_on_connect: open,
        }
    }

    #[cfg(test)]
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    #[cfg(test)]
    pub fn uncertain(&self) -> &[Operation] {
        &self.uncertain
    }

    /// Frames a read-only request. Queries never carry an operation id; `submit` routes every
    /// mutating request through [`Self::send_command`] instead.
    fn query_frame(&mut self, request: Request) -> RequestFrame {
        debug_assert!(!request.name().is_mutating(), "queries are read-only");
        RequestFrame {
            request_id: (self.new_id)(),
            operation_id: None,
            request,
        }
    }

    fn query(
        &mut self,
        intent: Option<Intent>,
        request: Request,
        context: Context,
    ) -> RequestFrame {
        let frame = self.query_frame(request);
        self.pending
            .insert(frame.request_id.clone(), Pending::Query { intent, context });
        frame
    }

    /// Frames a mutating command under its stable operation id, so a resend after a lost reply
    /// is recognised by the service as the same operation.
    fn send_command(&mut self, operation: Operation) -> RequestFrame {
        debug_assert!(
            operation.request.name().is_mutating(),
            "commands are mutating"
        );
        let frame = RequestFrame {
            request_id: (self.new_id)(),
            operation_id: Some(operation.id.clone()),
            request: operation.request.clone(),
        };
        self.pending
            .insert(frame.request_id.clone(), Pending::Command(operation));
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

    pub fn submit(&mut self, intent: Intent) -> Step {
        let (request, context) = match requests::request(&intent) {
            Ok(Some(mapped)) => mapped,
            Ok(None) => return Step::default(),
            Err(invalid) => {
                return not_sent(&intent, "invalid_request", &invalid.0, invalid.0.clone());
            }
        };
        let mutating = request.name().is_mutating();
        if !self.connected {
            return self.submit_disconnected(&intent, mutating);
        }
        if !mutating {
            return self.submit_query(intent, request, context);
        }
        let mut step = Step::default();
        let operation = Operation {
            id: (self.new_id)(),
            request,
            context,
            checks: 0,
        };
        if let Intent::SaveDraft { objective_id, .. } = &intent
            && !self.drafts_reconciled.contains(objective_id)
        {
            if !self.draft_pages.contains_key(objective_id) {
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

    /// Terminal intents are refused, commands fail with a notice, and queries wait for the
    /// next connection.
    fn submit_disconnected(&mut self, intent: &Intent, mutating: bool) -> Step {
        const REASON: &str = "the window is not connected to the service";
        if let Some(objective_id) = intent.terminal_objective() {
            return Step {
                updates: vec![StoreUpdate::TerminalProblem {
                    objective_id: objective_id.clone(),
                    message: format!("{} was not sent: {REASON}.", intent.label()),
                }],
                frames: Vec::new(),
            };
        }
        if mutating {
            return not_sent(intent, "not_connected", REASON, "not connected".into());
        }
        self.remember_query(intent);
        Step::default()
    }

    fn submit_query(&mut self, intent: Intent, request: Request, context: Context) -> Step {
        if matches!(&context, Context::Drafts(o) if self.draft_pages.contains_key(o))
            || matches!(&context, Context::Inputs(o) if self.input_pages.contains_key(o))
        {
            return Step::default();
        }
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
        let frame = self.query(Some(intent), request, context.clone());
        match context {
            Context::Drafts(o) => {
                self.draft_pages
                    .insert(o.clone(), Pages::new(frame.request_id.clone()));
                self.drafts_reconciled.remove(&o);
            }
            Context::Inputs(o) => {
                self.input_pages
                    .insert(o, Pages::new(frame.request_id.clone()));
            }
            _ => {}
        }
        Step {
            updates: Vec::new(),
            frames: vec![frame],
        }
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
                } => {
                    if !intent.is_terminal() {
                        self.remember_query(&intent);
                    }
                }
                Pending::Query { intent: None, .. } => {}
            }
        }
        self.deferred_saves.clear();
        self.draft_pages.clear();
        self.input_pages.clear();
    }

    pub fn on_reply(&mut self, reply: Reply) -> Step {
        let Some(pending) = self.pending.remove(&reply.request_id) else {
            return Step::default();
        };
        match pending {
            Pending::Query {
                context: Context::Drafts(o),
                ..
            } => self.draft_page(o, &reply.request_id, reply.result),
            Pending::Query {
                context: Context::Inputs(o),
                ..
            } => self.input_page(o, &reply.request_id, reply.result),
            Pending::Query { context, .. } => self.settle(&context, reply.result),
            Pending::Command(operation) => self.settle(&operation.context, reply.result),
            Pending::Recover(operation) => self.recovered(operation, reply.result),
        }
    }

    fn settle(&mut self, context: &Context, result: ReplyResult) -> Step {
        let (effects, rejected) = match result {
            ReplyResult::Ok(data) => {
                let data = *data;
                if let ReplyData::Snapshot(snapshot) = &data {
                    self.snapshot_in_flight = false;
                    self.last_sequence = Some(snapshot.sequence.value());
                }
                self.observe_reply(context, &data);
                (requests::reply_effects(context, &data), None)
            }
            ReplyResult::Rejected { rejection, .. } => {
                if matches!(context, Context::Snapshot) {
                    self.snapshot_in_flight = false;
                }
                (
                    requests::rejection_effects(context, &rejection),
                    Some(rejection),
                )
            }
        };
        let mut step = self.apply_effects(effects);
        if let Context::Drafts(objective_id) = context {
            self.finish_drafts(objective_id, rejected.as_ref(), &mut step);
        }
        step
    }

    fn finish_drafts(
        &mut self,
        objective_id: &ObjectiveId,
        rejected: Option<&Rejection>,
        step: &mut Step,
    ) {
        if rejected.is_none() {
            self.drafts_reconciled.insert(objective_id.clone());
        }
        let (ready, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.deferred_saves)
            .into_iter()
            .partition(|(o, _)| o == objective_id);
        self.deferred_saves = waiting;
        for (_, operation) in ready {
            match rejected {
                Some(rejection) => step
                    .updates
                    .extend(requests::rejection_effects(&operation.context, rejection).updates),
                None => step.frames.push(self.send_command(operation)),
            }
        }
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

    /// Keeps draft and input pages that are still loading current with a
    /// record this reply changed.
    fn observe_reply(&mut self, context: &Context, data: &ReplyData) {
        match data {
            ReplyData::DraftSave(draft) => self.draft_changed(convert::draft(draft)),
            ReplyData::DraftDelete(_) => {
                if let Context::DraftDelete(_, draft_id) = context {
                    for pages in self.draft_pages.values_mut() {
                        pages.remove(draft_id.clone());
                    }
                }
            }
            ReplyData::MessageSend(input) | ReplyData::FeedbackSubmit(input) => {
                self.input_changed(convert::input(input));
            }
            _ => {}
        }
    }

    /// The event counterpart of `observe_reply`.
    fn observe_event(&mut self, event: &Event) {
        match event {
            Event::DraftChanged(draft) => self.draft_changed(convert::draft(draft)),
            Event::InputChanged(input) => self.input_changed(convert::input(input)),
            _ => {}
        }
    }

    fn draft_changed(&mut self, draft: Draft) {
        if let Some(pages) = self.draft_pages.get_mut(&draft.objective_id) {
            pages.change(draft.draft_id.clone(), draft);
        }
    }

    fn input_changed(&mut self, input: InputSummary) {
        if let Some(pages) = self.input_pages.get_mut(&input.objective_id) {
            pages.change(input.input_id.clone(), input);
        }
    }

    pub fn unsendable(&mut self, request_id: &Id, rejection: &Rejection) -> Step {
        let context = match self.pending.remove(request_id) {
            Some(Pending::Query {
                context: Context::Drafts(o),
                ..
            }) => {
                return self.draft_page(
                    o,
                    request_id,
                    ReplyResult::Rejected {
                        name: None,
                        rejection: rejection.clone(),
                    },
                );
            }
            Some(Pending::Query {
                context: Context::Inputs(o),
                ..
            }) => {
                return self.input_page(
                    o,
                    request_id,
                    ReplyResult::Rejected {
                        name: None,
                        rejection: rejection.clone(),
                    },
                );
            }
            Some(Pending::Query { context, .. }) => context,
            Some(Pending::Command(operation) | Pending::Recover(operation)) => operation.context,
            None => return Step::default(),
        };
        Step {
            updates: requests::rejection_effects(&context, rejection).updates,
            frames: Vec::new(),
        }
    }

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

    pub fn on_event(&mut self, frame: EventFrame) -> Step {
        let mut step = Step::default();
        let sequence = frame.sequence.value();
        let same_epoch = self.epoch.as_ref() == Some(&frame.service_epoch);
        let resync = match self.last_sequence {
            _ if !same_epoch => true,
            None => false,
            Some(last) if sequence <= last => return step,
            Some(last) => sequence != last + 1,
        };
        if !resync && let Some(last) = self.last_sequence.as_mut() {
            *last = sequence;
        }
        self.observe_event(&frame.event);
        step.updates.extend(event_updates(frame.event));
        if resync {
            step.extend(self.resync());
        }
        step
    }

    fn resync(&mut self) -> Step {
        let mut step = Step::default();
        let mut queries = self.focused_queries();
        for (objective_id, pages) in self.draft_pages.drain() {
            self.pending.remove(&pages.request_id);
            queries.push(Intent::LoadDrafts { objective_id });
        }
        for (objective_id, pages) in self.input_pages.drain() {
            self.pending.remove(&pages.request_id);
            queries.push(Intent::LoadInputs { objective_id });
        }
        step.frames.extend(self.snapshot());
        for intent in queries {
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

#[cfg(test)]
mod tests;
