use std::path::PathBuf;
use std::rc::Rc;

use super::keys::Keymap;
use super::layout::{LayoutClass, Region};
use super::state::model::{Load, ObjectiveDetail, ObjectiveId};
use super::state::store::{Change, InspectorTab, MainView, Store};
use intents::{Intent, IntentSink, StopReason};

mod agent_session;
mod commands;
mod feedback;
pub mod intents;
mod review;
mod terminal;
#[cfg(test)]
mod tests;

pub use feedback::{FeedbackBatch, MAX_FEEDBACK_BATCH, UnsavedDraft};

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

    fn with_detail<T>(&self, f: impl FnOnce(&ObjectiveDetail) -> T) -> Option<T> {
        self.store.state().selected_detail().map(f)
    }
}
