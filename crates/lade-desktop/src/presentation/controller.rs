//! Turns user gestures into transient-state changes and [`Intent`]s. GTK-free and testable.
//!
//! Commands are emitted only when the service's `available_actions` projection enables them; the
//! controller never derives authority, it only refuses to send what the service already refused.

use std::path::PathBuf;
use std::rc::Rc;

use super::drafts::{DraftEditor, Millis};
use super::format;
use super::intents::{DraftRevisionRef, Intent, IntentSink, StartPoint, StopReason};
use super::keys::Keymap;
use super::layout::{LayoutClass, Region};
use super::model::{
    ActionAvailability, ActionName, ApprovalState, Basis, DeliveryMode, DraftId,
    Intent as ObjectiveIntent, Load, ModelRef, ObjectiveDetail, ObjectiveId, Reference,
    ReferenceVersion, RepoPath, SearchResult, SearchResultKind, SearchScope, SessionInfo, Side,
    VerificationState,
};
use super::store::{Change, InspectorTab, MainView, Store, ViewerContent, ViewerKey, ViewerMode};
use super::terminal::{TerminalSession, TerminalStatus, input_chunks};
use super::text_map::{MappedSelection, SelectionError};

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
    pub destination: Option<SessionInfo>,
    pub mode: DeliveryMode,
    /// Reasons the batch cannot be submitted now; empty means the request may be sent.
    pub blockers: Vec<String>,
}

pub struct Controller {
    pub store: Rc<Store>,
    sink: Rc<dyn IntentSink>,
}

impl Controller {
    pub fn new(store: Rc<Store>, sink: Rc<dyn IntentSink>) -> Rc<Self> {
        Rc::new(Self { store, sink })
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
            // Choosing Workbench or Activity shows the work area in the compact composition.
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

    /// Installs a validated keymap (see `keys::Keymap::propose`).
    pub fn set_keymap(&self, keymap: Keymap) {
        self.store.update(|s| {
            if s.ui.keymap == keymap {
                return ((), Vec::new());
            }
            s.ui.keymap = keymap;
            ((), vec![Change::Keymap])
        });
    }

    /// Records the allocated window width; switches between the wide and compact compositions.
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

    /// Shows `region` in the compact composition (explicit region switch or F6). The wide
    /// composition shows every region, so this only remembers the choice there.
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
        // Restore the previously inspected file for this Objective.
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

    /// Re-requests the file shown for the selected Objective.
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
                .is_some_and(|f| f.status == super::model::ChangeStatus::Deleted);
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

    /// Requests the next page of the file shown in full-file mode.
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

    /// Records the viewer selection as a versioned reference (or why it cannot be one).
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

    /// Creates a local draft for the current selection; returns its id. Nothing is sent.
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
            // Explicit opening moves to the requested task: the compact composition shows the
            // feedback panel holding the new draft.
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

    /// Emits due draft saves; returns the next time `poll_drafts` should run.
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

    pub fn retry_draft(&self, draft_id: &DraftId, now: Millis) {
        self.store.update(|s| {
            if let Some(editor) = s.draft_mut(draft_id) {
                editor.retry(now);
            }
            ((), vec![Change::DraftStatus(draft_id.clone())])
        });
    }

    pub fn resolve_draft_conflict(&self, draft_id: &DraftId, keep_local: bool, now: Millis) {
        let objective = self.store.update(|s| {
            let objective = s.draft_mut(draft_id).map(|editor| {
                editor.resolve_conflict(keep_local, now);
                editor.objective_id.clone()
            });
            let changes = objective
                .iter()
                .map(|o| Change::Drafts(o.clone()))
                .collect();
            (objective, changes)
        });
        drop(objective);
    }

    pub fn delete_draft(&self, draft_id: &DraftId) {
        let durable = self.store.update(|s| {
            let revision = s.draft_mut(draft_id).map(|e| e.durable_revision());
            match revision {
                Some(0) => {
                    // Never saved: discard the local text only.
                    let mut changes = Vec::new();
                    for (objective, editors) in &mut s.drafts {
                        let before = editors.len();
                        editors.retain(|e| &e.draft_id != draft_id);
                        if before != editors.len() {
                            changes.push(Change::Drafts(objective.clone()));
                        }
                    }
                    (None, changes)
                }
                other => (other, Vec::new()),
            }
        });
        if let Some(expected_revision) = durable {
            self.emit(Intent::DeleteDraft {
                draft_id: draft_id.clone(),
                expected_revision,
            });
        }
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
        let items: Vec<BatchItem> = state
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
        let mut blockers = Vec::new();
        match detail.map(|d| d.availability(ActionName::FeedbackSubmit)) {
            Some(ActionAvailability::Enabled) => {}
            Some(ActionAvailability::Disabled { reason }) => blockers.push(reason),
            Some(ActionAvailability::NotOffered) | None => {
                blockers.push("Submitting feedback is not offered for this Objective".to_owned());
            }
        }
        if !connected {
            blockers.push("Not connected to the service".to_owned());
        }
        if items.is_empty() {
            blockers.push("No drafts to submit".to_owned());
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
        let destination = detail.and_then(|d| d.session.clone());
        if destination.is_none() {
            blockers.push("No destination Agent Session".to_owned());
        }
        Some(FeedbackBatch {
            items,
            destination,
            mode,
            blockers,
        })
    }

    /// Sends the confirmed batch. The service still validates staleness and destination.
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

    /// Emits `intent` only if the service projection enables `action`.
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

    fn current_candidate(&self) -> Result<super::model::CandidateSummary, String> {
        self.with_detail(|d| d.current_candidate().cloned())
            .flatten()
            .ok_or_else(|| "There is no current candidate".to_owned())
    }

    /// Requests an intent change bound to the intent revision the developer edited; the service
    /// refuses it when the intent changed meanwhile.
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

    pub fn start_execution(&self, model: ModelRef) -> Result<(), String> {
        let objective_id = self.selected_or_err()?;
        let is_default = self.store.state().settings.default_model.as_ref() == Some(&model);
        self.command(
            ActionName::ExecutionStart,
            Intent::StartExecution {
                objective_id,
                model: model.clone(),
            },
        )?;
        // Picking a model makes it the default for the next execution.
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

    /// Whether approval would rely on a recorded override (missing or not-all-passed checks).
    /// Presentation hint for showing the reason dialog; the service decides.
    pub fn approval_needs_override(&self) -> bool {
        self.with_detail(|d| {
            let Some(candidate) = d.current_candidate() else {
                return true;
            };
            let mut latest: Vec<&super::model::VerificationSummary> = Vec::new();
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

    /// Follows a search result in the version it was found in (that version stays labelled).
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
        self.set_view(MainView::Workbench);
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

    // -----------------------------------------------------------------------------------------
    // Contained terminal (IMP-012).

    /// Opens a terminal for the selected Objective unless one is open or opening. The service
    /// decides the scope (read-write, read-only under review, or refused).
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

    /// The open terminal of the selected Objective: `(objective, terminal id)`.
    fn open_terminal_id(&self) -> Option<(ObjectiveId, String)> {
        let state = self.store.state();
        let id = state.ui.selected.clone()?;
        let session = state.terminal_for(&id).filter(|t| t.is_open())?;
        Some((id, session.terminal_id.clone()?))
    }

    /// Sends typed or pasted text to the selected Objective's open terminal (in bounded chunks).
    /// Returns `false` when no terminal is open.
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

    /// Applies a new grid size locally and tells the service (only when it changed).
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

    /// Contextual navigation from attention (a desktop notification or a board item): selects the
    /// Objective and shows its review context. Only projections are requested; nothing is
    /// approved, sent or changed in the service.
    pub fn navigate_to(&self, id: &ObjectiveId, review: bool) {
        self.select_objective(id);
        self.set_view(MainView::Workbench);
        self.set_inspector_tab(if review {
            InspectorTab::Evidence
        } else {
            InspectorTab::Overview
        });
    }

    /// Explicit Exit (or session logout). With control of the service, asks it to stop its agents
    /// and terminals and exit (nothing resumes on the next start) and returns `true`; the window
    /// closes once the service confirms. Without control (disconnected, another window holds the
    /// lease) the window cannot stop the service and only closes: returns `false`.
    pub fn exit(&self, reason: StopReason) -> bool {
        let connected = self.store.state().connection.is_connected();
        if connected && !self.store.state().is_preview() {
            self.emit(Intent::StopService { reason });
        }
        connected
    }

    /// Execution label for the status line.
    pub fn execution_line(&self) -> Option<String> {
        self.with_detail(|d| {
            let execution = format::execution_label_for(d);
            match &d.session {
                Some(s) => format!(
                    "{} · {} · generation {}",
                    s.model.label(),
                    execution,
                    s.execution_generation
                ),
                None => format!("No Agent Session · {execution}"),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation::intents::RecordingSink;
    use crate::presentation::preview;
    use crate::presentation::store::PresentationState;

    fn setup() -> (Rc<Controller>, Rc<RecordingSink>) {
        let mut state = PresentationState::new(super::super::model::ConnectionState::Connected);
        for update in preview::fixture_updates() {
            state.apply(update);
        }
        state.connection = super::super::model::ConnectionState::Connected;
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
        // Add feedback opens the feedback panel holding the new draft.
        controller.select_objective(&ObjectiveId::new(preview::MAIN_OBJECTIVE));
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
            s.objective_ui_mut(&ObjectiveId::new(preview::MAIN_OBJECTIVE))
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
    fn notification_navigation_only_requests_projections() {
        let (controller, sink) = setup();
        controller.navigate_to(&ObjectiveId::new(preview::SECOND_OBJECTIVE), true);
        let state = controller.store.state();
        assert_eq!(
            state.ui.selected.as_ref().map(ObjectiveId::as_str),
            Some(preview::SECOND_OBJECTIVE)
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
        use crate::presentation::store::StoreUpdate;
        use crate::presentation::terminal::{TerminalEnd, TerminalMode};
        let (controller, sink) = setup();
        let id = ObjectiveId::new(preview::MAIN_OBJECTIVE);
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
        // A lost connection ends terminals locally too (the service ended them with the lease).
        controller.open_terminal(80, 24).unwrap();
        controller.store.apply(StoreUpdate::Connection(
            super::super::model::ConnectionState::Reconnecting { attempt: 1 },
        ));
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
    fn exit_stops_the_service_only_with_control() {
        let (controller, sink) = setup();
        assert!(controller.exit(StopReason::Exit));
        assert!(sink.intents.borrow().contains(&Intent::StopService {
            reason: StopReason::Exit
        }));
        sink.intents.borrow_mut().clear();
        controller
            .store
            .apply(crate::presentation::store::StoreUpdate::Connection(
                super::super::model::ConnectionState::LeaseHeld,
            ));
        assert!(!controller.exit(StopReason::Logout));
        assert!(
            sink.intents.borrow().is_empty(),
            "no authority, nothing sent"
        );
    }

    #[test]
    fn selecting_an_objective_requests_its_projections() {
        let (controller, sink) = setup();
        controller.select_objective(&ObjectiveId::new(preview::SECOND_OBJECTIVE));
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
        controller.select_objective(&ObjectiveId::new(preview::MAIN_OBJECTIVE));
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
                    objective_id: ObjectiveId::new(preview::MAIN_OBJECTIVE),
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
        controller.select_objective(&ObjectiveId::new(preview::MAIN_OBJECTIVE));
        sink.intents.borrow_mut().clear();
        let err = controller.integrate().unwrap_err();
        assert!(!err.is_empty());
        assert!(sink.intents.borrow().is_empty());
    }

    #[test]
    fn stale_item_blocks_the_whole_batch() {
        let (controller, sink) = setup();
        controller.select_objective(&ObjectiveId::new(preview::MAIN_OBJECTIVE));
        sink.intents.borrow_mut().clear();
        let batch = controller.feedback_batch().unwrap();
        assert!(batch.items.iter().any(|i| i.stale));
        assert!(batch.blockers.iter().any(|b| b.contains("stale")));
        assert!(controller.submit_feedback(&batch).is_err());
        assert!(sink.intents.borrow().is_empty());
    }

    #[test]
    fn typing_in_a_draft_schedules_a_save_intent() {
        let (controller, sink) = setup();
        controller.select_objective(&ObjectiveId::new(preview::MAIN_OBJECTIVE));
        sink.intents.borrow_mut().clear();
        let draft = DraftId::new(preview::SAVED_DRAFT);
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
}
