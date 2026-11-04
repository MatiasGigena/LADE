//! The Change Set, file viewer, review selection and search, as the controller drives them.

use super::intents::Intent;
use crate::presentation::layout::Region;
use crate::presentation::state::model::{
    Basis, ChangeStatus, Load, ObjectiveId, Reference, ReferenceVersion, RepoPath, SearchResult,
    SearchResultKind, SearchScope, Side,
};
use crate::presentation::state::store::{
    Change, MainView, Reveal, ViewerContent, ViewerKey, ViewerMode,
};
use crate::presentation::text::map::{MappedSelection, SelectionError};

use super::Controller;

impl Controller {
    pub(super) fn load_changes(&self, id: &ObjectiveId, basis: Basis) {
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

    pub(super) fn clear_viewer(&self) {
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

    pub(super) fn load_viewer(&self, id: &ObjectiveId, path: &RepoPath) {
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
}
