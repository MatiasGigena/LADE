use std::cell::{Ref, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::agent_session::{AgentSessionChange, AgentSessionState};
use super::drafts::DraftEditor;
use super::model::{
    ActionSurface, Attention, ChangeList, ConnectionState, DraftId, DraftState, InputSummary, Load,
    ObjectiveDetail, ObjectiveId, ObjectiveSummary, Refusal, SearchResults, ServiceNotice,
    Settings, Workspace, WorkspaceId,
};
use crate::presentation::terminal::{TerminalSession, TerminalStatus};

mod activity;
mod apply;
mod ui;
mod update;

pub use activity::{ActivityChange, ActivityLog};
use apply::refusal_changes;
pub use ui::{
    InspectorTab, MainView, ObjectiveUi, Reveal, UiState, ViewerContent, ViewerKey, ViewerMode,
    ViewerState,
};
pub use update::{StoreUpdate, coalesce};

const MAX_NOTICES: usize = 20;
pub const MAX_TERMINAL_SESSIONS: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Connection,
    Workspaces,
    WorkspaceOpened,
    Objectives,
    Selection,
    Detail(ObjectiveId),
    Activity(ObjectiveId, ActivityChange),
    Changes(ObjectiveId),
    Viewer,
    ViewerSelection,
    Drafts(ObjectiveId),
    DraftStatus(DraftId),
    Inputs(ObjectiveId),
    Search,
    Notices,
    View,
    InspectorTab,
    Keymap,
    Layout,
    Catalog,
    Session(ObjectiveId),
    Resources,
    Dialogs,
    Terminal(ObjectiveId),
    Lifetime,
    DesktopAttention,
}

impl From<AgentSessionChange> for Change {
    fn from(change: AgentSessionChange) -> Self {
        match change {
            AgentSessionChange::Catalog => Self::Catalog,
            AgentSessionChange::Session(id) => Self::Session(id),
            AgentSessionChange::Resources => Self::Resources,
            AgentSessionChange::Dialogs => Self::Dialogs,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct KeptRefusal {
    refusal: Refusal,
    revision: Option<u64>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Board<'a> {
    pub groups: Vec<(Attention, Vec<&'a ObjectiveSummary>)>,
    pub archive: Vec<&'a ObjectiveSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresentationState {
    pub connection: ConnectionState,
    pub workspaces: Vec<Workspace>,
    pub active_workspace: Option<WorkspaceId>,
    pub settings: Settings,
    pub objectives: Vec<ObjectiveSummary>,
    pub details: HashMap<ObjectiveId, Load<ObjectiveDetail>>,
    pub activity: HashMap<ObjectiveId, ActivityLog>,
    pub changes: HashMap<ObjectiveId, Load<ChangeList>>,
    pub working_updated: HashSet<ObjectiveId>,
    pub drafts: HashMap<ObjectiveId, Vec<DraftEditor>>,
    pub inputs: HashMap<ObjectiveId, Vec<InputSummary>>,
    pub viewer: ViewerState,
    pub search: Load<SearchResults>,
    pub notices: Vec<ServiceNotice>,
    pub notices_received: usize,
    pub command_status: HashMap<ObjectiveId, ServiceNotice>,
    refusals: HashMap<(ObjectiveId, ActionSurface), KeptRefusal>,
    pub ui: UiState,
    pub agent_session: AgentSessionState,
    pub terminals: Vec<TerminalSession>,
    pub service_stopped: bool,
    pub notifications_unavailable: Option<String>,
}

impl Default for PresentationState {
    fn default() -> Self {
        Self::new(ConnectionState::Connecting)
    }
}
impl PresentationState {
    pub fn new(connection: ConnectionState) -> Self {
        Self {
            connection,
            workspaces: Vec::new(),
            active_workspace: None,
            settings: Settings::default(),
            objectives: Vec::new(),
            details: HashMap::new(),
            activity: HashMap::new(),
            changes: HashMap::new(),
            working_updated: HashSet::new(),
            drafts: HashMap::new(),
            inputs: HashMap::new(),
            viewer: ViewerState {
                key: None,
                content: ViewerContent::Empty,
                reveal: None,
            },
            search: Load::NotLoaded,
            notices: Vec::new(),
            notices_received: 0,
            command_status: HashMap::new(),
            refusals: HashMap::new(),
            ui: UiState::default(),
            agent_session: AgentSessionState::default(),
            terminals: Vec::new(),
            service_stopped: false,
            notifications_unavailable: None,
        }
    }

    pub fn active_workspace(&self) -> Option<&Workspace> {
        let id = self.active_workspace.as_ref()?;
        self.workspaces.iter().find(|w| &w.id == id)
    }

    pub fn objective(&self, id: &ObjectiveId) -> Option<&ObjectiveSummary> {
        self.objectives.iter().find(|o| &o.objective_id == id)
    }

    pub fn selected_summary(&self) -> Option<&ObjectiveSummary> {
        self.objective(self.ui.selected.as_ref()?)
    }

    pub fn detail(&self, id: &ObjectiveId) -> Option<&ObjectiveDetail> {
        self.details.get(id)?.loaded()
    }

    pub fn selected_detail(&self) -> Option<&ObjectiveDetail> {
        self.detail(self.ui.selected.as_ref()?)
    }

    pub fn selected_ui(&self) -> Option<&ObjectiveUi> {
        self.ui.per_objective.get(self.ui.selected.as_ref()?)
    }

    pub fn objective_ui_mut(&mut self, id: &ObjectiveId) -> &mut ObjectiveUi {
        self.ui.per_objective.entry(id.clone()).or_default()
    }

    pub fn workspace_objectives(&self) -> impl Iterator<Item = &ObjectiveSummary> {
        self.objectives.iter().filter(move |o| {
            self.active_workspace
                .as_ref()
                .is_none_or(|w| &o.workspace_id == w)
        })
    }

    pub fn board(&self) -> Board<'_> {
        let mut groups: Vec<(Attention, Vec<&ObjectiveSummary>)> = Attention::BOARD_ORDER
            .iter()
            .map(|&attention| (attention, Vec::new()))
            .collect();
        let mut archive = Vec::new();
        for objective in self.workspace_objectives() {
            if objective.is_archived() {
                archive.push(objective);
            } else if let Some((_, group)) =
                groups.iter_mut().find(|(a, _)| *a == objective.attention)
            {
                group.push(objective);
            }
        }
        let by_recency = |a: &&ObjectiveSummary, b: &&ObjectiveSummary| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.title.cmp(&b.title))
        };
        for (_, group) in &mut groups {
            group.sort_by(by_recency);
        }
        archive.sort_by(by_recency);
        groups.retain(|(_, g)| !g.is_empty());
        Board { groups, archive }
    }

    pub fn unsent_drafts(&self, id: &ObjectiveId) -> impl Iterator<Item = &DraftEditor> {
        self.drafts
            .get(id)
            .into_iter()
            .flatten()
            .filter(|d| d.state == DraftState::Unsent)
    }

    pub fn draft_mut(&mut self, draft_id: &DraftId) -> Option<&mut DraftEditor> {
        self.drafts
            .values_mut()
            .flatten()
            .find(|d| &d.draft_id == draft_id)
    }

    pub fn terminal_for(&self, id: &ObjectiveId) -> Option<&TerminalSession> {
        self.terminals.iter().rev().find(|t| &t.objective_id == id)
    }

    pub fn terminal_for_mut(&mut self, id: &ObjectiveId) -> Option<&mut TerminalSession> {
        self.terminals
            .iter_mut()
            .rev()
            .find(|t| &t.objective_id == id)
    }

    fn terminal_by_id_mut(&mut self, terminal_id: &str) -> Option<&mut TerminalSession> {
        self.terminals
            .iter_mut()
            .find(|t| t.terminal_id.as_deref() == Some(terminal_id))
    }

    pub fn push_terminal(&mut self, session: TerminalSession) {
        self.terminals.push(session);
        while self.terminals.len() > MAX_TERMINAL_SESSIONS {
            match self
                .terminals
                .iter()
                .position(|t| matches!(t.status, TerminalStatus::Ended(_)))
            {
                Some(index) => {
                    self.terminals.remove(index);
                }
                None => break,
            }
        }
    }

    pub fn last_refusal(
        &self,
        objective_id: &ObjectiveId,
        surface: ActionSurface,
    ) -> Option<&Refusal> {
        self.refusals
            .get(&(objective_id.clone(), surface))
            .map(|kept| &kept.refusal)
    }

    pub fn command_note(&self, objective_id: &ObjectiveId) -> Option<&ServiceNotice> {
        self.command_status.get(objective_id)
    }

    fn keep_refusal(
        &mut self,
        objective_id: ObjectiveId,
        surface: ActionSurface,
        refusal: Refusal,
    ) -> Vec<Change> {
        let revision = self.objective(&objective_id).map(|o| o.revision);
        self.refusals.insert(
            (objective_id.clone(), surface),
            KeptRefusal { refusal, revision },
        );
        refusal_changes(objective_id, surface)
    }

    fn objective_revised(&mut self, objective_id: &ObjectiveId, revision: u64) {
        self.refusals.retain(|(id, _), kept| {
            if id != objective_id {
                return true;
            }
            if let Some(kept_at) = kept.revision {
                kept_at == revision
            } else {
                kept.revision = Some(revision);
                true
            }
        });
    }

    fn terminal_problem(&mut self, objective_id: ObjectiveId, refusal: Refusal) -> Vec<Change> {
        if let Some(index) = self.terminals.iter().rposition(|t| {
            t.objective_id == objective_id
                && t.status == TerminalStatus::Opening
                && t.terminal_id.is_none()
        }) {
            self.terminals.remove(index);
        }
        self.keep_refusal(objective_id, ActionSurface::Terminal, refusal)
    }

    fn push_notice(&mut self, notice: ServiceNotice) {
        self.notices_received += 1;
        self.notices.push(notice);
        if self.notices.len() > MAX_NOTICES {
            self.notices.remove(0);
        }
    }
}

type Listener = Rc<dyn Fn(&[Change])>;

pub struct Store {
    state: RefCell<PresentationState>,
    listeners: RefCell<Vec<Listener>>,
}

impl Store {
    pub fn new(state: PresentationState) -> Rc<Self> {
        Rc::new(Self {
            state: RefCell::new(state),
            listeners: RefCell::new(Vec::new()),
        })
    }

    pub fn state(&self) -> Ref<'_, PresentationState> {
        self.state.borrow()
    }

    pub fn subscribe(&self, listener: impl Fn(&[Change]) + 'static) {
        self.listeners.borrow_mut().push(Rc::new(listener));
    }

    pub fn apply(&self, update: StoreUpdate) {
        self.apply_all(vec![update]);
    }

    pub fn apply_all(&self, updates: Vec<StoreUpdate>) {
        let changes = {
            let mut state = self.state.borrow_mut();
            coalesce(updates)
                .into_iter()
                .flat_map(|u| state.apply(u))
                .collect::<Vec<_>>()
        };
        self.notify(changes);
    }

    pub fn update<R>(&self, f: impl FnOnce(&mut PresentationState) -> (R, Vec<Change>)) -> R {
        let (result, changes) = f(&mut self.state.borrow_mut());
        self.notify(changes);
        result
    }

    fn notify(&self, changes: Vec<Change>) {
        let mut unique: Vec<Change> = Vec::with_capacity(changes.len());
        for change in changes {
            if !unique.contains(&change) {
                unique.push(change);
            }
        }
        if unique.is_empty() {
            return;
        }
        let listeners: Vec<Listener> = self.listeners.borrow().clone();
        for listener in listeners {
            listener(&unique);
        }
    }
}

#[cfg(test)]
mod tests;
