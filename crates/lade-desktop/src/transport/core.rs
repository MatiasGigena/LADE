use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use lade_protocol::client::{
    DraftCursor, ErrorCode, Event, EventFrame, OperationGetQuery, OperationState, OperationStatus,
    Outcome, Rejection, Reply, ReplyData, ReplyResult, Request, RequestFrame, SafeAction,
};
use lade_protocol::types::{Counter, Id};

use super::convert;
use super::requests::{self, Context, Effects};
use crate::presentation::controller::intents::Intent;
use crate::presentation::state::agent_session::AgentSessionUpdate;
use crate::presentation::state::model::{
    ConnectionState, Draft, DraftId, InputId, InputSummary, NoticeLevel, NoticeScope, ObjectiveId,
    ServiceNotice,
};
use crate::presentation::state::store::StoreUpdate;

mod pages;
use pages::Pages;

const MAX_DEFERRED_QUERIES: usize = 64;
const MAX_RECHECKS: u32 = 60;

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
        if !self.connected
            && let Some(objective_id) = intent.terminal_objective()
        {
            step.updates.push(StoreUpdate::TerminalProblem {
                objective_id: objective_id.clone(),
                message: format!(
                    "{} was not sent: the window is not connected to the service.",
                    intent.label()
                ),
            });
            return step;
        }
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
            if matches!(&context, Context::Drafts(o) if self.draft_pages.contains_key(o))
                || matches!(&context, Context::Inputs(o) if self.input_pages.contains_key(o))
            {
                return step;
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
            step.frames.push(frame);
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
            } => self.draft_page(o, reply.request_id, reply.result),
            Pending::Query {
                context: Context::Inputs(o),
                ..
            } => self.input_page(o, reply.request_id, reply.result),
            Pending::Query { context, .. } => self.settle(&context, reply.result),
            Pending::Command(operation) => self.settle(&operation.context, reply.result),
            Pending::Recover(operation) => self.recovered(operation, reply.result),
        }
    }

    fn invalid_page(&mut self, context: &Context) -> Step {
        self.settle(
            context,
            ReplyResult::Rejected {
                name: None,
                rejection: Rejection::new(
                    ErrorCode::InvalidRequest,
                    SafeAction::None,
                    "The service returned an invalid history page. Refresh to try again.",
                ),
            },
        )
    }

    fn draft_page(
        &mut self,
        objective_id: ObjectiveId,
        request_id: Id,
        result: ReplyResult,
    ) -> Step {
        if !self
            .draft_pages
            .get(&objective_id)
            .is_some_and(|p| p.request_id == request_id)
        {
            return Step::default();
        }
        let mut pages = self
            .draft_pages
            .remove(&objective_id)
            .expect("checked active page");
        let context = Context::Drafts(objective_id.clone());
        let data = match result {
            ReplyResult::Ok(data) => match *data {
                ReplyData::DraftsList(data) => data,
                _ => return self.invalid_page(&context),
            },
            rejected => return self.settle(&context, rejected),
        };
        if data
            .drafts
            .iter()
            .any(|draft| draft.objective_id.as_str() != objective_id.as_str())
        {
            return self.invalid_page(&context);
        }
        if let Some(next) = &data.next_after {
            let advances = pages.after.as_ref().is_none_or(|after| {
                (&next.created_at, &next.draft_id) > (&after.created_at, &after.draft_id)
            });
            if !advances
                || data
                    .drafts
                    .last()
                    .is_none_or(|draft| draft.draft_id != next.draft_id)
            {
                return self.invalid_page(&context);
            }
        }
        pages.page(
            data.drafts
                .iter()
                .map(convert::draft)
                .map(|draft| (draft.draft_id.clone(), draft)),
        );
        if let Some(next) = data.next_after {
            let intent = Intent::LoadDrafts {
                objective_id: objective_id.clone(),
            };
            let (mut request, _) = requests::request(&intent)
                .expect("validated objective")
                .expect("load query");
            let Request::DraftsList(query) = &mut request else {
                unreachable!()
            };
            query.after = Some(next.clone());
            let frame = self.query(Some(intent), request, context);
            pages.request_id = frame.request_id.clone();
            pages.after = Some(next);
            self.draft_pages.insert(objective_id, pages);
            return Step {
                updates: Vec::new(),
                frames: vec![frame],
            };
        }
        let mut step = Step {
            updates: vec![StoreUpdate::Drafts {
                objective_id: objective_id.clone(),
                drafts: pages.complete(),
            }],
            frames: Vec::new(),
        };
        self.finish_drafts(&objective_id, None, &mut step);
        step
    }

    fn input_page(
        &mut self,
        objective_id: ObjectiveId,
        request_id: Id,
        result: ReplyResult,
    ) -> Step {
        if !self
            .input_pages
            .get(&objective_id)
            .is_some_and(|p| p.request_id == request_id)
        {
            return Step::default();
        }
        let mut pages = self
            .input_pages
            .remove(&objective_id)
            .expect("checked active page");
        let context = Context::Inputs(objective_id.clone());
        let data = match result {
            ReplyResult::Ok(data) => match *data {
                ReplyData::InputsList(data) => data,
                _ => return self.invalid_page(&context),
            },
            rejected => return self.settle(&context, rejected),
        };
        if data
            .inputs
            .iter()
            .any(|input| input.objective_id.as_str() != objective_id.as_str())
        {
            return self.invalid_page(&context);
        }
        if let Some(next) = &data.next_after
            && (data.inputs.is_empty()
                || next.value() <= pages.after.as_ref().map_or(0, Counter::value))
        {
            return self.invalid_page(&context);
        }
        pages.page(
            data.inputs
                .iter()
                .map(convert::input)
                .map(|input| (input.input_id.clone(), input)),
        );
        if let Some(next) = data.next_after {
            let intent = Intent::LoadInputs {
                objective_id: objective_id.clone(),
            };
            let (mut request, _) = requests::request(&intent)
                .expect("validated objective")
                .expect("load query");
            let Request::InputsList(query) = &mut request else {
                unreachable!()
            };
            query.after = Some(next.clone());
            let frame = self.query(Some(intent), request, context);
            pages.request_id = frame.request_id.clone();
            pages.after = Some(next);
            self.input_pages.insert(objective_id, pages);
            return Step {
                updates: Vec::new(),
                frames: vec![frame],
            };
        }
        Step {
            updates: vec![StoreUpdate::Inputs {
                objective_id,
                inputs: pages.complete(),
            }],
            frames: Vec::new(),
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
                (requests::reply_effects(context, data), None)
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
        self.observe_updates(&effects.updates);
        let mut step = Step {
            updates: effects.updates,
            frames: Vec::new(),
        };
        for intent in effects.follow_ups {
            step.extend(self.submit(intent));
        }
        step
    }

    fn observe_updates(&mut self, updates: &[StoreUpdate]) {
        for update in updates {
            match update {
                StoreUpdate::DraftChanged(draft) => {
                    if let Some(pages) = self.draft_pages.get_mut(&draft.objective_id) {
                        pages.change(draft.draft_id.clone(), draft.clone());
                    }
                }
                StoreUpdate::DraftDeleted { draft_id } => {
                    for pages in self.draft_pages.values_mut() {
                        pages.remove(draft_id.clone());
                    }
                }
                StoreUpdate::InputChanged {
                    objective_id,
                    input,
                } => {
                    if let Some(pages) = self.input_pages.get_mut(objective_id) {
                        pages.change(input.input_id.clone(), input.clone());
                    }
                }
                _ => {}
            }
        }
    }

    fn settle_recovered(&mut self, context: &Context, data: ReplyData) -> Step {
        let refresh = match &data {
            ReplyData::DraftSave(draft) => vec![Intent::LoadDrafts {
                objective_id: ObjectiveId::new(draft.objective_id.as_str()),
            }],
            ReplyData::DraftDelete(_) => self
                .drafts_requested
                .iter()
                .map(|objective_id| Intent::LoadDrafts {
                    objective_id: objective_id.clone(),
                })
                .collect(),
            ReplyData::MessageSend(input) | ReplyData::FeedbackSubmit(input) => {
                vec![Intent::LoadInputs {
                    objective_id: ObjectiveId::new(input.objective_id.as_str()),
                }]
            }
            _ => return self.settle(context, ReplyResult::Ok(Box::new(data))),
        };
        let mut effects = requests::reply_effects(context, data);
        // An operation outcome contains the record as it was when the command
        // completed. Current pages/events may already contain newer metadata,
        // even at the same draft revision. Reconcile from current service state.
        effects.updates.retain(|update| {
            !matches!(
                update,
                StoreUpdate::DraftChanged(_)
                    | StoreUpdate::DraftDeleted { .. }
                    | StoreUpdate::InputChanged { .. }
            )
        });
        effects.follow_ups.extend(refresh);
        self.apply_effects(effects)
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
                    &format!("check the outcome of “{label}”"),
                    &rejection,
                )),
            ),
        };
        match outcome {
            Some(Outcome::Ok(data)) => {
                let mut step = self.settle_recovered(&operation.context, *data);
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

    pub fn unsendable(&mut self, request_id: &Id, rejection: &Rejection) -> Step {
        let context = match self.pending.remove(request_id) {
            Some(Pending::Query {
                context: Context::Drafts(o),
                ..
            }) => {
                return self.draft_page(
                    o,
                    request_id.clone(),
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
                    request_id.clone(),
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
        let updates = event_updates(frame.event);
        self.observe_updates(&updates);
        step.updates.extend(updates);
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

pub fn event_updates(event: Event) -> Vec<StoreUpdate> {
    vec![match event {
        Event::ObjectiveChanged(s) => StoreUpdate::ObjectiveChanged(convert::summary(&s)),
        Event::ObjectiveDetail(d) => StoreUpdate::ObjectiveDetail(Box::new(convert::detail(&d))),
        Event::ActivityAppended(a) => StoreUpdate::ActivityAppended {
            objective_id: ObjectiveId::new(a.objective_id.as_str()),
            items: a.items.iter().map(convert::activity_item).collect(),
        },
        Event::ActivityDelta(d) => StoreUpdate::ActivityDelta {
            objective_id: ObjectiveId::new(d.objective_id.as_str()),
            item_id: crate::presentation::state::model::ActivityItemId::new(d.item_id.as_str()),
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
        Event::SessionChanged(s) => {
            let (objective_id, session) = super::agent_session::session(&s);
            StoreUpdate::AgentSession(AgentSessionUpdate::Session {
                objective_id,
                session,
            })
        }
        Event::UiRequest(r) => StoreUpdate::AgentSession(AgentSessionUpdate::DialogOpened(
            super::agent_session::dialog(&r),
        )),
        Event::UiClosed(c) => StoreUpdate::AgentSession(AgentSessionUpdate::DialogClosed {
            ui_id: c.ui_id.as_str().to_owned(),
        }),
        Event::ResourcesChanged(i) => StoreUpdate::AgentSession(AgentSessionUpdate::Inventory(
            super::agent_session::inventory(&i),
        )),
        Event::TerminalOutput(o) => StoreUpdate::TerminalOutput {
            terminal_id: o.terminal_id.as_str().to_owned(),
            seq: o.seq.value(),
            data: o.data,
            dropped: o.dropped_bytes.value(),
        },
        Event::TerminalClosed(c) => StoreUpdate::TerminalClosed {
            terminal_id: c.terminal_id.as_str().to_owned(),
            end: convert::terminal_end(&c),
        },
    }]
}

#[cfg(test)]
mod tests;
