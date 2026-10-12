//! The single client presentation store: service projections (rebuilt from snapshot + events) plus
//! transient UI state (selection, viewer, unsaved composer and draft text). No widget owns state.
//!
//! The store is main-thread only (`Rc`). Other threads send [`StoreUpdate`]s through
//! `views::bridge::UpdateSender`, which marshals them onto the GTK main context.

use std::cell::{Ref, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::compat::{CompatChange, CompatState, CompatUpdate};
use super::drafts::DraftEditor;
use super::keys::Keymap;
use super::layout::{LayoutClass, Region};
use super::model::{
    ActivityItem, ActivityItemId, Attention, Basis, ChangeList, ConnectionState, DeliveryMode,
    Draft, DraftId, FileContent, FileDiff, FilePage, InputSummary, Load, ObjectiveDetail,
    ObjectiveId, ObjectiveSummary, Reference, RepoPath, SearchResults, ServiceNotice, Settings,
    Side, VerificationSummary, Workspace, WorkspaceId,
};
use super::terminal::{TerminalEnd, TerminalMode, TerminalSession, TerminalStatus};

/// Maximum retained notices (older ones are dropped from presentation only).
const MAX_NOTICES: usize = 20;
/// Terminal sessions kept for display (ended ones are dropped oldest first).
pub const MAX_TERMINAL_SESSIONS: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreUpdate {
    Connection(ConnectionState),
    Snapshot {
        workspaces: Vec<Workspace>,
        objectives: Vec<ObjectiveSummary>,
        settings: Settings,
    },
    WorkspaceOpened(Workspace),
    ObjectiveChanged(ObjectiveSummary),
    ObjectiveDetail(Box<ObjectiveDetail>),
    ObjectiveLoadFailed {
        objective_id: ObjectiveId,
        message: String,
    },
    ActivityPage {
        objective_id: ObjectiveId,
        items: Vec<ActivityItem>,
        has_more: bool,
    },
    ActivityAppended {
        objective_id: ObjectiveId,
        items: Vec<ActivityItem>,
    },
    ActivityDelta {
        objective_id: ObjectiveId,
        item_id: ActivityItemId,
        append_text: String,
    },
    Changes(ChangeList),
    ChangesFailed {
        objective_id: ObjectiveId,
        basis: Basis,
        message: String,
    },
    WorktreeChanged {
        objective_id: ObjectiveId,
    },
    Diff(FileDiff),
    FilePage(FilePage),
    ViewerFailed {
        objective_id: ObjectiveId,
        path: RepoPath,
        message: String,
    },
    Drafts {
        objective_id: ObjectiveId,
        drafts: Vec<Draft>,
    },
    DraftChanged(Draft),
    DraftSaveFailed {
        draft_id: DraftId,
        message: String,
    },
    DraftDeleted {
        draft_id: DraftId,
    },
    Inputs {
        objective_id: ObjectiveId,
        inputs: Vec<InputSummary>,
    },
    InputChanged {
        objective_id: ObjectiveId,
        input: InputSummary,
    },
    VerificationChanged(VerificationSummary),
    Search(SearchResults),
    SearchFailed {
        message: String,
    },
    Notice(ServiceNotice),
    /// Settings changed (`settings.update` reply).
    Settings(Settings),
    /// The service created an Objective at this window's request: select it.
    Select(ObjectiveId),
    /// Outcome status of the latest command for an Objective, shown beside its actions
    /// (`None` clears it).
    CommandStatus {
        objective_id: ObjectiveId,
        status: Option<ServiceNotice>,
    },
    /// IMP-011 projections (catalogue, session, resources, dialogs).
    Compat(CompatUpdate),
    TerminalOpened {
        objective_id: ObjectiveId,
        terminal_id: String,
        mode: TerminalMode,
        cols: u16,
        rows: u16,
    },
    TerminalOutput {
        terminal_id: String,
        seq: u64,
        data: String,
        dropped: u64,
    },
    TerminalClosed {
        terminal_id: String,
        end: TerminalEnd,
    },
    /// A terminal request was refused or could not be sent; shown in the terminal pane.
    TerminalProblem {
        objective_id: ObjectiveId,
        message: String,
    },
    /// The service accepted `service.stop`: the window ends without reconnecting.
    ServiceStopped,
    /// Desktop notifications cannot be shown (the board remains the source of attention).
    NotificationsUnavailable {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActivityChange {
    Reset,
    Appended { from_index: usize },
    Delta { index: usize },
}

/// What changed, so views refresh only the affected parts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Connection,
    Workspaces,
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
    /// The model catalogue or the picker's choice changed.
    Catalog,
    /// An Objective's pi session state, tree or export changed.
    Session(ObjectiveId),
    /// The LADE-profile resource inventory changed.
    Resources,
    /// Extension dialogs opened or closed.
    Dialogs,
    Terminal(ObjectiveId),
    /// The service was stopped on request (Exit/logout).
    Lifetime,
    /// Availability of the desktop notification channel.
    DesktopAttention,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MainView {
    #[default]
    Workbench,
    Activity,
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum InspectorTab {
    #[default]
    Overview,
    Evidence,
    Feedback,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ViewerMode {
    #[default]
    Diff,
    FullFile,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectiveUi {
    pub basis: Basis,
    pub file: Option<RepoPath>,
    pub mode: ViewerMode,
    pub composer_text: String,
    pub composer_mode: DeliveryMode,
    pub feedback_mode: DeliveryMode,
    /// The current viewer selection mapped to a versioned reference, or why it cannot be.
    pub selection: Option<Result<Reference, String>>,
}

impl Default for ObjectiveUi {
    fn default() -> Self {
        Self {
            basis: Basis::Working,
            file: None,
            mode: ViewerMode::Diff,
            composer_text: String::new(),
            composer_mode: DeliveryMode::Prompt,
            feedback_mode: DeliveryMode::Steer,
            selection: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct UiState {
    pub view: MainView,
    pub inspector_tab: InspectorTab,
    pub selected: Option<ObjectiveId>,
    pub archive_expanded: bool,
    /// Current keyboard shortcuts (a client preference; see `keys`).
    pub keymap: Keymap,
    /// Responsive composition (see `layout`): class from the allocated width, the region shown in
    /// the compact composition and whether its Objective details are expanded.
    pub layout: LayoutClass,
    pub compact_region: Region,
    pub compact_details: bool,
    pub per_objective: HashMap<ObjectiveId, ObjectiveUi>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewerKey {
    pub objective_id: ObjectiveId,
    pub basis: Basis,
    pub path: RepoPath,
    pub mode: ViewerMode,
    pub side: Side,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewerContent {
    Empty,
    Loading,
    Diff(FileDiff),
    File(FileContent),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Viewer {
    pub key: Option<ViewerKey>,
    pub content: ViewerContent,
    /// A byte range to select once the file shown under `key` has loaded it (a followed
    /// content search result), so it becomes the feedback anchor.
    pub reveal: Option<Reveal>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reveal {
    pub key: ViewerKey,
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ActivityLog {
    pub items: Vec<ActivityItem>,
    pub has_more: bool,
    pub loaded: bool,
}

/// Board grouping derived from service attention; archived lifecycles are listed separately.
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
    /// Objectives whose working capture changed after the shown Change Set was loaded.
    pub working_updated: HashSet<ObjectiveId>,
    pub drafts: HashMap<ObjectiveId, Vec<DraftEditor>>,
    pub inputs: HashMap<ObjectiveId, Vec<InputSummary>>,
    pub viewer: Viewer,
    pub search: Load<SearchResults>,
    pub notices: Vec<ServiceNotice>,
    /// Latest command outcome per Objective (rejection, or "Checking outcome…").
    pub command_status: HashMap<ObjectiveId, ServiceNotice>,
    pub ui: UiState,
    /// IMP-011 projections and the model picker's choice.
    pub compat: CompatState,
    /// Contained terminals of this window, newest last.
    pub terminals: Vec<TerminalSession>,
    /// `service.stop` was accepted.
    pub service_stopped: bool,
    /// Why desktop notifications are unavailable, once known.
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
            viewer: Viewer {
                key: None,
                content: ViewerContent::Empty,
                reveal: None,
            },
            search: Load::NotLoaded,
            notices: Vec::new(),
            command_status: HashMap::new(),
            ui: UiState::default(),
            compat: CompatState::default(),
            terminals: Vec::new(),
            service_stopped: false,
            notifications_unavailable: None,
        }
    }

    pub fn active_workspace(&self) -> Option<&Workspace> {
        let id = self.active_workspace.as_ref()?;
        self.workspaces.iter().find(|w| &w.workspace_id == id)
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

    /// Needs attention > Ready for review > Working > Idle, most recently updated first within a
    /// group; Integrated / Completed / Discarded in the archive.
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
            .filter(|d| d.state == super::model::DraftState::Unsent)
    }

    pub fn draft_mut(&mut self, draft_id: &DraftId) -> Option<&mut DraftEditor> {
        self.drafts
            .values_mut()
            .flatten()
            .find(|d| &d.draft_id == draft_id)
    }

    /// The latest terminal session of an Objective.
    pub fn terminal_for(&self, id: &ObjectiveId) -> Option<&TerminalSession> {
        self.terminals.iter().rev().find(|t| &t.objective_id == id)
    }

    pub fn terminal_for_mut(&mut self, id: &ObjectiveId) -> Option<&mut TerminalSession> {
        self.terminals
            .iter_mut()
            .rev()
            .find(|t| &t.objective_id == id)
    }

    fn terminal_by_id(&mut self, terminal_id: &str) -> Option<&mut TerminalSession> {
        self.terminals
            .iter_mut()
            .find(|t| t.terminal_id.as_deref() == Some(terminal_id))
    }

    /// Adds a session, dropping the oldest ended ones beyond [`MAX_TERMINAL_SESSIONS`].
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

    fn push_notice(&mut self, notice: ServiceNotice) {
        self.notices.push(notice);
        if self.notices.len() > MAX_NOTICES {
            self.notices.remove(0);
        }
    }

    /// Applies one service-derived update, returning what changed.
    pub fn apply(&mut self, update: StoreUpdate) -> Vec<Change> {
        match update {
            StoreUpdate::Connection(connection) => {
                let connected = connection.is_connected();
                self.connection = connection;
                let mut changes = vec![Change::Connection];
                if !connected {
                    for editor in self.drafts.values_mut().flatten() {
                        editor.connection_lost();
                        changes.push(Change::DraftStatus(editor.draft_id.clone()));
                    }
                    // The service ends terminals with the lease.
                    for session in &mut self.terminals {
                        if !matches!(session.status, TerminalStatus::Ended(_)) {
                            session.status = TerminalStatus::Ended(TerminalEnd::ConnectionLost);
                            changes.push(Change::Terminal(session.objective_id.clone()));
                        }
                    }
                }
                changes
            }
            StoreUpdate::Snapshot {
                workspaces,
                objectives,
                settings,
            } => {
                if self
                    .active_workspace
                    .as_ref()
                    .is_none_or(|id| !workspaces.iter().any(|w| &w.workspace_id == id))
                {
                    self.active_workspace = workspaces.first().map(|w| w.workspace_id.clone());
                }
                self.workspaces = workspaces;
                self.objectives = objectives;
                self.settings = settings;
                let mut changes = vec![Change::Workspaces, Change::Objectives];
                if self
                    .ui
                    .selected
                    .as_ref()
                    .is_some_and(|id| self.objective(id).is_none())
                {
                    self.ui.selected = None;
                    changes.push(Change::Selection);
                }
                changes
            }
            StoreUpdate::WorkspaceOpened(workspace) => {
                self.active_workspace = Some(workspace.workspace_id.clone());
                match self
                    .workspaces
                    .iter_mut()
                    .find(|w| w.workspace_id == workspace.workspace_id)
                {
                    Some(existing) => *existing = workspace,
                    None => self.workspaces.push(workspace),
                }
                vec![Change::Workspaces, Change::Objectives]
            }
            StoreUpdate::ObjectiveChanged(summary) => {
                let id = summary.objective_id.clone();
                self.upsert_summary(summary.clone());
                if let Some(Load::Loaded(detail)) = self.details.get_mut(&id) {
                    detail.summary = summary;
                }
                vec![Change::Objectives, Change::Detail(id)]
            }
            StoreUpdate::ObjectiveDetail(detail) => {
                let id = detail.summary.objective_id.clone();
                self.upsert_summary(detail.summary.clone());
                self.details.insert(id.clone(), Load::Loaded(*detail));
                vec![Change::Objectives, Change::Detail(id)]
            }
            StoreUpdate::ObjectiveLoadFailed {
                objective_id,
                message,
            } => {
                self.details
                    .insert(objective_id.clone(), Load::Failed(message));
                vec![Change::Detail(objective_id)]
            }
            StoreUpdate::ActivityPage {
                objective_id,
                items,
                has_more,
            } => {
                let log = self.activity.entry(objective_id.clone()).or_default();
                // `has_more` speaks about history older than the page: it decides only when the
                // page reaches at least as far back as what is already known (or is empty).
                let known_oldest = log.items.first().map(|i| i.seq);
                let page_oldest = items.iter().map(|i| i.seq).min();
                let decides = match (known_oldest, page_oldest) {
                    (Some(known), Some(page)) => page <= known,
                    _ => true,
                };
                // A page is authoritative for the items it carries: upsert by item id (a resync
                // after missed deltas replaces their stale text), then keep one ascending list.
                let index: HashMap<ActivityItemId, usize> = log
                    .items
                    .iter()
                    .enumerate()
                    .map(|(i, item)| (item.item_id.clone(), i))
                    .collect();
                let mut fresh = Vec::new();
                for item in items {
                    match index.get(&item.item_id) {
                        Some(&i) => log.items[i] = item,
                        None => fresh.push(item),
                    }
                }
                log.items.extend(fresh);
                log.items.sort_by_key(|i| i.seq);
                if decides || !log.loaded {
                    log.has_more = has_more;
                }
                log.loaded = true;
                vec![Change::Activity(objective_id, ActivityChange::Reset)]
            }
            StoreUpdate::ActivityAppended {
                objective_id,
                items,
            } => {
                let log = self.activity.entry(objective_id.clone()).or_default();
                let from_index = log.items.len();
                let mut out_of_order = false;
                for item in items {
                    if let Some(existing) = log.items.iter_mut().find(|i| i.item_id == item.item_id)
                    {
                        *existing = item; // authoritative replacement (e.g. message end)
                        out_of_order = true;
                    } else {
                        out_of_order |= log.items.last().is_some_and(|last| last.seq > item.seq);
                        log.items.push(item);
                    }
                }
                let change = if out_of_order {
                    log.items.sort_by_key(|i| i.seq);
                    ActivityChange::Reset
                } else {
                    ActivityChange::Appended { from_index }
                };
                vec![Change::Activity(objective_id, change)]
            }
            StoreUpdate::ActivityDelta {
                objective_id,
                item_id,
                append_text,
            } => {
                let log = self.activity.entry(objective_id.clone()).or_default();
                match log.items.iter().position(|i| i.item_id == item_id) {
                    Some(index) => {
                        log.items[index].text.push_str(&append_text);
                        vec![Change::Activity(
                            objective_id,
                            ActivityChange::Delta { index },
                        )]
                    }
                    // A delta for an unknown item means we missed its start; re-query instead.
                    None => Vec::new(),
                }
            }
            StoreUpdate::Changes(list) => {
                let id = list.objective_id.clone();
                let current_basis = self.objective_ui_mut(&id).basis.clone();
                if list.basis != current_basis {
                    return Vec::new(); // reply for a basis the developer already left
                }
                self.working_updated.remove(&id);
                let ui = self.objective_ui_mut(&id);
                if ui
                    .file
                    .as_ref()
                    .is_some_and(|f| !list.files.iter().any(|c| &c.path == f))
                {
                    ui.file = None;
                }
                self.changes.insert(id.clone(), Load::Loaded(list));
                vec![Change::Changes(id)]
            }
            StoreUpdate::ChangesFailed {
                objective_id,
                basis,
                message,
            } => {
                if self.objective_ui_mut(&objective_id).basis != basis {
                    return Vec::new();
                }
                self.changes
                    .insert(objective_id.clone(), Load::Failed(message));
                vec![Change::Changes(objective_id)]
            }
            StoreUpdate::WorktreeChanged { objective_id } => {
                // Never replace the inspected capture invisibly: flag it and let the developer refresh.
                self.working_updated.insert(objective_id.clone());
                vec![Change::Changes(objective_id)]
            }
            StoreUpdate::Diff(diff) => {
                let matches = self.viewer.key.as_ref().is_some_and(|k| {
                    k.mode == ViewerMode::Diff
                        && k.objective_id == diff.objective_id
                        && k.basis == diff.basis
                        && k.path == diff.path
                });
                if !matches {
                    return Vec::new();
                }
                self.viewer.content = ViewerContent::Diff(diff);
                vec![Change::Viewer]
            }
            StoreUpdate::FilePage(page) => self.apply_file_page(page),
            StoreUpdate::ViewerFailed {
                objective_id,
                path,
                message,
            } => {
                let matches = self
                    .viewer
                    .key
                    .as_ref()
                    .is_some_and(|k| k.objective_id == objective_id && k.path == path);
                if !matches {
                    return Vec::new();
                }
                self.viewer.content = ViewerContent::Failed(message);
                vec![Change::Viewer]
            }
            StoreUpdate::Drafts {
                objective_id,
                drafts,
            } => {
                let editors = self.drafts.entry(objective_id.clone()).or_default();
                for draft in &drafts {
                    match editors.iter_mut().find(|e| e.draft_id == draft.draft_id) {
                        Some(editor) => {
                            if draft.revision == editor.durable_revision() {
                                editor.reconciled();
                            }
                            editor.acknowledge(draft);
                        }
                        None => editors.push(DraftEditor::from_durable(draft)),
                    }
                }
                // A never-saved local draft the service does not list: its first save (if one
                // was in flight when the link dropped) did not land, so it may be sent again.
                for editor in editors.iter_mut() {
                    if editor.durable_revision() == 0
                        && !drafts.iter().any(|d| d.draft_id == editor.draft_id)
                    {
                        editor.reconciled();
                    }
                }
                // Keep only local drafts never saved (revision 0) besides the durable ones.
                editors.retain(|e| {
                    e.durable_revision() == 0 || drafts.iter().any(|d| d.draft_id == e.draft_id)
                });
                vec![Change::Drafts(objective_id)]
            }
            StoreUpdate::DraftChanged(draft) => {
                let objective_id = draft.objective_id.clone();
                let editors = self.drafts.entry(objective_id.clone()).or_default();
                match editors.iter_mut().find(|e| e.draft_id == draft.draft_id) {
                    Some(editor) => {
                        let was_unsent = editor.state == super::model::DraftState::Unsent;
                        let had_text = editor.body.clone();
                        editor.acknowledge(&draft);
                        if was_unsent != (editor.state == super::model::DraftState::Unsent)
                            || had_text != editor.body
                        {
                            vec![Change::Drafts(objective_id)]
                        } else {
                            vec![Change::DraftStatus(draft.draft_id)]
                        }
                    }
                    None => {
                        editors.push(DraftEditor::from_durable(&draft));
                        vec![Change::Drafts(objective_id)]
                    }
                }
            }
            StoreUpdate::DraftSaveFailed { draft_id, message } => match self.draft_mut(&draft_id) {
                Some(editor) => {
                    editor.fail(&message);
                    vec![Change::DraftStatus(draft_id)]
                }
                None => Vec::new(),
            },
            StoreUpdate::DraftDeleted { draft_id } => {
                let mut changes = Vec::new();
                for (objective_id, editors) in &mut self.drafts {
                    let before = editors.len();
                    editors.retain(|e| e.draft_id != draft_id);
                    if editors.len() != before {
                        changes.push(Change::Drafts(objective_id.clone()));
                    }
                }
                changes
            }
            StoreUpdate::Inputs {
                objective_id,
                inputs,
            } => {
                self.inputs.insert(objective_id.clone(), inputs);
                vec![Change::Inputs(objective_id)]
            }
            StoreUpdate::InputChanged {
                objective_id,
                input,
            } => {
                let inputs = self.inputs.entry(objective_id.clone()).or_default();
                match inputs.iter_mut().find(|i| i.input_id == input.input_id) {
                    Some(existing) => *existing = input,
                    None => inputs.push(input),
                }
                vec![Change::Inputs(objective_id)]
            }
            StoreUpdate::VerificationChanged(verification) => {
                let objective_id = verification.objective_id.clone();
                let Some(Load::Loaded(detail)) = self.details.get_mut(&objective_id) else {
                    return Vec::new();
                };
                match detail
                    .verifications
                    .iter_mut()
                    .find(|v| v.verification_id == verification.verification_id)
                {
                    Some(existing) => *existing = verification,
                    None => detail.verifications.push(verification),
                }
                vec![Change::Detail(objective_id)]
            }
            StoreUpdate::Search(results) => {
                self.search = Load::Loaded(results);
                vec![Change::Search]
            }
            StoreUpdate::SearchFailed { message } => {
                self.search = Load::Failed(message);
                vec![Change::Search]
            }
            StoreUpdate::Notice(notice) => {
                self.push_notice(notice);
                vec![Change::Notices]
            }
            StoreUpdate::Settings(settings) => {
                self.settings = settings;
                let mut changes = vec![Change::Workspaces];
                changes.extend(self.ui.selected.clone().map(Change::Detail));
                changes
            }
            StoreUpdate::Select(objective_id) => {
                if self.ui.selected.as_ref() == Some(&objective_id) {
                    return Vec::new();
                }
                self.ui.selected = Some(objective_id);
                self.viewer.key = None;
                self.viewer.content = ViewerContent::Empty;
                vec![Change::Selection, Change::Viewer]
            }
            StoreUpdate::CommandStatus {
                objective_id,
                status,
            } => {
                let changed = match status {
                    Some(status) => {
                        self.command_status
                            .insert(objective_id.clone(), status.clone())
                            != Some(status)
                    }
                    None => self.command_status.remove(&objective_id).is_some(),
                };
                if changed {
                    vec![Change::Detail(objective_id)]
                } else {
                    Vec::new()
                }
            }
            StoreUpdate::Compat(update) => self
                .compat
                .apply(update)
                .into_iter()
                .map(|change| match change {
                    CompatChange::Catalog => Change::Catalog,
                    CompatChange::Session(id) => Change::Session(id),
                    CompatChange::Resources => Change::Resources,
                    CompatChange::Dialogs => Change::Dialogs,
                })
                .collect(),
            StoreUpdate::TerminalOpened {
                objective_id,
                terminal_id,
                mode,
                cols,
                rows,
            } => {
                let session = match self
                    .terminal_for_mut(&objective_id)
                    .filter(|t| t.status == TerminalStatus::Opening)
                {
                    Some(session) => session,
                    None => {
                        self.push_terminal(TerminalSession::opening(
                            objective_id.clone(),
                            usize::from(cols),
                            usize::from(rows),
                        ));
                        let Some(session) = self.terminals.last_mut() else {
                            return Vec::new();
                        };
                        session
                    }
                };
                session.terminal_id = Some(terminal_id);
                session.mode = Some(mode);
                session.status = TerminalStatus::Open;
                session.problem = None;
                session.screen.resize(usize::from(cols), usize::from(rows));
                vec![Change::Terminal(objective_id)]
            }
            StoreUpdate::TerminalOutput {
                terminal_id,
                seq,
                data,
                dropped,
            } => match self.terminal_by_id(&terminal_id) {
                Some(session) => {
                    session.output(seq, &data, dropped);
                    vec![Change::Terminal(session.objective_id.clone())]
                }
                None => Vec::new(),
            },
            StoreUpdate::TerminalClosed { terminal_id, end } => {
                match self.terminal_by_id(&terminal_id) {
                    Some(session) => {
                        session.status = TerminalStatus::Ended(end);
                        vec![Change::Terminal(session.objective_id.clone())]
                    }
                    None => Vec::new(),
                }
            }
            StoreUpdate::TerminalProblem {
                objective_id,
                message,
            } => {
                match self.terminal_for_mut(&objective_id) {
                    Some(session) => {
                        if session.status == TerminalStatus::Opening {
                            session.status = TerminalStatus::Ended(TerminalEnd::Failed);
                        }
                        session.problem = Some(message);
                    }
                    None => {
                        let mut session = TerminalSession::opening(objective_id.clone(), 80, 24);
                        session.status = TerminalStatus::Ended(TerminalEnd::Failed);
                        session.problem = Some(message);
                        self.push_terminal(session);
                    }
                }
                vec![Change::Terminal(objective_id)]
            }
            StoreUpdate::ServiceStopped => {
                self.service_stopped = true;
                vec![Change::Lifetime]
            }
            StoreUpdate::NotificationsUnavailable { reason } => {
                self.notifications_unavailable = Some(reason);
                vec![Change::DesktopAttention]
            }
        }
    }

    fn upsert_summary(&mut self, summary: ObjectiveSummary) {
        match self
            .objectives
            .iter_mut()
            .find(|o| o.objective_id == summary.objective_id)
        {
            Some(existing) => *existing = summary,
            None => self.objectives.push(summary),
        }
    }

    fn apply_file_page(&mut self, page: FilePage) -> Vec<Change> {
        let Some(key) = self.viewer.key.as_ref() else {
            return Vec::new();
        };
        if key.mode != ViewerMode::FullFile
            || key.objective_id != page.objective_id
            || key.basis != page.basis
            || key.path != page.path
            || key.side != page.side
        {
            return Vec::new();
        }
        let page_len = page.length;
        match &mut self.viewer.content {
            ViewerContent::File(content)
                if content.artifact_digest == page.artifact_digest
                    && page.offset == content.loaded_bytes =>
            {
                content
                    .text
                    .push_str(page.text.as_deref().unwrap_or_default());
                content.loaded_bytes += page_len;
                content.eof = page.eof;
            }
            ViewerContent::File(content) if content.artifact_digest == page.artifact_digest => {
                return Vec::new(); // duplicate or non-contiguous page
            }
            content => {
                if page.offset != 0 {
                    return Vec::new();
                }
                *content = ViewerContent::File(FileContent {
                    objective_id: page.objective_id,
                    basis: page.basis,
                    path: page.path,
                    side: page.side,
                    artifact_digest: page.artifact_digest,
                    total_bytes: page.total_bytes,
                    encoding: page.encoding,
                    text: page.text.unwrap_or_default(),
                    loaded_bytes: page_len,
                    eof: page.eof,
                    version_label: page.version_label,
                    chunked: page.chunked,
                });
            }
        }
        vec![Change::Viewer]
    }
}

/// Merges adjacent deltas for the same activity item so bursts cost one view update.
pub fn coalesce(updates: Vec<StoreUpdate>) -> Vec<StoreUpdate> {
    let mut out: Vec<StoreUpdate> = Vec::with_capacity(updates.len());
    for update in updates {
        if let (
            Some(StoreUpdate::ActivityDelta {
                objective_id: last_objective,
                item_id: last_item,
                append_text: last_text,
            }),
            StoreUpdate::ActivityDelta {
                objective_id,
                item_id,
                append_text,
            },
        ) = (out.last_mut(), &update)
            && last_objective == objective_id
            && last_item == item_id
        {
            last_text.push_str(append_text);
            continue;
        }
        out.push(update);
    }
    out
}

type Listener = Rc<dyn Fn(&[Change])>;

/// Main-thread store handle with change notification.
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

    /// Mutates transient state; the closure returns what changed.
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
mod tests {
    use super::*;
    use crate::presentation::model::{ActivityRole, ExecutionState, Lifecycle, Timestamp};

    fn summary(
        id: &str,
        attention: Attention,
        lifecycle: Lifecycle,
        updated: i64,
    ) -> ObjectiveSummary {
        ObjectiveSummary {
            objective_id: ObjectiveId::new(id),
            workspace_id: WorkspaceId::new("w"),
            title: id.to_owned(),
            outcome: String::new(),
            lifecycle,
            execution: crate::presentation::model::Execution::new(ExecutionState::None, 0),
            attention,
            badges: Vec::new(),
            updated_at: Timestamp(updated),
            revision: 1,
        }
    }

    fn item(id: &str, seq: u64, text: &str) -> ActivityItem {
        ActivityItem {
            item_id: ActivityItemId::new(id),
            seq,
            at: Timestamp(0),
            role: ActivityRole::Assistant,
            title: String::new(),
            text: text.to_owned(),
            tool_name: None,
            is_error: false,
            truncated: false,
        }
    }

    #[test]
    fn board_orders_groups_by_attention_and_archives_terminal_lifecycles() {
        let mut state = PresentationState::new(ConnectionState::Connected);
        state.apply(StoreUpdate::Snapshot {
            workspaces: Vec::new(),
            objectives: vec![
                summary("idle", Attention::Idle, Lifecycle::Defined, 5),
                summary("working", Attention::Working, Lifecycle::Active, 4),
                summary("ready-old", Attention::ReadyForReview, Lifecycle::Frozen, 1),
                summary(
                    "ready-new",
                    Attention::ReadyForReview,
                    Lifecycle::Approved,
                    9,
                ),
                summary("needs", Attention::NeedsAttention, Lifecycle::Active, 2),
                summary("done", Attention::Idle, Lifecycle::Integrated, 7),
                summary("gone", Attention::Idle, Lifecycle::Discarded, 8),
            ],
            settings: Settings::default(),
        });
        let board = state.board();
        let order: Vec<(Attention, Vec<&str>)> = board
            .groups
            .iter()
            .map(|(a, g)| (*a, g.iter().map(|o| o.title.as_str()).collect()))
            .collect();
        assert_eq!(
            order,
            vec![
                (Attention::NeedsAttention, vec!["needs"]),
                (Attention::ReadyForReview, vec!["ready-new", "ready-old"]),
                (Attention::Working, vec!["working"]),
                (Attention::Idle, vec!["idle"]),
            ]
        );
        let archive: Vec<&str> = board.archive.iter().map(|o| o.title.as_str()).collect();
        assert_eq!(archive, vec!["gone", "done"]);
    }

    #[test]
    fn file_pages_advance_by_the_reply_length() {
        use crate::presentation::model::{ArtifactDigest, FileEncoding, FilePage, RepoPath, Side};
        let mut state = PresentationState::default();
        let o = ObjectiveId::new("o");
        let path = RepoPath::utf8("a.txt");
        state.viewer.key = Some(ViewerKey {
            objective_id: o.clone(),
            basis: Basis::Working,
            path: path.clone(),
            mode: ViewerMode::FullFile,
            side: Side::New,
        });
        let page = |offset: u64, length: u64, text: &str, eof: bool| {
            StoreUpdate::FilePage(FilePage {
                objective_id: o.clone(),
                basis: Basis::Working,
                path: path.clone(),
                side: Side::New,
                artifact_digest: ArtifactDigest::new("d"),
                total_bytes: 10,
                offset,
                length,
                encoding: FileEncoding::Utf8,
                text: Some(text.into()),
                eof,
                version_label: "working".into(),
                chunked: false,
            })
        };
        state.apply(page(0, 6, "héllo", false));
        let ViewerContent::File(content) = &state.viewer.content else {
            panic!("file content");
        };
        assert_eq!(content.loaded_bytes, 6);
        state.apply(page(6, 4, "rest", true));
        let ViewerContent::File(content) = &state.viewer.content else {
            panic!("file content");
        };
        assert_eq!((content.loaded_bytes, content.eof), (10, true));
        assert_eq!(content.text, "héllorest");
    }

    #[test]
    fn a_resynced_activity_page_replaces_items_by_id() {
        let mut state = PresentationState::default();
        let o = ObjectiveId::new("o");
        state.apply(StoreUpdate::ActivityPage {
            objective_id: o.clone(),
            items: vec![item("a", 1, "hi"), item("b", 2, "par")],
            has_more: false,
        });
        // Deltas for "b" were missed while disconnected; the re-fetched page carries the full
        // text and a new item.
        state.apply(StoreUpdate::ActivityPage {
            objective_id: o.clone(),
            items: vec![item("b", 2, "partial text"), item("c", 3, "new")],
            has_more: true,
        });
        let log = &state.activity[&o];
        let texts: Vec<&str> = log.items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, ["hi", "partial text", "new"]);
        assert!(!log.has_more, "a newer page does not reopen older history");
        // An older page that reaches back further decides `has_more`.
        state.apply(StoreUpdate::ActivityPage {
            objective_id: o.clone(),
            items: vec![item("z", 0, "first")],
            has_more: false,
        });
        assert_eq!(state.activity[&o].items.len(), 4);
        assert_eq!(state.activity[&o].items[0].text, "first");
    }

    #[test]
    fn activity_appends_incrementally_and_applies_deltas_in_place() {
        let mut state = PresentationState::default();
        let o = ObjectiveId::new("o");
        state.apply(StoreUpdate::ActivityPage {
            objective_id: o.clone(),
            items: vec![item("a", 1, "hi")],
            has_more: true,
        });
        let changes = state.apply(StoreUpdate::ActivityAppended {
            objective_id: o.clone(),
            items: vec![item("b", 2, "par")],
        });
        assert_eq!(
            changes,
            vec![Change::Activity(
                o.clone(),
                ActivityChange::Appended { from_index: 1 }
            )]
        );
        let changes = state.apply(StoreUpdate::ActivityDelta {
            objective_id: o.clone(),
            item_id: ActivityItemId::new("b"),
            append_text: "tial".into(),
        });
        assert_eq!(
            changes,
            vec![Change::Activity(
                o.clone(),
                ActivityChange::Delta { index: 1 }
            )]
        );
        assert_eq!(state.activity[&o].items[1].text, "partial");
        // Unknown item deltas are ignored rather than inventing an item.
        assert!(
            state
                .apply(StoreUpdate::ActivityDelta {
                    objective_id: o.clone(),
                    item_id: ActivityItemId::new("zz"),
                    append_text: "x".into(),
                })
                .is_empty()
        );
    }

    #[test]
    fn deltas_coalesce_per_item() {
        let o = ObjectiveId::new("o");
        let delta = |id: &str, t: &str| StoreUpdate::ActivityDelta {
            objective_id: o.clone(),
            item_id: ActivityItemId::new(id),
            append_text: t.into(),
        };
        let merged = coalesce(vec![
            delta("a", "1"),
            delta("a", "2"),
            delta("b", "3"),
            delta("a", "4"),
        ]);
        assert_eq!(
            merged,
            vec![delta("a", "12"), delta("b", "3"), delta("a", "4")]
        );
    }

    #[test]
    fn stale_changes_reply_for_previous_basis_is_ignored() {
        let mut state = PresentationState::default();
        let o = ObjectiveId::new("o");
        state.objective_ui_mut(&o).basis = Basis::Candidate {
            candidate_id: crate::presentation::model::CandidateId::new("c2"),
        };
        let list = ChangeList {
            objective_id: o.clone(),
            basis: Basis::Working,
            basis_label: "Working inspection".into(),
            target: crate::presentation::model::TargetRef {
                branch: "main".into(),
                commit: crate::presentation::model::GitId::sha1("abc"),
            },
            capture_id: None,
            candidate_id: None,
            files: Vec::new(),
            truncated: false,
            total_added: 0,
            total_removed: 0,
            notices: Vec::new(),
        };
        assert!(state.apply(StoreUpdate::Changes(list)).is_empty());
        assert!(!state.changes.contains_key(&o));
    }

    #[test]
    fn worktree_change_flags_instead_of_replacing() {
        let mut state = PresentationState::default();
        let o = ObjectiveId::new("o");
        state.apply(StoreUpdate::WorktreeChanged {
            objective_id: o.clone(),
        });
        assert!(state.working_updated.contains(&o));
    }

    #[test]
    fn store_notifies_listeners_once_per_change() {
        let store = Store::new(PresentationState::default());
        let seen = Rc::new(RefCell::new(Vec::new()));
        let sink = seen.clone();
        store.subscribe(move |changes| sink.borrow_mut().extend_from_slice(changes));
        store.apply_all(vec![
            StoreUpdate::Connection(ConnectionState::Connected),
            StoreUpdate::Connection(ConnectionState::Reconnecting { attempt: 1 }),
        ]);
        assert_eq!(*seen.borrow(), vec![Change::Connection]);
        assert_eq!(
            store.state().connection,
            ConnectionState::Reconnecting { attempt: 1 }
        );
    }

    #[test]
    fn never_saved_draft_unlisted_after_reconnect_is_saved_again() {
        use crate::presentation::drafts::DraftEditor;
        use crate::presentation::model::{
            ArtifactDigest, CaptureId, DraftId, Reference, ReferenceVersion, RepoPath, SessionId,
            Side,
        };
        let mut state = PresentationState::new(ConnectionState::Connected);
        let o = ObjectiveId::new("o");
        let reference = Reference {
            artifact_digest: ArtifactDigest::new("digest"),
            path: RepoPath::utf8("a.txt"),
            version: ReferenceVersion::Working {
                capture_id: CaptureId::new("cap"),
            },
            side: Side::New,
            byte_start: 0,
            byte_end: 1,
            selected_text: "a".into(),
            line_hint: Some(1),
        };
        let id = DraftId::new("d");
        let mut editor =
            DraftEditor::new_local(id.clone(), o.clone(), SessionId::new("s"), reference);
        editor.edit("typed", 0);
        assert!(editor.poll(300, true).is_some());
        state.drafts.insert(o.clone(), vec![editor]);
        state.apply(StoreUpdate::Connection(ConnectionState::Reconnecting {
            attempt: 1,
        }));
        state.apply(StoreUpdate::Connection(ConnectionState::Connected));
        let draft = state.draft_mut(&id).unwrap();
        assert!(
            draft.poll(10_000, true).is_none(),
            "outcome unknown until drafts are listed"
        );
        // The service lists no such draft: the first save never landed.
        state.apply(StoreUpdate::Drafts {
            objective_id: o.clone(),
            drafts: Vec::new(),
        });
        let draft = state.draft_mut(&id).expect("local text kept");
        assert_eq!(draft.body, "typed");
        let request = draft.poll(10_000, true).expect("saved again");
        assert_eq!(request.expected_revision, 0);
    }
}
