use super::*;
use crate::presentation::state::model::{
    ActivityItem, ActivityItemId, ActivityRole, ExecutionState, Lifecycle, NoticeLevel,
    NoticeScope, ReportCode, Timestamp,
};

/// An Assistant activity item with no title or tool.
pub(super) fn item(id: &str, seq: u64, text: &str) -> ActivityItem {
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

pub(super) fn summary(
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
        execution: crate::presentation::state::model::Execution::new(ExecutionState::None, 0),
        attention,
        badges: Vec::new(),
        updated_at: Timestamp(updated),
        revision: 1,
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
fn notices_received_keeps_counting_past_the_retained_cap() {
    let mut state = PresentationState::default();
    for n in 0..MAX_NOTICES + 5 {
        state.apply(StoreUpdate::Notice(ServiceNotice {
            level: NoticeLevel::Warning,
            code: ReportCode::Recovered,
            message: format!("notice {n}"),
            scope: NoticeScope::General,
        }));
    }
    assert_eq!(state.notices.len(), MAX_NOTICES);
    assert_eq!(state.notices_received, MAX_NOTICES + 5);
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
