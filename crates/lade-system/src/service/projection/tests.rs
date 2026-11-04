use lade_protocol::client::{
    ActionName, AvailableAction, ExecutionStartCommand, ObjectiveDetail, Request,
};
use lade_protocol::types::Model;

use super::MAX_SUMMARY_BADGES;
use super::actions::{
    BUSY, NO_WORKTREE_YET, delete_history_blocker, freeze_blocker, terminal_blocker,
};
use crate::service::test_harness::Harness;

fn action(detail: &ObjectiveDetail, name: ActionName) -> &AvailableAction {
    detail
        .available_actions
        .iter()
        .find(|a| a.action == name)
        .unwrap_or_else(|| panic!("{name:?} is listed"))
}

#[test]
fn a_defined_objective_offers_start_and_explains_what_it_cannot_do_yet() {
    let mut harness = Harness::new("project-defined");
    let (_, oid) = harness.create_objective("Defined");
    let st = harness.service.objectives[&oid].clone();
    let detail = harness.service.detail_of(&st);
    assert!(action(&detail, ActionName::ExecutionStart).enabled);
    let message = action(&detail, ActionName::MessageSend);
    assert!(!message.enabled);
    assert!(message.reason.is_some());
    let freeze = action(&detail, ActionName::ReviewFreeze);
    assert_eq!(
        freeze.reason.as_deref(),
        Some("Start the agent first; there is nothing to freeze yet.")
    );
    assert_eq!(
        action(&detail, ActionName::TerminalOpen).reason.as_deref(),
        Some(NO_WORKTREE_YET)
    );
    assert_eq!(freeze_blocker(&st), freeze.reason.as_deref());
    assert_eq!(
        delete_history_blocker(&st),
        Some("Clean up the Objective before deleting its Git history.")
    );
    let summary = harness.service.summary(&st);
    assert_eq!(summary.objective_id, detail.objective_id);
    assert!(summary.badges.len() <= MAX_SUMMARY_BADGES);
}

#[test]
fn a_busy_objective_disables_its_actions_with_the_busy_reason() {
    let mut harness = Harness::new("project-busy");
    let (id, oid) = harness.create_objective("Busy");
    harness
        .call(Request::ExecutionStart(ExecutionStartCommand {
            objective_id: id,
            model: Model {
                provider: "anthropic".into(),
                id: "m".into(),
            },
            thinking: None,
        }))
        .expect("start");
    let st = harness.service.objectives[&oid].clone();
    assert!(st.row.worktree.as_ref().is_some_and(|w| w.is_dir()));
    assert!(
        terminal_blocker(&st).is_none(),
        "a provisioned worktree opens a terminal"
    );
    assert!(freeze_blocker(&st).is_none());
    harness.service.rt(oid).busy = Some("freeze");
    let detail = harness.service.detail_of(&st);
    let freeze = action(&detail, ActionName::ReviewFreeze);
    assert!(!freeze.enabled);
    assert_eq!(freeze.reason.as_deref(), Some(BUSY));
}

#[test]
fn the_target_is_resolved_fresh_and_then_served_from_the_cache() {
    let mut harness = Harness::new("project-target");
    let (_, oid) = harness.create_objective("Target");
    let st = harness.service.objectives[&oid].clone();
    let fresh = harness.service.fresh_target(&st).expect("main resolves");
    assert_eq!(harness.service.cached_target(&st), Some(fresh.clone()));
    let workspace = st.objective().record().workspace_id;
    assert_eq!(harness.service.resolve_target(workspace, "missing"), None);
    assert_eq!(
        harness.service.resolve_target(workspace, "main"),
        Some(fresh)
    );
}
