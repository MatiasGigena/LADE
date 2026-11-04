//! The updates the transport and controller apply to the presentation state.

use crate::presentation::state::agent_session::AgentSessionUpdate;
use crate::presentation::state::model::{
    ActionSurface, ActivityItem, ActivityItemId, Basis, ChangeList, ConnectionState, Draft,
    DraftId, FileDiff, FilePage, InputSummary, ObjectiveDetail, ObjectiveId, ObjectiveSummary,
    Refusal, RepoPath, SearchResults, ServiceNotice, Settings, VerificationSummary, Workspace,
};
use crate::presentation::terminal::{TerminalEnd, TerminalMode};

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
    DraftSaveRefused {
        draft_id: DraftId,
        refusal: Refusal,
    },
    DraftDeleteRefused {
        draft_id: DraftId,
        refusal: Refusal,
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
    Settings(Settings),
    Select(ObjectiveId),
    CommandStatus {
        objective_id: ObjectiveId,
        status: Option<ServiceNotice>,
    },
    Refused {
        objective_id: ObjectiveId,
        surface: ActionSurface,
        refusal: Refusal,
    },
    RefusalCleared {
        objective_id: ObjectiveId,
        surface: ActionSurface,
    },
    AgentSession(AgentSessionUpdate),
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
    TerminalProblem {
        objective_id: ObjectiveId,
        message: String,
    },
    TerminalRefused {
        objective_id: ObjectiveId,
        refusal: Refusal,
    },
    ServiceStopped,
    NotificationsUnavailable {
        reason: String,
    },
}

/// Merges consecutive text deltas for the same activity item, so a burst of streamed text
/// applies as one change.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn delta(objective: &str, item: &str, text: &str) -> StoreUpdate {
        StoreUpdate::ActivityDelta {
            objective_id: ObjectiveId::new(objective),
            item_id: ActivityItemId::new(item),
            append_text: text.into(),
        }
    }

    #[test]
    fn consecutive_deltas_for_one_item_merge_in_order() {
        let merged = coalesce(vec![
            delta("o", "a", "1"),
            delta("o", "a", "2"),
            delta("o", "b", "3"),
            delta("o", "a", "4"),
        ]);
        assert_eq!(
            merged,
            vec![
                delta("o", "a", "12"),
                delta("o", "b", "3"),
                delta("o", "a", "4")
            ]
        );
    }

    #[test]
    fn deltas_for_the_same_item_id_in_other_objectives_stay_apart() {
        let merged = coalesce(vec![delta("o", "a", "1"), delta("p", "a", "2")]);
        assert_eq!(merged, vec![delta("o", "a", "1"), delta("p", "a", "2")]);
    }

    #[test]
    fn any_other_update_breaks_a_run_of_deltas() {
        let between = StoreUpdate::WorktreeChanged {
            objective_id: ObjectiveId::new("o"),
        };
        let merged = coalesce(vec![
            delta("o", "a", "1"),
            between.clone(),
            delta("o", "a", "2"),
        ]);
        assert_eq!(
            merged,
            vec![delta("o", "a", "1"), between, delta("o", "a", "2")]
        );
        assert!(coalesce(Vec::new()).is_empty());
    }
}
