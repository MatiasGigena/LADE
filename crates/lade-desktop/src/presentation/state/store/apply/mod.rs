//! Applying a [`StoreUpdate`] to the [`PresentationState`] and reporting what changed.

use super::activity::{ActivityChange, ActivityLog};
use super::ui::{ViewerContent, ViewerMode};
use super::update::StoreUpdate;
use super::{Change, PresentationState};
use crate::presentation::state::drafts::DraftEditor;
use crate::presentation::state::model::{
    ActionSurface, Basis, ChangeList, ConnectionState, Draft, DraftId, DraftState, FileContent,
    FileDiff, FilePage, InputSummary, Load, NoticeLevel, NoticeScope, ObjectiveDetail, ObjectiveId,
    ObjectiveSummary, Refusal, RepoPath, ServiceNotice, Settings, VerificationSummary, Workspace,
};
use crate::presentation::terminal::{TerminalEnd, TerminalMode, TerminalSession, TerminalStatus};

pub(super) fn refusal_changes(objective_id: ObjectiveId, surface: ActionSurface) -> Vec<Change> {
    let mut changes = vec![Change::Detail(objective_id.clone())];
    match surface {
        ActionSurface::Terminal => changes.push(Change::Terminal(objective_id)),
        ActionSurface::Drafts | ActionSurface::Feedback => {
            changes.push(Change::Drafts(objective_id));
        }
        _ => {}
    }
    changes
}

fn reconcile_drafts(editors: &mut Vec<DraftEditor>, drafts: &[Draft]) {
    for draft in drafts {
        match editors.iter_mut().find(|e| e.draft_id == draft.id) {
            Some(editor) => {
                if draft.revision == editor.durable_revision() {
                    editor.reconciled();
                }
                editor.acknowledge(draft);
            }
            None => editors.push(DraftEditor::from_durable(draft)),
        }
    }
    let listed = |editor: &DraftEditor| drafts.iter().any(|d| d.id == editor.draft_id);
    for editor in editors.iter_mut() {
        if editor.durable_revision() == 0 && !listed(editor) {
            editor.reconciled();
        }
    }
    editors.retain(|e| e.durable_revision() == 0 || listed(e));
}

impl PresentationState {
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per StoreUpdate variant, each a delegation or a single assignment"
    )]
    pub fn apply(&mut self, update: StoreUpdate) -> Vec<Change> {
        match update {
            StoreUpdate::Connection(connection) => self.apply_connection(connection),
            StoreUpdate::Snapshot {
                workspaces,
                objectives,
                settings,
            } => self.apply_snapshot(workspaces, objectives, settings),
            StoreUpdate::WorkspaceOpened(workspace) => self.apply_workspace_opened(workspace),
            StoreUpdate::ObjectiveChanged(summary) => self.apply_objective_changed(summary),
            StoreUpdate::ObjectiveDetail(detail) => self.apply_objective_detail(*detail),
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
                self.activity_log(&objective_id).merge_page(items, has_more);
                vec![Change::Activity(objective_id, ActivityChange::Reset)]
            }
            StoreUpdate::ActivityAppended {
                objective_id,
                items,
            } => {
                let change = self.activity_log(&objective_id).append(items);
                vec![Change::Activity(objective_id, change)]
            }
            StoreUpdate::ActivityDelta {
                objective_id,
                item_id,
                append_text,
            } => self
                .activity_log(&objective_id)
                .append_text(&item_id, &append_text)
                .map(|index| Change::Activity(objective_id, ActivityChange::Delta { index }))
                .into_iter()
                .collect(),
            StoreUpdate::Changes(list) => self.apply_changes(list),
            StoreUpdate::ChangesFailed {
                objective_id,
                basis,
                message,
            } => self.apply_changes_failed(objective_id, &basis, message),
            StoreUpdate::WorktreeChanged { objective_id } => {
                self.working_updated.insert(objective_id.clone());
                vec![Change::Changes(objective_id)]
            }
            StoreUpdate::Diff(diff) => self.apply_diff(diff),
            StoreUpdate::FilePage(page) => self.apply_file_page(page),
            StoreUpdate::ViewerFailed {
                objective_id,
                path,
                message,
            } => self.apply_viewer_failed(&objective_id, &path, message),
            StoreUpdate::Drafts {
                objective_id,
                drafts,
            } => {
                reconcile_drafts(
                    self.drafts.entry(objective_id.clone()).or_default(),
                    &drafts,
                );
                vec![Change::Drafts(objective_id)]
            }
            StoreUpdate::DraftChanged(draft) => self.apply_draft_changed(&draft),
            StoreUpdate::DraftSaveFailed { draft_id, message } => {
                self.draft_failed(draft_id, |editor| editor.fail(&message))
            }
            StoreUpdate::DraftSaveRefused { draft_id, refusal } => {
                self.draft_failed(draft_id, |editor| editor.fail_with(&refusal))
            }
            StoreUpdate::DraftDeleteRefused { draft_id, refusal } => {
                self.apply_draft_delete_refused(&draft_id, refusal)
            }
            StoreUpdate::DraftDeleted { draft_id } => self.remove_draft(&draft_id),
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
            } => self.apply_input_changed(objective_id, input),
            StoreUpdate::VerificationChanged(verification) => {
                self.apply_verification_changed(verification)
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
            StoreUpdate::Select(objective_id) => self.apply_select(objective_id),
            StoreUpdate::CommandStatus {
                objective_id,
                status,
            } => self.apply_command_status(objective_id, status),
            StoreUpdate::Refused {
                objective_id,
                surface,
                refusal,
            } => self.keep_refusal(objective_id, surface, refusal),
            StoreUpdate::RefusalCleared {
                objective_id,
                surface,
            } => match self.refusals.remove(&(objective_id.clone(), surface)) {
                Some(_) => refusal_changes(objective_id, surface),
                None => Vec::new(),
            },
            StoreUpdate::AgentSession(update) => self
                .agent_session
                .apply(update)
                .into_iter()
                .map(Change::from)
                .collect(),
            StoreUpdate::TerminalOpened {
                objective_id,
                terminal_id,
                mode,
                cols,
                rows,
            } => self.apply_terminal_opened(objective_id, terminal_id, mode, cols, rows),
            StoreUpdate::TerminalOutput {
                terminal_id,
                seq,
                data,
                dropped,
            } => self.terminal_changed(&terminal_id, |session| {
                session.output(seq, &data, dropped);
            }),
            StoreUpdate::TerminalClosed { terminal_id, end } => {
                self.terminal_changed(&terminal_id, |session| {
                    session.status = TerminalStatus::Ended(end);
                })
            }
            StoreUpdate::TerminalProblem {
                objective_id,
                message,
            } => self.terminal_problem(
                objective_id,
                Refusal {
                    code: "terminal_problem",
                    message,
                    details: String::new(),
                },
            ),
            StoreUpdate::TerminalRefused {
                objective_id,
                refusal,
            } => self.terminal_problem(objective_id, refusal),
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

    fn activity_log(&mut self, objective_id: &ObjectiveId) -> &mut ActivityLog {
        self.activity.entry(objective_id.clone()).or_default()
    }

    /// Whether `summary` is older than the Objective already held. An Objective's revision only
    /// grows, so an older summary comes from a reply or recovered outcome that fresher state has
    /// overtaken, and applying it would roll the Objective back.
    fn superseded(&self, summary: &ObjectiveSummary) -> bool {
        let id = &summary.objective_id;
        let listed = self.objective(id).map(|o| o.revision);
        let detailed = self.detail(id).map(|d| d.summary.revision);
        listed
            .max(detailed)
            .is_some_and(|held| summary.revision < held)
    }

    fn apply_objective_changed(&mut self, summary: ObjectiveSummary) -> Vec<Change> {
        if self.superseded(&summary) {
            return Vec::new();
        }
        let id = summary.objective_id.clone();
        self.objective_revised(&id, summary.revision);
        self.upsert_summary(summary.clone());
        if let Some(Load::Loaded(detail)) = self.details.get_mut(&id) {
            detail.summary = summary;
        }
        vec![Change::Objectives, Change::Detail(id)]
    }

    fn apply_objective_detail(&mut self, detail: ObjectiveDetail) -> Vec<Change> {
        if self.superseded(&detail.summary) {
            return Vec::new();
        }
        let id = detail.summary.objective_id.clone();
        self.objective_revised(&id, detail.summary.revision);
        self.upsert_summary(detail.summary.clone());
        self.details.insert(id.clone(), Load::Loaded(detail));
        vec![Change::Objectives, Change::Detail(id)]
    }

    /// A failed Change Set load only counts for the basis the objective still shows.
    fn apply_changes_failed(
        &mut self,
        objective_id: ObjectiveId,
        basis: &Basis,
        message: String,
    ) -> Vec<Change> {
        if &self.objective_ui_mut(&objective_id).basis != basis {
            return Vec::new();
        }
        self.changes
            .insert(objective_id.clone(), Load::Failed(message));
        vec![Change::Changes(objective_id)]
    }

    fn apply_diff(&mut self, diff: FileDiff) -> Vec<Change> {
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

    fn apply_viewer_failed(
        &mut self,
        objective_id: &ObjectiveId,
        path: &RepoPath,
        message: String,
    ) -> Vec<Change> {
        let matches = self
            .viewer
            .key
            .as_ref()
            .is_some_and(|k| &k.objective_id == objective_id && &k.path == path);
        if !matches {
            return Vec::new();
        }
        self.viewer.content = ViewerContent::Failed(message);
        vec![Change::Viewer]
    }

    fn draft_failed(
        &mut self,
        draft_id: DraftId,
        fail: impl FnOnce(&mut DraftEditor),
    ) -> Vec<Change> {
        match self.draft_mut(&draft_id) {
            Some(editor) => {
                fail(editor);
                vec![Change::DraftStatus(draft_id)]
            }
            None => Vec::new(),
        }
    }

    fn apply_input_changed(
        &mut self,
        objective_id: ObjectiveId,
        input: InputSummary,
    ) -> Vec<Change> {
        let inputs = self.inputs.entry(objective_id.clone()).or_default();
        match inputs.iter_mut().find(|i| i.input_id == input.input_id) {
            Some(existing) => *existing = input,
            None => inputs.push(input),
        }
        vec![Change::Inputs(objective_id)]
    }

    fn apply_verification_changed(&mut self, verification: VerificationSummary) -> Vec<Change> {
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

    fn apply_select(&mut self, objective_id: ObjectiveId) -> Vec<Change> {
        if self.ui.selected.as_ref() == Some(&objective_id) {
            return Vec::new();
        }
        self.ui.selected = Some(objective_id);
        self.viewer.key = None;
        self.viewer.content = ViewerContent::Empty;
        vec![Change::Selection, Change::Viewer]
    }

    fn terminal_changed(
        &mut self,
        terminal_id: &str,
        change: impl FnOnce(&mut TerminalSession),
    ) -> Vec<Change> {
        match self.terminal_by_id_mut(terminal_id) {
            Some(session) => {
                change(session);
                vec![Change::Terminal(session.objective_id.clone())]
            }
            None => Vec::new(),
        }
    }

    fn apply_connection(&mut self, connection: ConnectionState) -> Vec<Change> {
        let connected = connection.is_connected();
        self.connection = connection;
        let mut changes = vec![Change::Connection];
        if connected {
            return changes;
        }
        for editor in self.drafts.values_mut().flatten() {
            editor.connection_lost();
            changes.push(Change::DraftStatus(editor.draft_id.clone()));
        }
        self.terminals.retain(|t| {
            let pending = t.status == TerminalStatus::Opening && t.terminal_id.is_none();
            if pending {
                changes.push(Change::Terminal(t.objective_id.clone()));
            }
            !pending
        });
        for session in &mut self.terminals {
            if !matches!(session.status, TerminalStatus::Ended(_)) {
                session.status = TerminalStatus::Ended(TerminalEnd::ConnectionLost);
                changes.push(Change::Terminal(session.objective_id.clone()));
            }
        }
        if !self.agent_session.dialogs.is_empty() {
            self.agent_session.dialogs.clear();
            changes.push(Change::Dialogs);
        }
        changes
    }

    fn apply_snapshot(
        &mut self,
        workspaces: Vec<Workspace>,
        objectives: Vec<ObjectiveSummary>,
        settings: Settings,
    ) -> Vec<Change> {
        if self
            .active_workspace
            .as_ref()
            .is_none_or(|id| !workspaces.iter().any(|w| &w.id == id))
        {
            self.active_workspace = workspaces.first().map(|w| w.id.clone());
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

    fn apply_workspace_opened(&mut self, workspace: Workspace) -> Vec<Change> {
        self.active_workspace = Some(workspace.id.clone());
        match self.workspaces.iter_mut().find(|w| w.id == workspace.id) {
            Some(existing) => *existing = workspace,
            None => self.workspaces.push(workspace),
        }
        vec![
            Change::Workspaces,
            Change::WorkspaceOpened,
            Change::Objectives,
        ]
    }

    fn apply_changes(&mut self, list: ChangeList) -> Vec<Change> {
        let id = list.objective_id.clone();
        let ui = self.objective_ui_mut(&id);
        if list.basis != ui.basis {
            return Vec::new();
        }
        if ui
            .file
            .as_ref()
            .is_some_and(|f| !list.files.iter().any(|c| &c.path == f))
        {
            ui.file = None;
        }
        self.working_updated.remove(&id);
        self.changes.insert(id.clone(), Load::Loaded(list));
        vec![Change::Changes(id)]
    }

    fn apply_draft_changed(&mut self, draft: &Draft) -> Vec<Change> {
        let objective_id = draft.objective_id.clone();
        let editors = self.drafts.entry(objective_id.clone()).or_default();
        let Some(editor) = editors.iter_mut().find(|e| e.draft_id == draft.id) else {
            editors.push(DraftEditor::from_durable(draft));
            return vec![Change::Drafts(objective_id)];
        };
        let was_unsent = editor.state == DraftState::Unsent;
        let had_text = editor.body.clone();
        editor.acknowledge(draft);
        if was_unsent != (editor.state == DraftState::Unsent) || had_text != editor.body {
            vec![Change::Drafts(objective_id)]
        } else {
            vec![Change::DraftStatus(draft.id.clone())]
        }
    }

    fn apply_draft_delete_refused(&mut self, draft_id: &DraftId, refusal: Refusal) -> Vec<Change> {
        if let Some(objective_id) = self.draft_mut(draft_id).map(|d| d.objective_id.clone()) {
            return self.keep_refusal(objective_id, ActionSurface::Drafts, refusal);
        }
        self.push_notice(ServiceNotice {
            level: NoticeLevel::Warning,
            code: refusal.code.to_owned(),
            message: refusal.message,
            scope: NoticeScope::General,
        });
        vec![Change::Notices]
    }

    pub fn remove_draft(&mut self, draft_id: &DraftId) -> Vec<Change> {
        let mut changes = Vec::new();
        for (objective_id, editors) in &mut self.drafts {
            let before = editors.len();
            editors.retain(|e| &e.draft_id != draft_id);
            if editors.len() != before {
                changes.push(Change::Drafts(objective_id.clone()));
            }
        }
        changes
    }

    fn apply_command_status(
        &mut self,
        objective_id: ObjectiveId,
        status: Option<ServiceNotice>,
    ) -> Vec<Change> {
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

    fn apply_terminal_opened(
        &mut self,
        objective_id: ObjectiveId,
        terminal_id: String,
        mode: TerminalMode,
        cols: u16,
        rows: u16,
    ) -> Vec<Change> {
        if self
            .terminal_for(&objective_id)
            .is_none_or(|t| t.status != TerminalStatus::Opening)
        {
            self.push_terminal(TerminalSession::opening(
                objective_id.clone(),
                usize::from(cols),
                usize::from(rows),
            ));
        }
        let Some(session) = self.terminal_for_mut(&objective_id) else {
            return Vec::new();
        };
        session.terminal_id = Some(terminal_id);
        session.mode = Some(mode);
        session.status = TerminalStatus::Open;
        session.screen.resize(usize::from(cols), usize::from(rows));
        self.refusals
            .remove(&(objective_id.clone(), ActionSurface::Terminal));
        vec![Change::Terminal(objective_id)]
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
                return Vec::new();
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

#[cfg(test)]
mod tests;
