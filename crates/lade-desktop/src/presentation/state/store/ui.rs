//! Per-window view state: the main view, inspector tab, per-objective review UI and viewer.

use std::collections::HashMap;

use crate::presentation::keys::Keymap;
use crate::presentation::layout::{LayoutClass, Region};
use crate::presentation::state::model::{
    Basis, DeliveryMode, FileContent, FileDiff, ObjectiveId, Reference, RepoPath, Side,
};

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
    pub keymap: Keymap,
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
    pub reveal: Option<Reveal>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reveal {
    pub key: ViewerKey,
    pub start: u64,
    pub end: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_objective_starts_on_the_working_diff_prompting_and_steering_feedback() {
        let ui = ObjectiveUi::default();
        assert_eq!(ui.basis, Basis::Working);
        assert_eq!(ui.mode, ViewerMode::Diff);
        assert_eq!(ui.composer_mode, DeliveryMode::Prompt);
        assert_eq!(ui.feedback_mode, DeliveryMode::Steer);
        assert!(ui.file.is_none() && ui.selection.is_none() && ui.composer_text.is_empty());
    }
}
