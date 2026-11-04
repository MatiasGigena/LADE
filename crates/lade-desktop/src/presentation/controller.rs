use std::path::PathBuf;
use std::rc::Rc;

use super::keys::Keymap;
use super::layout::{LayoutClass, Region};
use super::state::agent_session::{Readiness, ThinkingLevel};
use super::state::drafts::{DraftEditor, Millis, SaveStatus};
use super::state::model::{
    ActionAvailability, ActionName, ApprovalState, Basis, CandidateSummary, ChangeStatus,
    DeliveryMode, DraftId, ExecutionState, Intent as ObjectiveIntent, Load, ModelRef,
    ObjectiveDetail, ObjectiveId, Reference, ReferenceVersion, RepoPath, SearchResult,
    SearchResultKind, SearchScope, SessionInfo, Side, VerificationState, VerificationSummary,
};
use super::state::store::{
    Change, InspectorTab, MainView, Reveal, Store, ViewerContent, ViewerKey, ViewerMode,
};
use super::terminal::{TerminalSession, TerminalStatus, input_chunks};
use super::text::format;
use super::text::map::{MappedSelection, SelectionError};
use intents::{DraftRevisionRef, Intent, IntentSink, StartPoint, StopReason};

mod agent_session;
pub mod intents;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchItem {
    pub draft_id: DraftId,
    pub revision: u64,
    pub reference_label: String,
    pub body: String,
    pub stale: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedbackBatch {
    pub items: Vec<BatchItem>,
    pub deferred: usize,
    pub destination: Option<SessionInfo>,
    pub mode: DeliveryMode,
    pub blockers: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsavedDraft {
    pub draft_id: DraftId,
    pub objective: String,
    pub reference_label: String,
    pub status: String,
    pub pending: bool,
}

pub const MAX_FEEDBACK_BATCH: usize = 256;

pub struct Controller {
    pub store: Rc<Store>,
    sink: Rc<dyn IntentSink>,
}

impl Controller {
    pub fn new(store: Rc<Store>, sink: Rc<dyn IntentSink>) -> Rc<Self> {
        let controller = Rc::new(Self { store, sink });
        let weak = Rc::downgrade(&controller);
        controller.store.subscribe(move |changes| {
            if let Some(controller) = weak.upgrade() {
                controller.follow_store(changes);
            }
        });
        controller
    }

    fn emit(&self, intent: Intent) {
        self.sink.submit(intent);
    }

    fn selected(&self) -> Option<ObjectiveId> {
        self.store.state().ui.selected.clone()
    }

    pub fn retry_connection(&self) {
        self.emit(Intent::RetryConnection);
    }

    pub fn open_workspace(&self, path: PathBuf) {
        self.emit(Intent::OpenWorkspace { path });
    }

    pub fn set_view(&self, view: MainView) {
        let objective = self.store.update(|s| {
            let mut changes = Vec::new();
            if s.ui.view != view {
                s.ui.view = view;
                changes.push(Change::View);
            }
            if s.ui.layout == LayoutClass::Compact && s.ui.compact_region != Region::Main {
                s.ui.compact_region = Region::Main;
                changes.push(Change::Layout);
            }
            (s.ui.selected.clone(), changes)
        });
        if view == MainView::Activity
            && let Some(id) = objective
        {
            self.ensure_activity(&id);
        }
    }

    pub fn set_inspector_tab(&self, tab: InspectorTab) {
        self.store.update(|s| {
            let changed = s.ui.inspector_tab != tab;
            s.ui.inspector_tab = tab;
            (
                (),
                if changed {
                    vec![Change::InspectorTab]
                } else {
                    Vec::new()
                },
            )
        });
    }

    pub fn set_keymap(&self, keymap: Keymap) {
        self.store.update(|s| {
            if s.ui.keymap == keymap {
                return ((), Vec::new());
            }
            s.ui.keymap = keymap;
            ((), vec![Change::Keymap])
        });
    }

    pub fn set_layout_width(&self, width: i32) {
        let class = LayoutClass::for_width(width);
        self.store.update(|s| {
            if s.ui.layout == class {
                return ((), Vec::new());
            }
            s.ui.layout = class;
            ((), vec![Change::Layout])
        });
    }

    pub fn show_region(&self, region: Region) {
        self.store.update(|s| {
            if s.ui.compact_region == region {
                return ((), Vec::new());
            }
            s.ui.compact_region = region;
            ((), vec![Change::Layout])
        });
    }

    pub fn set_compact_details(&self, expanded: bool) {
        self.store.update(|s| {
            if s.ui.compact_details == expanded {
                return ((), Vec::new());
            }
            s.ui.compact_details = expanded;
            ((), vec![Change::Layout])
        });
    }

    pub fn set_archive_expanded(&self, expanded: bool) {
        self.store.update(|s| {
            s.ui.archive_expanded = expanded;
            ((), Vec::new())
        });
    }

    fn ensure_activity(&self, id: &ObjectiveId) {
        let loaded = self
            .store
            .state()
            .activity
            .get(id)
            .is_some_and(|l| l.loaded);
        if !loaded {
            self.emit(Intent::LoadActivity {
                objective_id: id.clone(),
                before: None,
            });
        }
    }

    pub fn select_objective(&self, id: &ObjectiveId) {
        let (changed, view, basis, has_changes, has_inputs, has_drafts) = self.store.update(|s| {
            let changed = s.ui.selected.as_ref() != Some(id);
            s.ui.selected = Some(id.clone());
            let basis = s.objective_ui_mut(id).basis.clone();
            let result = (
                changed,
                s.ui.view,
                basis,
                matches!(s.changes.get(id), Some(Load::Loaded(_))),
                s.inputs.contains_key(id),
                s.drafts.contains_key(id),
            );
            (
                result,
                if changed {
                    vec![Change::Selection]
                } else {
                    Vec::new()
                },
            )
        });
        if !changed {
            return;
        }
        self.emit(Intent::LoadObjective {
            objective_id: id.clone(),
        });
        if !has_changes {
            self.load_changes(id, basis);
        }
        if !has_drafts {
            self.emit(Intent::LoadDrafts {
                objective_id: id.clone(),
            });
        }
        if !has_inputs {
            self.emit(Intent::LoadInputs {
                objective_id: id.clone(),
            });
        }
        if view == MainView::Activity {
            self.ensure_activity(id);
        }
        let file = self
            .store
            .state()
            .selected_ui()
            .and_then(|u| u.file.clone());
        if let Some(path) = file {
            self.load_viewer(id, &path);
        } else {
            self.clear_viewer();
        }
    }

    fn load_changes(&self, id: &ObjectiveId, basis: Basis) {
        self.store.update(|s| {
            s.changes.insert(id.clone(), Load::Loading);
            ((), vec![Change::Changes(id.clone())])
        });
        self.emit(Intent::LoadChanges {
            objective_id: id.clone(),
            basis,
        });
    }

    pub fn refresh_changes(&self) {
        let Some(id) = self.selected() else { return };
        let basis = self.store.state().selected_ui().map(|u| u.basis.clone());
        self.load_changes(&id, basis.unwrap_or(Basis::Working));
        self.reload_viewer();
    }

    pub fn reload_viewer(&self) {
        let Some(id) = self.selected() else { return };
        let file = self
            .store
            .state()
            .selected_ui()
            .and_then(|u| u.file.clone());
        if let Some(path) = file {
            self.load_viewer(&id, &path);
        }
    }

    pub fn select_basis(&self, basis: Basis) {
        let Some(id) = self.selected() else { return };
        let changed = self.store.update(|s| {
            let ui = s.objective_ui_mut(&id);
            let changed = ui.basis != basis;
            ui.basis = basis.clone();
            ui.selection = None;
            (changed, vec![Change::ViewerSelection])
        });
        if changed {
            self.load_changes(&id, basis);
            let file = self
                .store
                .state()
                .selected_ui()
                .and_then(|u| u.file.clone());
            match file {
                Some(path) => self.load_viewer(&id, &path),
                None => self.clear_viewer(),
            }
        }
    }

    fn clear_viewer(&self) {
        self.store.update(|s| {
            s.viewer.key = None;
            s.viewer.content = ViewerContent::Empty;
            ((), vec![Change::Viewer])
        });
    }

    pub fn select_file(&self, path: &RepoPath) {
        let Some(id) = self.selected() else { return };
        let changed = self.store.update(|s| {
            let ui = s.objective_ui_mut(&id);
            let changed = ui.file.as_ref() != Some(path);
            ui.file = Some(path.clone());
            (changed, Vec::new())
        });
        if changed {
            self.load_viewer(&id, path);
        }
    }

    pub fn set_viewer_mode(&self, mode: ViewerMode) {
        let Some(id) = self.selected() else { return };
        let (changed, file) = self.store.update(|s| {
            let ui = s.objective_ui_mut(&id);
            let changed = ui.mode != mode;
            ui.mode = mode;
            ((changed, ui.file.clone()), Vec::new())
        });
        if let (true, Some(path)) = (changed, file) {
            self.load_viewer(&id, &path);
        }
    }

    pub fn toggle_viewer_mode(&self) {
        let mode = self.store.state().selected_ui().map(|u| u.mode);
        self.set_viewer_mode(match mode {
            Some(ViewerMode::Diff) | None => ViewerMode::FullFile,
            Some(ViewerMode::FullFile) => ViewerMode::Diff,
        });
    }

    fn load_viewer(&self, id: &ObjectiveId, path: &RepoPath) {
        let (basis, mode, side) = {
            let state = self.store.state();
            let ui = state.ui.per_objective.get(id).cloned().unwrap_or_default();
            let deleted = state
                .changes
                .get(id)
                .and_then(Load::loaded)
                .and_then(|l| l.files.iter().find(|f| &f.path == path))
                .is_some_and(|f| f.status == ChangeStatus::Deleted);
            (
                ui.basis,
                ui.mode,
                if deleted { Side::Old } else { Side::New },
            )
        };
        self.store.update(|s| {
            s.viewer.key = Some(ViewerKey {
                objective_id: id.clone(),
                basis: basis.clone(),
                path: path.clone(),
                mode,
                side,
            });
            s.viewer.content = ViewerContent::Loading;
            s.objective_ui_mut(id).selection = None;
            ((), vec![Change::Viewer, Change::ViewerSelection])
        });
        self.emit(match mode {
            ViewerMode::Diff => Intent::LoadDiff {
                objective_id: id.clone(),
                basis,
                path: path.clone(),
            },
            ViewerMode::FullFile => Intent::OpenFile {
                objective_id: id.clone(),
                basis,
                path: path.clone(),
                side,
                offset: 0,
            },
        });
    }

    pub fn load_more(&self) {
        let request = {
            let state = self.store.state();
            match (&state.viewer.key, &state.viewer.content) {
                (Some(key), ViewerContent::File(content)) if !content.eof => {
                    Some(Intent::OpenFile {
                        objective_id: key.objective_id.clone(),
                        basis: key.basis.clone(),
                        path: key.path.clone(),
                        side: key.side,
                        offset: content.loaded_bytes,
                    })
                }
                _ => None,
            }
        };
        if let Some(intent) = request {
            self.emit(intent);
        }
    }

    pub fn set_viewer_selection(&self, mapped: Option<Result<MappedSelection, SelectionError>>) {
        let Some(id) = self.selected() else { return };
        let selection = mapped.map(|m| {
            m.map_err(|e| e.message().to_owned())
                .and_then(|m| self.reference_for(&id, m))
        });
        self.store.update(|s| {
            let ui = s.objective_ui_mut(&id);
            if ui.selection == selection {
                return ((), Vec::new());
            }
            ui.selection = selection;
            ((), vec![Change::ViewerSelection])
        });
    }

    fn reference_for(&self, id: &ObjectiveId, m: MappedSelection) -> Result<Reference, String> {
        let state = self.store.state();
        let key = state
            .viewer
            .key
            .as_ref()
            .ok_or("Nothing is open in the viewer")?;
        let digest = match &state.viewer.content {
            ViewerContent::Diff(diff) => match m.side {
                Side::New => diff.new_digest.clone(),
                Side::Old => diff.old_digest.clone(),
            },
            ViewerContent::File(content) => Some(content.artifact_digest.clone()),
            _ => None,
        }
        .ok_or("The inspected content has no retained identity yet — refresh")?;
        let version = match &key.basis {
            Basis::Working => {
                let capture = state
                    .changes
                    .get(id)
                    .and_then(Load::loaded)
                    .and_then(|l| l.capture_id.clone())
                    .ok_or("The working capture identity is unavailable — refresh")?;
                ReferenceVersion::Working {
                    capture_id: capture,
                }
            }
            Basis::Candidate { candidate_id } => ReferenceVersion::Candidate {
                candidate_id: candidate_id.clone(),
            },
            Basis::Since {
                from_candidate_id,
                to_candidate_id,
            } => ReferenceVersion::Candidate {
                candidate_id: match m.side {
                    Side::New => to_candidate_id.clone(),
                    Side::Old => from_candidate_id.clone(),
                },
            },
        };
        Ok(Reference {
            artifact_digest: digest,
            path: key.path.clone(),
            version,
            side: m.side,
            byte_start: m.start,
            byte_end: m.end,
            selected_text: m.selected_text,
            line_hint: m.line_hint,
        })
    }

    pub fn add_feedback(&self, draft_id: DraftId) -> Result<DraftId, String> {
        let id = self.selected().ok_or("Select an Objective first")?;
        let (reference, session) = {
            let state = self.store.state();
            let reference = match state.selected_ui().and_then(|u| u.selection.clone()) {
                Some(Ok(reference)) => reference,
                Some(Err(reason)) => return Err(reason),
                None => return Err(SelectionError::Empty.message().to_owned()),
            };
            let session = state
                .selected_detail()
                .and_then(|d| d.session.clone())
                .ok_or("No Agent Session to address — start the agent first")?;
            (reference, session)
        };
        self.store.update(|s| {
            s.drafts.entry(id.clone()).or_default().insert(
                0,
                DraftEditor::new_local(draft_id.clone(), id.clone(), session.session_id, reference),
            );
            s.ui.inspector_tab = InspectorTab::Feedback;
            let mut changes = vec![Change::Drafts(id.clone()), Change::InspectorTab];
            if s.ui.layout == LayoutClass::Compact && s.ui.compact_region != Region::Inspector {
                s.ui.compact_region = Region::Inspector;
                changes.push(Change::Layout);
            }
            ((), changes)
        });
        Ok(draft_id)
    }

    pub fn edit_draft(&self, draft_id: &DraftId, body: &str, now: Millis) {
        self.store.update(|s| match s.draft_mut(draft_id) {
            Some(editor) => {
                editor.edit(body, now);
                ((), vec![Change::DraftStatus(draft_id.clone())])
            }
            None => ((), Vec::new()),
        });
    }

    pub fn poll_drafts(&self, now: Millis) -> Option<Millis> {
        let (requests, next) = self.store.update(|s| {
            let connected = s.connection.is_connected();
            let mut requests = Vec::new();
            let mut changes = Vec::new();
            let mut next: Option<Millis> = None;
            for editor in s.drafts.values_mut().flatten() {
                if let Some(request) = editor.poll(now, connected) {
                    changes.push(Change::DraftStatus(editor.draft_id.clone()));
                    requests.push(request);
                }
                if let Some(deadline) = editor.next_deadline() {
                    next = Some(next.map_or(deadline, |n| n.min(deadline)));
                }
            }
            ((requests, next), changes)
        });
        for r in requests {
            self.emit(Intent::SaveDraft {
                objective_id: r.objective_id,
                draft_id: r.draft_id,
                expected_revision: r.expected_revision,
                reference: r.reference,
                body: r.body,
            });
        }
        next
    }

    pub fn unsaved_drafts(&self) -> Vec<UnsavedDraft> {
        let state = self.store.state();
        let connected = state.connection.is_connected();
        let mut unsaved: Vec<UnsavedDraft> = state
            .drafts
            .values()
            .flatten()
            .filter(|d| d.has_unsaved_text())
            .map(|d| {
                let status = d.status(connected);
                UnsavedDraft {
                    draft_id: d.draft_id.clone(),
                    objective: state
                        .objective(&d.objective_id)
                        .map_or_else(|| d.objective_id.short().to_owned(), |o| o.title.clone()),
                    reference_label: format::reference_label(
                        &d.reference,
                        state.detail(&d.objective_id),
                    ),
                    pending: matches!(status, SaveStatus::Unsaved | SaveStatus::Saving),
                    status: status.label(),
                }
            })
            .collect();
        unsaved.sort_by(|a, b| a.draft_id.as_str().cmp(b.draft_id.as_str()));
        unsaved
    }

    pub fn flush_drafts(&self, now: Millis) -> Option<Millis> {
        self.store.update(|s| {
            for editor in s.drafts.values_mut().flatten() {
                editor.flush(now);
            }
            ((), Vec::new())
        });
        self.poll_drafts(now)
    }

    pub fn retry_draft(&self, draft_id: &DraftId, now: Millis) {
        self.store.update(|s| {
            if let Some(editor) = s.draft_mut(draft_id) {
                editor.retry(now);
            }
            ((), vec![Change::DraftStatus(draft_id.clone())])
        });
    }

    pub fn resolve_draft_conflict(&self, draft_id: &DraftId, keep_local: bool, now: Millis) {
        self.store.update(|s| {
            let changes = s
                .draft_mut(draft_id)
                .map(|editor| {
                    editor.resolve_conflict(keep_local, now);
                    Change::Drafts(editor.objective_id.clone())
                })
                .into_iter()
                .collect();
            ((), changes)
        });
    }

    pub fn delete_draft(&self, draft_id: &DraftId) -> Result<(), String> {
        let durable = self.store.update(|s| {
            let revision = match s
                .draft_mut(draft_id)
                .map(|e| e.removal_revision())
                .transpose()
            {
                Ok(revision) => revision,
                Err(reason) => return (Err(reason), Vec::new()),
            };
            match revision {
                Some(0) => {
                    let mut changes = Vec::new();
                    for (objective, editors) in &mut s.drafts {
                        let before = editors.len();
                        editors.retain(|e| &e.draft_id != draft_id);
                        if before != editors.len() {
                            changes.push(Change::Drafts(objective.clone()));
                        }
                    }
                    (Ok(None), changes)
                }
                other => (Ok(other), Vec::new()),
            }
        })?;
        if let Some(expected_revision) = durable {
            self.emit(Intent::DeleteDraft {
                draft_id: draft_id.clone(),
                expected_revision,
            });
        }
        Ok(())
    }

    pub fn set_feedback_mode(&self, mode: DeliveryMode) {
        let Some(id) = self.selected() else { return };
        self.store.update(|s| {
            s.objective_ui_mut(&id).feedback_mode = mode;
            ((), vec![Change::Drafts(id.clone())])
        });
    }

    pub fn feedback_batch(&self) -> Option<FeedbackBatch> {
        let state = self.store.state();
        let id = state.ui.selected.as_ref()?;
        let detail = state.detail(id);
        let mode = state
            .ui
            .per_objective
            .get(id)
            .map_or(DeliveryMode::Steer, |u| u.feedback_mode);
        let connected = state.connection.is_connected();
        let mut items: Vec<BatchItem> = state
            .unsent_drafts(id)
            .filter(|d| d.durable_revision() > 0 || !d.body.is_empty())
            .map(|d| BatchItem {
                draft_id: d.draft_id.clone(),
                revision: d.durable_revision(),
                reference_label: format::reference_label(&d.reference, detail),
                body: d.body.clone(),
                stale: d.stale,
            })
            .collect();
        let deferred = items.len().saturating_sub(MAX_FEEDBACK_BATCH);
        items.truncate(MAX_FEEDBACK_BATCH);
        let destination = detail.and_then(|d| d.session.clone());
        let mut blockers = Vec::new();
        let explained = !connected || destination.is_none() || items.is_empty();
        if !connected {
            blockers.push("Not connected to the service".to_owned());
        }
        if destination.is_none() {
            blockers.push(
                "There is no Agent Session to send feedback to — start the agent in Activity first"
                    .to_owned(),
            );
        }
        if items.is_empty() {
            blockers.push("No drafts to submit".to_owned());
        }
        if !explained {
            match detail.map(|d| d.availability(ActionName::FeedbackSubmit)) {
                Some(ActionAvailability::Enabled) => {}
                Some(ActionAvailability::Disabled { reason }) => blockers.push(reason),
                Some(ActionAvailability::NotOffered) | None => blockers
                    .push("Submitting feedback is not offered for this Objective".to_owned()),
            }
        }
        let stale: Vec<&BatchItem> = items.iter().filter(|i| i.stale).collect();
        if !stale.is_empty() {
            let names: Vec<String> = stale
                .iter()
                .map(|i| format!("{} (draft {})", i.reference_label, i.draft_id.short()))
                .collect();
            blockers.push(format!(
                "{} stale item{} block{} the whole batch: {} — re-anchor or remove {}",
                stale.len(),
                if stale.len() == 1 { "" } else { "s" },
                if stale.len() == 1 { "s" } else { "" },
                names.join("; "),
                if stale.len() == 1 { "it" } else { "them" },
            ));
        }
        if state
            .unsent_drafts(id)
            .any(|d| !d.is_saved() && !d.body.is_empty())
        {
            blockers.push("Wait until every draft is saved".to_owned());
        }
        Some(FeedbackBatch {
            items,
            deferred,
            destination,
            mode,
            blockers,
        })
    }

    pub fn submit_feedback(&self, batch: &FeedbackBatch) -> Result<(), String> {
        if let Some(blocker) = batch.blockers.first() {
            return Err(blocker.clone());
        }
        let id = self.selected().ok_or("Select an Objective first")?;
        let destination = batch
            .destination
            .clone()
            .ok_or("No destination Agent Session")?;
        self.emit(Intent::SubmitFeedback {
            objective_id: id,
            destination_session_id: destination.session_id,
            association_revision: destination.association_revision,
            drafts: batch
                .items
                .iter()
                .map(|i| DraftRevisionRef {
                    draft_id: i.draft_id.clone(),
                    revision: i.revision,
                })
                .collect(),
            mode: batch.mode,
            note: None,
        });
        Ok(())
    }

    fn with_detail<T>(&self, f: impl FnOnce(&ObjectiveDetail) -> T) -> Option<T> {
        self.store.state().selected_detail().map(f)
    }

    fn command(&self, action: ActionName, intent: Intent) -> Result<(), String> {
        let availability = self
            .with_detail(|d| d.availability(action))
            .ok_or("The Objective is not loaded")?;
        match availability {
            ActionAvailability::Enabled => {
                self.emit(intent);
                Ok(())
            }
            ActionAvailability::Disabled { reason } => Err(reason),
            ActionAvailability::NotOffered => Err("This action is not offered".to_owned()),
        }
    }

    fn selected_or_err(&self) -> Result<ObjectiveId, String> {
        self.selected()
            .ok_or_else(|| "Select an Objective first".to_owned())
    }

    fn current_candidate(&self) -> Result<CandidateSummary, String> {
        self.with_detail(|d| d.current_candidate().cloned())
            .flatten()
            .ok_or_else(|| "There is no current candidate".to_owned())
    }

    pub fn update_intent(
        &self,
        expected_intent_revision: u64,
        intent: ObjectiveIntent,
    ) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        if intent.outcome.trim().is_empty() {
            return Err("Describe the expected outcome".to_owned());
        }
        let unchanged = self
            .with_detail(|d| d.intent == intent)
            .ok_or("The Objective is not loaded")?;
        if unchanged {
            return Err("Nothing changed".to_owned());
        }
        self.command(
            ActionName::ObjectiveUpdateIntent,
            Intent::UpdateObjective {
                objective_id,
                expected_intent_revision,
                intent,
            },
        )
    }

    pub fn start_execution(
        &self,
        model: ModelRef,
        thinking: Option<ThinkingLevel>,
    ) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        let is_default = self.store.state().settings.default_model.as_ref() == Some(&model);
        self.command(
            ActionName::ExecutionStart,
            Intent::StartExecution {
                objective_id,
                model: model.clone(),
                thinking,
            },
        )?;
        if !is_default {
            self.emit(Intent::UpdateSettings {
                default_model: model,
            });
        }
        Ok(())
    }

    pub fn cancel(&self) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        self.command(ActionName::ExecutionCancel, Intent::Cancel { objective_id })
    }

    pub fn resume(&self) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        self.command(ActionName::ExecutionResume, Intent::Resume { objective_id })
    }

    pub fn set_composer(&self, text: &str, mode: DeliveryMode) {
        let Some(id) = self.selected() else { return };
        self.store.update(|s| {
            let ui = s.objective_ui_mut(&id);
            text.clone_into(&mut ui.composer_text);
            ui.composer_mode = mode;
            ((), Vec::new())
        });
    }

    pub fn send_message(&self, text: &str, mode: DeliveryMode) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        if text.trim().is_empty() {
            return Err("Type a message first".to_owned());
        }
        self.command(
            ActionName::MessageSend,
            Intent::SendMessage {
                objective_id,
                text: text.to_owned(),
                mode,
            },
        )
    }

    pub fn freeze(&self) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        self.command(ActionName::ReviewFreeze, Intent::Freeze { objective_id })
    }

    pub fn return_for_revision(&self) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        let candidate = self.current_candidate()?;
        self.command(
            ActionName::ReviewReturn,
            Intent::ReturnForRevision {
                objective_id,
                candidate_id: candidate.candidate_id,
            },
        )
    }

    pub fn verify(&self, command: &str) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        let argv = format::split_argv(command).map_err(str::to_owned)?;
        let candidate = self.current_candidate()?;
        self.command(
            ActionName::ReviewVerify,
            Intent::Verify {
                objective_id,
                candidate_id: candidate.candidate_id,
                argv,
            },
        )
    }

    pub fn approval_needs_override(&self) -> bool {
        self.with_detail(|d| {
            let Some(candidate) = d.current_candidate() else {
                return true;
            };
            let mut latest: Vec<&VerificationSummary> = Vec::new();
            for v in d
                .verifications
                .iter()
                .filter(|v| v.candidate_id == candidate.candidate_id)
            {
                match latest.iter_mut().find(|l| l.command == v.command) {
                    Some(l) if l.started_at < v.started_at => *l = v,
                    Some(_) => {}
                    None => latest.push(v),
                }
            }
            latest.is_empty() || latest.iter().any(|v| v.state != VerificationState::Passed)
        })
        .unwrap_or(true)
    }

    pub fn approve(&self, override_reason: Option<String>) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        let candidate = self.current_candidate()?;
        let target_commit = self
            .with_detail(|d| d.integration_target.commit.clone())
            .ok_or("The Objective is not loaded")?;
        let override_reason = override_reason.filter(|r| !r.trim().is_empty());
        if self.approval_needs_override() && override_reason.is_none() {
            return Err(
                "Checks are missing or not all passed — give an override reason".to_owned(),
            );
        }
        self.command(
            ActionName::ReviewApprove,
            Intent::Approve {
                objective_id,
                candidate_id: candidate.candidate_id,
                target_commit,
                override_reason,
            },
        )
    }

    pub fn refresh_candidate(&self) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        let candidate = self.current_candidate()?;
        self.command(
            ActionName::ReviewRefresh,
            Intent::Refresh {
                objective_id,
                candidate_id: candidate.candidate_id,
            },
        )
    }

    pub fn integrate(&self) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        let approval_id = self
            .with_detail(|d| {
                d.approval
                    .as_ref()
                    .filter(|a| a.state == ApprovalState::Valid)
                    .map(|a| a.approval_id.clone())
            })
            .flatten()
            .ok_or("There is no valid approval")?;
        self.command(
            ActionName::ReviewIntegrate,
            Intent::Integrate {
                objective_id,
                approval_id,
            },
        )
    }

    pub fn complete(&self) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        self.command(
            ActionName::ObjectiveComplete,
            Intent::Complete { objective_id },
        )
    }

    pub fn discard(&self) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        self.command(
            ActionName::ObjectiveDiscard,
            Intent::Discard { objective_id },
        )
    }

    pub fn cleanup(&self, confirm_data_loss: bool) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        self.command(
            ActionName::ObjectiveCleanup,
            Intent::Cleanup {
                objective_id,
                confirm_data_loss,
            },
        )
    }

    pub fn delete_history(&self) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        self.command(
            ActionName::ObjectiveDeleteHistory,
            Intent::DeleteHistory { objective_id },
        )
    }

    pub fn create_objective(
        &self,
        title: &str,
        intent: ObjectiveIntent,
        integration_target: &str,
        start: StartPoint,
    ) -> Result<(), String> {
        let title = title.trim();
        if title.is_empty() || title.chars().count() > 200 {
            return Err("Give the Objective a title of 1–200 characters".to_owned());
        }
        if intent.outcome.trim().is_empty() {
            return Err("Describe the expected outcome".to_owned());
        }
        let start = match start {
            StartPoint::Target => StartPoint::Target,
            StartPoint::Commit(hex) => {
                let hex = hex.trim().to_ascii_lowercase();
                if !matches!(hex.len(), 40 | 64) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(
                        "Enter the full 40- or 64-character commit id, or leave the start point empty"
                            .to_owned(),
                    );
                }
                StartPoint::Commit(hex)
            }
        };
        let workspace_id = self
            .store
            .state()
            .active_workspace
            .clone()
            .ok_or("Open a Git repository first")?;
        self.emit(Intent::CreateObjective {
            workspace_id,
            title: title.to_owned(),
            intent,
            integration_target: integration_target.to_owned(),
            start,
        });
        Ok(())
    }

    pub fn search(&self, scope: SearchScope, query: &str) {
        let query = query.trim();
        let request = {
            let state = self.store.state();
            let Some(workspace_id) = state.active_workspace.clone() else {
                return;
            };
            let (objective_id, basis) = match scope {
                SearchScope::Objectives => (None, None),
                SearchScope::Files | SearchScope::Content => (
                    state.ui.selected.clone(),
                    state.selected_ui().map(|u| u.basis.clone()),
                ),
            };
            (!query.is_empty()).then(|| Intent::Search {
                workspace_id,
                scope,
                objective_id,
                basis,
                query: query.to_owned(),
            })
        };
        if let Some(intent) = request {
            self.store.update(|s| {
                s.search = Load::Loading;
                ((), vec![Change::Search])
            });
            self.emit(intent);
        }
    }

    pub fn open_search_result(&self, result: &SearchResult) {
        let searched_basis = self
            .store
            .state()
            .search
            .loaded()
            .and_then(|r| r.basis.clone());
        if let Some(id) = &result.objective_id {
            self.select_objective(id);
        }
        self.show_region(Region::Main);
        if result.kind == SearchResultKind::Objective {
            return;
        }
        if let Some(basis) = searched_basis {
            self.select_basis(basis);
        }
        if result.kind == SearchResultKind::Content {
            self.set_viewer_mode(ViewerMode::FullFile);
        }
        if let Some(path) = &result.path {
            self.select_file(path);
        }
        if let (SearchResultKind::Content, Some(start), Some(end)) =
            (result.kind, result.byte_start, result.byte_end)
        {
            self.store.update(|s| match s.viewer.key.clone() {
                Some(key) if Some(&key.path) == result.path.as_ref() => {
                    s.viewer.reveal = Some(Reveal { key, start, end });
                    ((), vec![Change::Viewer])
                }
                _ => ((), Vec::new()),
            });
        }
        self.set_view(MainView::Workbench);
    }

    pub fn take_reveal(&self, key: &ViewerKey, loaded_bytes: u64) -> Option<(u64, u64)> {
        self.store.update(|s| {
            let taken = match &s.viewer.reveal {
                Some(r) if &r.key != key => {
                    s.viewer.reveal = None;
                    None
                }
                Some(r) if r.end <= loaded_bytes => {
                    let range = (r.start, r.end);
                    s.viewer.reveal = None;
                    Some(range)
                }
                _ => None,
            };
            (taken, Vec::new())
        })
    }

    pub fn load_older_activity(&self) {
        let Some(id) = self.selected() else { return };
        let before = self
            .store
            .state()
            .activity
            .get(&id)
            .and_then(|l| l.items.first().map(|i| i.seq));
        self.emit(Intent::LoadActivity {
            objective_id: id,
            before,
        });
    }

    pub fn open_terminal(&self, cols: u16, rows: u16) -> Result<(), String> {
        let id = self.selected_or_err()?;
        let opened = self.store.update(|s| {
            if s.terminal_for(&id)
                .is_some_and(|t| !matches!(t.status, TerminalStatus::Ended(_)))
            {
                return (false, Vec::new());
            }
            s.push_terminal(TerminalSession::opening(
                id.clone(),
                usize::from(cols),
                usize::from(rows),
            ));
            (true, vec![Change::Terminal(id.clone())])
        });
        if opened {
            self.emit(Intent::OpenTerminal {
                objective_id: id,
                cols,
                rows,
            });
        }
        Ok(())
    }

    fn open_terminal_id(&self) -> Option<(ObjectiveId, String)> {
        let state = self.store.state();
        let id = state.ui.selected.clone()?;
        let session = state.terminal_for(&id).filter(|t| t.is_open())?;
        Some((id, session.terminal_id.clone()?))
    }

    pub fn terminal_input(&self, text: &str) -> bool {
        let Some((objective_id, terminal_id)) = self.open_terminal_id() else {
            return false;
        };
        for data in input_chunks(text) {
            self.emit(Intent::TerminalInput {
                objective_id: objective_id.clone(),
                terminal_id: terminal_id.clone(),
                data,
            });
        }
        true
    }

    pub fn resize_terminal(&self, cols: u16, rows: u16) {
        let send = self.store.update(|s| {
            let Some(id) = s.ui.selected.clone() else {
                return (None, Vec::new());
            };
            let Some(session) = s.terminal_for_mut(&id) else {
                return (None, Vec::new());
            };
            let before = session.screen.size();
            session.screen.resize(usize::from(cols), usize::from(rows));
            if session.screen.size() == before {
                return (None, Vec::new());
            }
            let (c, r) = session.screen.size();
            let send = session
                .is_open()
                .then(|| session.terminal_id.clone())
                .flatten()
                .map(|t| {
                    (
                        id.clone(),
                        t,
                        u16::try_from(c).unwrap_or(u16::MAX),
                        u16::try_from(r).unwrap_or(u16::MAX),
                    )
                });
            (send, vec![Change::Terminal(id)])
        });
        if let Some((objective_id, terminal_id, cols, rows)) = send {
            self.emit(Intent::ResizeTerminal {
                objective_id,
                terminal_id,
                cols,
                rows,
            });
        }
    }

    pub fn close_terminal(&self) {
        if let Some((objective_id, terminal_id)) = self.open_terminal_id() {
            self.emit(Intent::CloseTerminal {
                objective_id,
                terminal_id,
            });
        }
    }

    pub fn navigate_to(&self, id: &ObjectiveId, review: bool) {
        self.select_objective(id);
        self.set_view(MainView::Workbench);
        self.set_inspector_tab(if review {
            InspectorTab::Evidence
        } else {
            InspectorTab::Overview
        });
    }

    pub fn request_service_stop(&self, reason: StopReason) -> bool {
        let connected = self.store.state().connection.is_connected();
        if connected {
            self.emit(Intent::StopService { reason });
        }
        connected
    }

    pub fn agent_line(&self) -> Option<AgentLine> {
        let readiness = self.readiness();
        self.with_detail(|d| agent_line(d, &readiness))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentLine {
    pub sentence: String,
    pub details: Option<String>,
}

fn agent_line(d: &ObjectiveDetail, readiness: &Readiness) -> AgentLine {
    let resumable = d.availability(ActionName::ExecutionResume) == ActionAvailability::Enabled;
    let resume = |what: &str| {
        if resumable {
            format!("{what} Resume it to continue the same Agent Session.")
        } else {
            what.to_owned()
        }
    };
    let sentence = match (&d.session, d.summary.execution.state) {
        (_, ExecutionState::Starting) => "The agent is starting.".to_owned(),
        (_, ExecutionState::Running) => "The agent is working on a turn.".to_owned(),
        (_, ExecutionState::Idle) => "The agent is idle and waiting for your message.".to_owned(),
        (_, ExecutionState::Stopping) => "The agent is stopping.".to_owned(),
        (_, ExecutionState::Cancelled) => resume("The agent was cancelled."),
        (_, ExecutionState::Failed) => resume("The agent stopped after an error."),
        (Some(_), ExecutionState::None) => resume("The agent isn't running."),
        (None, ExecutionState::None) => match d.availability(ActionName::ExecutionStart) {
            ActionAvailability::NotOffered => "The agent hasn't started.".to_owned(),
            ActionAvailability::Disabled { .. } => {
                "The agent hasn't started and can't be started right now.".to_owned()
            }
            ActionAvailability::Enabled => format!(
                "The agent hasn't started. {}",
                match readiness {
                    Readiness::Ready { model, .. } =>
                        format!("Start it below with {}.", model.name),
                    Readiness::CatalogLoading => "Loading the models it can use…".to_owned(),
                    Readiness::NoProviders { .. } => {
                        "No model provider is available to start it.".to_owned()
                    }
                    Readiness::NoProviderChosen => {
                        "Choose a model provider below to start it.".to_owned()
                    }
                    Readiness::ModelsLoading { provider } => {
                        format!("Loading {provider}'s models…")
                    }
                    Readiness::NoModels { provider } => {
                        format!("{provider} has no models; choose another provider below.")
                    }
                    Readiness::NeedsCredentials { provider } => format!(
                        "Add credentials for {provider} to start it, or choose another provider \
                         below."
                    ),
                    Readiness::Restricted { provider } =>
                        format!("{provider} can't be used in LADE; choose another provider below."),
                }
            ),
        },
    };
    let details = d.session.as_ref().map(|s| {
        let mut details = format!(
            "Agent Session {} · association r{} · execution generation {} · model {}",
            s.session_id.as_str(),
            s.association_revision,
            s.execution_generation,
            s.model.label(),
        );
        if let Some(thinking) = s.thinking {
            details.push_str(&format!(" · thinking {}", thinking.label()));
        }
        details
    });
    AgentLine { sentence, details }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation::controller::intents::RecordingSink;
    use crate::presentation::state::model::{AvailableAction, ConnectionState};
    use crate::presentation::state::store::{PresentationState, StoreUpdate};
    use crate::presentation::test_fixture;

    fn setup() -> (Rc<Controller>, Rc<RecordingSink>) {
        let mut state = PresentationState::new(ConnectionState::Connected);
        for update in test_fixture::fixture_updates() {
            state.apply(update);
        }
        state.connection = ConnectionState::Connected;
        let store = Store::new(state);
        let sink = Rc::new(RecordingSink::default());
        (Controller::new(store, sink.clone()), sink)
    }

    #[test]
    fn compact_layout_follows_width_and_explicit_tasks() {
        let (controller, _) = setup();
        let changes = Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = changes.clone();
        controller
            .store
            .subscribe(move |c| seen.borrow_mut().extend_from_slice(c));
        controller.set_layout_width(1440);
        assert!(changes.borrow().is_empty(), "already wide");
        controller.set_layout_width(800);
        assert_eq!(controller.store.state().ui.layout, LayoutClass::Compact);
        assert_eq!(changes.borrow().as_slice(), [Change::Layout]);
        controller.show_region(Region::Objectives);
        controller.set_view(MainView::Activity);
        assert_eq!(controller.store.state().ui.compact_region, Region::Main);
        controller.select_objective(&ObjectiveId::new(test_fixture::MAIN_OBJECTIVE));
        controller.set_view(MainView::Workbench);
        let reference = controller
            .store
            .state()
            .drafts
            .values()
            .flatten()
            .next()
            .map(|d| d.reference.clone())
            .expect("fixture draft");
        controller.store.update(|s| {
            s.objective_ui_mut(&ObjectiveId::new(test_fixture::MAIN_OBJECTIVE))
                .selection = Some(Ok(reference));
            ((), Vec::new())
        });
        controller
            .add_feedback(DraftId::new("compact-draft"))
            .expect("draft created");
        let state = controller.store.state();
        assert_eq!(state.ui.compact_region, Region::Inspector);
        assert_eq!(state.ui.inspector_tab, InspectorTab::Feedback);
        drop(state);
        controller.set_compact_details(true);
        assert!(controller.store.state().ui.compact_details);
        controller.set_layout_width(1280);
        assert_eq!(controller.store.state().ui.layout, LayoutClass::Wide);
    }

    #[test]
    fn content_search_results_become_the_viewer_selection_range() {
        let (controller, _) = setup();
        let id = ObjectiveId::new(test_fixture::MAIN_OBJECTIVE);
        controller.select_objective(&id);
        let path = RepoPath::utf8("src/network/retry.ts");
        controller.open_search_result(&SearchResult {
            kind: SearchResultKind::Content,
            objective_id: Some(id.clone()),
            path: Some(path.clone()),
            line: Some(2),
            preview: "retry".into(),
            byte_start: Some(10),
            byte_end: Some(15),
        });
        let key = controller
            .store
            .state()
            .viewer
            .key
            .clone()
            .expect("file opened");
        assert_eq!((key.path.clone(), key.mode), (path, ViewerMode::FullFile));
        assert_eq!(
            controller.take_reveal(&key, 12),
            None,
            "range not loaded yet"
        );
        assert_eq!(controller.take_reveal(&key, 15), Some((10, 15)));
        assert_eq!(controller.take_reveal(&key, 15), None, "taken once");
        controller.open_search_result(&SearchResult {
            kind: SearchResultKind::Content,
            objective_id: Some(id),
            path: Some(RepoPath::utf8("src/network/retry.ts")),
            line: Some(2),
            preview: "retry".into(),
            byte_start: None,
            byte_end: None,
        });
        assert_eq!(controller.store.state().viewer.reveal, None);
    }

    #[test]
    fn notification_navigation_only_requests_projections() {
        let (controller, sink) = setup();
        controller.navigate_to(&ObjectiveId::new(test_fixture::SECOND_OBJECTIVE), true);
        let state = controller.store.state();
        assert_eq!(
            state.ui.selected.as_ref().map(ObjectiveId::as_str),
            Some(test_fixture::SECOND_OBJECTIVE)
        );
        assert_eq!(state.ui.view, MainView::Workbench);
        assert_eq!(state.ui.inspector_tab, InspectorTab::Evidence);
        let intents = sink.intents.borrow();
        assert!(!intents.is_empty());
        assert!(
            intents.iter().all(Intent::is_query),
            "navigation never sends a command: {intents:?}"
        );
    }

    #[test]
    fn terminal_lifecycle_in_the_controller() {
        use crate::presentation::terminal::{TerminalEnd, TerminalMode};
        let (controller, sink) = setup();
        let id = ObjectiveId::new(test_fixture::MAIN_OBJECTIVE);
        controller.select_objective(&id);
        sink.intents.borrow_mut().clear();
        assert!(!controller.terminal_input("ls\r"), "no terminal yet");
        controller.open_terminal(80, 24).unwrap();
        controller.open_terminal(80, 24).unwrap();
        assert_eq!(
            sink.intents
                .borrow()
                .iter()
                .filter(|i| matches!(i, Intent::OpenTerminal { .. }))
                .count(),
            1,
            "one open in flight"
        );
        controller.store.apply(StoreUpdate::TerminalOpened {
            objective_id: id.clone(),
            terminal_id: "00000000-0000-4000-8000-000000000042".into(),
            mode: TerminalMode::ReadOnly,
            cols: 80,
            rows: 24,
        });
        assert!(controller.terminal_input(&"x".repeat(5000)));
        let inputs = sink
            .intents
            .borrow()
            .iter()
            .filter(|i| matches!(i, Intent::TerminalInput { .. }))
            .count();
        assert_eq!(inputs, 2, "long input is chunked");
        controller.resize_terminal(100, 30);
        controller.resize_terminal(100, 30);
        assert_eq!(
            sink.intents
                .borrow()
                .iter()
                .filter(|i| matches!(
                    i,
                    Intent::ResizeTerminal {
                        cols: 100,
                        rows: 30,
                        ..
                    }
                ))
                .count(),
            1
        );
        controller.store.apply(StoreUpdate::TerminalOutput {
            terminal_id: "00000000-0000-4000-8000-000000000042".into(),
            seq: 1,
            data: "hello".into(),
            dropped: 0,
        });
        controller.store.apply(StoreUpdate::TerminalClosed {
            terminal_id: "00000000-0000-4000-8000-000000000042".into(),
            end: TerminalEnd::ReviewFreeze,
        });
        assert!(
            !controller.terminal_input("x"),
            "an ended terminal takes no input"
        );
        controller.open_terminal(80, 24).unwrap();
        controller.store.apply(StoreUpdate::TerminalOpened {
            objective_id: id.clone(),
            terminal_id: "00000000-0000-4000-8000-000000000043".into(),
            mode: TerminalMode::ReadWrite,
            cols: 80,
            rows: 24,
        });
        controller
            .store
            .apply(StoreUpdate::Connection(ConnectionState::Reconnecting {
                attempt: 1,
            }));
        assert_eq!(
            controller
                .store
                .state()
                .terminal_for(&id)
                .map(|t| t.status.clone()),
            Some(TerminalStatus::Ended(TerminalEnd::ConnectionLost))
        );
    }

    #[test]
    fn service_stop_is_requested_only_with_control() {
        let (controller, sink) = setup();
        assert!(controller.request_service_stop(StopReason::Exit));
        assert!(sink.intents.borrow().contains(&Intent::StopService {
            reason: StopReason::Exit
        }));
        sink.intents.borrow_mut().clear();
        controller
            .store
            .apply(StoreUpdate::Connection(ConnectionState::LeaseHeld));
        assert!(!controller.request_service_stop(StopReason::Logout));
        assert!(
            sink.intents.borrow().is_empty(),
            "no authority, nothing sent"
        );
    }

    #[test]
    fn selecting_an_objective_requests_its_projections() {
        let (controller, sink) = setup();
        controller.select_objective(&ObjectiveId::new(test_fixture::SECOND_OBJECTIVE));
        let intents = sink.intents.borrow();
        assert!(
            intents
                .iter()
                .any(|i| matches!(i, Intent::LoadObjective { .. }))
        );
        assert!(
            intents
                .iter()
                .any(|i| matches!(i, Intent::LoadChanges { .. }))
        );
    }

    #[test]
    fn edit_intent_sends_the_edited_revision() {
        let (controller, sink) = setup();
        controller.select_objective(&ObjectiveId::new(test_fixture::MAIN_OBJECTIVE));
        sink.intents.borrow_mut().clear();
        let (revision, mut intent, enabled) = {
            let state = controller.store.state();
            let d = state.selected_detail().unwrap();
            (
                d.intent_revision,
                d.intent.clone(),
                d.availability(ActionName::ObjectiveUpdateIntent) == ActionAvailability::Enabled,
            )
        };
        assert_eq!(
            controller.update_intent(revision, intent.clone()),
            Err("Nothing changed".into())
        );
        intent.constraints = "Keep the public API".into();
        let result = controller.update_intent(revision, intent.clone());
        if enabled {
            result.unwrap();
            assert_eq!(
                sink.intents.borrow().as_slice(),
                [Intent::UpdateObjective {
                    objective_id: ObjectiveId::new(test_fixture::MAIN_OBJECTIVE),
                    expected_intent_revision: revision,
                    intent,
                }]
            );
        } else {
            assert!(result.is_err());
            assert!(sink.intents.borrow().is_empty());
        }
    }

    #[test]
    fn disabled_actions_are_not_sent() {
        let (controller, sink) = setup();
        controller.select_objective(&ObjectiveId::new(test_fixture::MAIN_OBJECTIVE));
        sink.intents.borrow_mut().clear();
        let err = controller.integrate().unwrap_err();
        assert!(!err.is_empty());
        assert!(sink.intents.borrow().is_empty());
    }

    fn without_session(
        detail: &ObjectiveDetail,
        start: Option<(bool, Option<&str>)>,
    ) -> ObjectiveDetail {
        let mut d = detail.clone();
        d.session = None;
        d.summary.execution.state = ExecutionState::None;
        d.available_actions.retain(|a| {
            !matches!(
                a.action,
                ActionName::ExecutionStart | ActionName::ExecutionResume
            )
        });
        if let Some((enabled, reason)) = start {
            d.available_actions.push(AvailableAction {
                action: ActionName::ExecutionStart,
                enabled,
                reason: reason.map(str::to_owned),
            });
        }
        d
    }

    #[test]
    fn the_agent_line_is_one_sentence_per_state_with_ids_only_in_the_details() {
        let (controller, _) = setup();
        controller.select_objective(&ObjectiveId::new(test_fixture::MAIN_OBJECTIVE));
        let detail = controller
            .store
            .state()
            .selected_detail()
            .cloned()
            .expect("fixture detail");
        let line = agent_line(&detail, &Readiness::CatalogLoading);
        let session = detail.session.clone().expect("fixture session");
        assert!(!line.sentence.contains(session.session_id.short()));
        assert!(
            line.details
                .as_deref()
                .is_some_and(|d| d.contains(session.session_id.as_str()))
        );
        let fresh = without_session(&detail, Some((true, None)));
        for (readiness, sentence) in [
            (
                Readiness::NoProviderChosen,
                "The agent hasn't started. Choose a model provider below to start it.",
            ),
            (
                Readiness::NoProviders {
                    error: Some("sandbox_unavailable".into()),
                },
                "The agent hasn't started. No model provider is available to start it.",
            ),
            (
                Readiness::ModelsLoading {
                    provider: "Fixture".into(),
                },
                "The agent hasn't started. Loading Fixture's models…",
            ),
            (
                Readiness::NeedsCredentials {
                    provider: "Anthropic".into(),
                },
                "The agent hasn't started. Add credentials for Anthropic to start it, or choose \
                 another provider below.",
            ),
        ] {
            let line = agent_line(&fresh, &readiness);
            assert_eq!(line.sentence, sentence);
            assert_eq!(line.details, None, "no session, nothing technical");
        }
        let refused = without_session(&detail, Some((false, Some("why"))));
        assert_eq!(
            agent_line(&refused, &Readiness::NoProviderChosen).sentence,
            "The agent hasn't started and can't be started right now."
        );
    }

    #[test]
    fn feedback_without_a_session_states_that_once() {
        let (controller, _) = setup();
        let id = ObjectiveId::new(test_fixture::MAIN_OBJECTIVE);
        controller.select_objective(&id);
        controller.store.update(|s| {
            if let Some(Load::Loaded(detail)) = s.details.get_mut(&id) {
                *detail = without_session(detail, None);
                for available in &mut detail.available_actions {
                    if available.action == ActionName::FeedbackSubmit {
                        available.enabled = false;
                        available.reason = Some("input requires an active objective".into());
                    }
                }
            }
            ((), Vec::new())
        });
        let batch = controller.feedback_batch().unwrap();
        assert!(
            batch
                .blockers
                .iter()
                .any(|b| b.contains("no Agent Session")),
            "{:?}",
            batch.blockers
        );
        assert!(
            !batch
                .blockers
                .iter()
                .any(|b| b == "input requires an active objective"),
            "the service reason would only restate it: {:?}",
            batch.blockers
        );
    }

    #[test]
    fn stale_item_blocks_the_whole_batch() {
        let (controller, sink) = setup();
        controller.select_objective(&ObjectiveId::new(test_fixture::MAIN_OBJECTIVE));
        sink.intents.borrow_mut().clear();
        let batch = controller.feedback_batch().unwrap();
        assert!(batch.items.iter().any(|i| i.stale));
        assert!(batch.blockers.iter().any(|b| b.contains("stale")));
        assert!(controller.submit_feedback(&batch).is_err());
        assert!(sink.intents.borrow().is_empty());
    }

    #[test]
    fn a_feedback_batch_takes_at_most_the_wire_bound_and_defers_the_rest() {
        use crate::presentation::state::model::{Draft, DraftState, SessionId};
        let (controller, sink) = setup();
        let id = ObjectiveId::new(test_fixture::MAIN_OBJECTIVE);
        controller.select_objective(&id);
        sink.intents.borrow_mut().clear();
        controller.store.update(|s| {
            let template = s.drafts[&id][0].reference.clone();
            let session = s
                .selected_detail()
                .and_then(|d| d.session.clone())
                .map_or_else(|| SessionId::new("s"), |d| d.session_id);
            let editors = (0..MAX_FEEDBACK_BATCH + 44)
                .map(|n| {
                    DraftEditor::from_durable(&Draft {
                        draft_id: DraftId::new(format!("draft-{n}")),
                        objective_id: id.clone(),
                        destination_session_id: session.clone(),
                        reference: template.clone(),
                        body: format!("note {n}"),
                        revision: 1,
                        state: DraftState::Unsent,
                        stale: false,
                        input_id: None,
                    })
                })
                .collect();
            s.drafts.insert(id.clone(), editors);
            if let Some(Load::Loaded(detail)) = s.details.get_mut(&id) {
                for available in &mut detail.available_actions {
                    if available.action == ActionName::FeedbackSubmit {
                        available.enabled = true;
                        available.reason = None;
                    }
                }
            }
            ((), Vec::new())
        });
        let batch = controller.feedback_batch().unwrap();
        assert_eq!(
            (batch.items.len(), batch.deferred),
            (MAX_FEEDBACK_BATCH, 44)
        );
        assert_eq!(batch.blockers, Vec::<String>::new());
        controller.submit_feedback(&batch).unwrap();
        let intents = sink.intents.borrow();
        let Some(Intent::SubmitFeedback { drafts, .. }) = intents.last() else {
            panic!("feedback submitted");
        };
        assert_eq!(drafts.len(), MAX_FEEDBACK_BATCH);
    }

    #[test]
    fn closing_flushes_drafts_and_lists_what_is_not_yet_saved() {
        let (controller, sink) = setup();
        controller.select_objective(&ObjectiveId::new(test_fixture::MAIN_OBJECTIVE));
        sink.intents.borrow_mut().clear();
        let draft = DraftId::new(test_fixture::SAVED_DRAFT);
        let before = controller.unsaved_drafts().len();
        controller.edit_draft(&draft, "Closing right after typing.", 1_000);
        let unsaved = controller.unsaved_drafts();
        assert_eq!(unsaved.len(), before + 1);
        assert!(
            unsaved
                .iter()
                .any(|d| d.draft_id == draft && d.status == "Unsaved")
        );
        controller.flush_drafts(1_010);
        assert!(
            sink.intents
                .borrow()
                .iter()
                .any(|i| matches!(i, Intent::SaveDraft { draft_id, .. } if draft_id == &draft)),
            "the flush does not wait for the quiet period"
        );
        assert!(
            controller
                .unsaved_drafts()
                .iter()
                .any(|d| d.draft_id == draft && d.status == "Saving…"),
            "in flight is still unsaved until the service acknowledges it"
        );
    }

    #[test]
    fn typing_in_a_draft_schedules_a_save_intent() {
        let (controller, sink) = setup();
        controller.select_objective(&ObjectiveId::new(test_fixture::MAIN_OBJECTIVE));
        sink.intents.borrow_mut().clear();
        let draft = DraftId::new(test_fixture::SAVED_DRAFT);
        controller.edit_draft(&draft, "Use a typed TimeoutError here.", 1_000);
        assert_eq!(controller.poll_drafts(1_100), Some(1_300));
        assert!(sink.intents.borrow().is_empty());
        controller.poll_drafts(1_300);
        let intents = sink.intents.borrow();
        assert!(matches!(
            intents.as_slice(),
            [Intent::SaveDraft {
                expected_revision: 3,
                ..
            }]
        ));
    }

    #[test]
    fn draft_removal_waits_for_its_first_save_to_prevent_resurrection() {
        use crate::presentation::state::model::{Draft, DraftState};
        let (controller, sink) = setup();
        let objective_id = ObjectiveId::new(test_fixture::MAIN_OBJECTIVE);
        let draft_id = DraftId::new("new-draft");
        controller.select_objective(&objective_id);
        controller.store.update(|state| {
            let template = state.drafts[&objective_id][0].clone();
            let editor = DraftEditor::new_local(
                draft_id.clone(),
                objective_id.clone(),
                template.destination_session_id,
                template.reference,
            );
            state.drafts.get_mut(&objective_id).unwrap().push(editor);
            ((), Vec::new())
        });
        controller.edit_draft(&draft_id, "Do not resurrect this draft", 1_000);
        controller.poll_drafts(1_300);
        let saved = {
            let state = controller.store.state();
            let editor = state.drafts[&objective_id]
                .iter()
                .find(|draft| draft.draft_id == draft_id)
                .unwrap();
            Draft {
                draft_id: draft_id.clone(),
                objective_id: objective_id.clone(),
                destination_session_id: editor.destination_session_id.clone(),
                reference: editor.reference.clone(),
                body: editor.body.clone(),
                revision: 1,
                state: DraftState::Unsent,
                stale: false,
                input_id: None,
            }
        };
        sink.intents.borrow_mut().clear();
        assert!(controller.delete_draft(&draft_id).is_err());
        assert_eq!(
            controller.store.state().drafts[&objective_id]
                .iter()
                .filter(|draft| draft.draft_id == draft_id)
                .map(|draft| draft.body.clone())
                .collect::<Vec<_>>(),
            ["Do not resurrect this draft"],
            "a refused removal preserves the editor and its text"
        );
        controller.store.apply(StoreUpdate::DraftChanged(saved));
        assert_eq!(
            controller.store.state().drafts[&objective_id]
                .iter()
                .filter(|draft| draft.draft_id == draft_id)
                .map(|draft| draft.body.clone())
                .collect::<Vec<_>>(),
            ["Do not resurrect this draft"],
            "the acknowledgement keeps exactly one editor"
        );
        assert!(sink.intents.borrow().is_empty());
        controller
            .delete_draft(&draft_id)
            .expect("save acknowledged");
        assert_eq!(
            sink.intents.borrow().as_slice(),
            [Intent::DeleteDraft {
                draft_id,
                expected_revision: 1,
            }]
        );
    }
}
