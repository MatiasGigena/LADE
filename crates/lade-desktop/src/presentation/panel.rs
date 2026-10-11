//! The right panel's progressive disclosure, decided without GTK: which service-available action
//! is the one **next action** for the lifecycle step, how the other actions are grouped (More
//! actions, destructive ones last), the one-line essentials and the collapsed Details rows.
//!
//! Authority stays with the service: every action shown here carries exactly the availability of
//! `available_actions` (`ObjectiveDetail::availability`). The client only chooses which offered
//! action to emphasise; it never enables, hides a disabled reason, or invents an action.

use super::format;
use super::model::{
    ActionAvailability, ActionName, ExecutionState, Lifecycle, ObjectiveDetail, ObjectiveSummary,
    Timestamp,
};

/// Accessible name and visible label of the collapsed group of the other actions.
pub const MORE_ACTIONS_TITLE: &str = "More actions";
/// Accessible name and visible label of the collapsed metadata.
pub const DETAILS_TITLE: &str = "Details";

/// How an action is drawn. `Primary` (black) is used only for the enabled next action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Primary,
    Neutral,
    Destructive,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelAction {
    pub action: ActionName,
    /// Visible label and accessible name.
    pub label: String,
    /// Exactly the service's availability for `action`.
    pub availability: ActionAvailability,
    /// The service's note on an *enabled* action (for example "verification missing; an override
    /// reason is required"). Disabled reasons live in `availability`.
    pub note: Option<String>,
    pub tone: Tone,
}

/// Lifecycle actions the Overview offers, in the order "More actions" lists them.
const MORE_ORDER: [ActionName; 9] = [
    ActionName::ExecutionStart,
    ActionName::ExecutionResume,
    ActionName::ExecutionCancel,
    ActionName::ReviewFreeze,
    ActionName::ReviewReturn,
    ActionName::ReviewRefresh,
    ActionName::ReviewApprove,
    ActionName::ReviewIntegrate,
    ActionName::ObjectiveComplete,
];

/// Destructive actions: always last, separated, never the primary treatment.
pub const DESTRUCTIVE: [ActionName; 3] = [
    ActionName::ObjectiveDiscard,
    ActionName::ObjectiveCleanup,
    ActionName::ObjectiveDeleteHistory,
];

/// Steps that move the collaboration forward, most advanced first: the first one the service
/// enables is the next action.
const PROGRESSION: [ActionName; 6] = [
    ActionName::ReviewIntegrate,
    ActionName::ReviewRefresh,
    ActionName::ReviewApprove,
    ActionName::ExecutionResume,
    ActionName::ReviewFreeze,
    ActionName::ExecutionStart,
];

pub fn action_label(action: ActionName, detail: &ObjectiveDetail) -> String {
    match action {
        ActionName::ExecutionStart => "Start agent".to_owned(),
        ActionName::ExecutionCancel => "Cancel agent".to_owned(),
        ActionName::ExecutionResume => "Resume agent".to_owned(),
        ActionName::ReviewFreeze => "Freeze for review".to_owned(),
        ActionName::ReviewReturn => "Return for revision".to_owned(),
        ActionName::ReviewRefresh => "Refresh against target".to_owned(),
        ActionName::ReviewApprove => match detail.current_candidate() {
            Some(c) => format!("Approve Candidate {}", c.ordinal),
            None => "Approve".to_owned(),
        },
        ActionName::ReviewIntegrate => {
            format!("Integrate into {}", detail.integration_target.branch)
        }
        ActionName::ObjectiveComplete => "Complete without changes".to_owned(),
        ActionName::ObjectiveDiscard => "Discard Objective".to_owned(),
        ActionName::ObjectiveCleanup => "Clean up branch and worktree".to_owned(),
        ActionName::ObjectiveDeleteHistory => "Delete Git history".to_owned(),
        ActionName::ObjectiveUpdateIntent => "Edit intent".to_owned(),
        ActionName::MessageSend => "Send".to_owned(),
        ActionName::FeedbackSubmit => "Submit feedback".to_owned(),
        ActionName::ReviewVerify => "Run check".to_owned(),
    }
}

/// The service's note on an enabled action, if it gave one.
pub fn enabled_note(detail: &ObjectiveDetail, action: ActionName) -> Option<String> {
    detail
        .available_actions
        .iter()
        .find(|a| a.action == action && a.enabled)
        .and_then(|a| a.reason.clone())
        .filter(|r| !r.trim().is_empty())
}

/// The primary (black) treatment is only for a cleanly available step: an enabled action that
/// carries a service caveat (for example an approval that needs an override reason) is neutral.
fn entry(detail: &ObjectiveDetail, action: ActionName, tone: Tone) -> PanelAction {
    let availability = detail.availability(action);
    let note = enabled_note(detail, action);
    let tone = match (&availability, tone) {
        (ActionAvailability::Enabled, Tone::Primary) if note.is_some() => Tone::Neutral,
        (ActionAvailability::Enabled, tone) => tone,
        (_, Tone::Destructive) => Tone::Destructive,
        _ => Tone::Neutral,
    };
    PanelAction {
        action,
        label: action_label(action, detail),
        note,
        availability,
        tone,
    }
}

/// The step the lifecycle is at, shown (disabled, with the service's reason) when the service
/// enables no step forward.
fn expected_step(detail: &ObjectiveDetail) -> Option<ActionName> {
    match detail.summary.lifecycle {
        Lifecycle::Defined => Some(ActionName::ExecutionStart),
        Lifecycle::Active => Some(
            if matches!(
                detail.summary.execution.state,
                ExecutionState::Cancelled | ExecutionState::Failed
            ) {
                ActionName::ExecutionResume
            } else {
                ActionName::ReviewFreeze
            },
        ),
        Lifecycle::Frozen => Some(ActionName::ReviewApprove),
        Lifecycle::Approved => Some(ActionName::ReviewIntegrate),
        Lifecycle::Integrated | Lifecycle::CompletedNoChanges | Lifecycle::Discarded => None,
    }
}

/// The one next action: the most advanced step the service enables; for a closed Objective, an
/// enabled Clean up (destructive treatment, confirmed in a dialog); otherwise the lifecycle's own
/// step with the service's reason. `None` when nothing applies.
pub fn next_action(detail: &ObjectiveDetail) -> Option<PanelAction> {
    if let Some(action) = PROGRESSION
        .into_iter()
        .find(|a| detail.availability(*a) == ActionAvailability::Enabled)
    {
        return Some(entry(detail, action, Tone::Primary));
    }
    if detail.summary.lifecycle.is_archived() {
        return (detail.availability(ActionName::ObjectiveCleanup) == ActionAvailability::Enabled)
            .then(|| entry(detail, ActionName::ObjectiveCleanup, Tone::Destructive));
    }
    expected_step(detail)
        .filter(|a| detail.availability(*a) != ActionAvailability::NotOffered)
        .map(|a| entry(detail, a, Tone::Neutral))
}

/// Everything else the service offers, for "More actions": lifecycle actions in a stable order,
/// then the destructive ones. The next action is not repeated; not-offered actions are left out;
/// Delete Git history disappears once the history is deleted (the panel says so instead).
pub fn more_actions(
    detail: &ObjectiveDetail,
    next: Option<&PanelAction>,
) -> (Vec<PanelAction>, Vec<PanelAction>) {
    let shown = |action: &ActionName| {
        next.is_none_or(|n| n.action != *action)
            && detail.availability(*action) != ActionAvailability::NotOffered
    };
    let regular = MORE_ORDER
        .iter()
        .filter(|a| shown(a))
        .map(|a| entry(detail, *a, Tone::Neutral))
        .collect();
    let destructive = DESTRUCTIVE
        .iter()
        .filter(|a| shown(a))
        .filter(|a| !(detail.history_deleted && **a == ActionName::ObjectiveDeleteHistory))
        .map(|a| entry(detail, *a, Tone::Destructive))
        .collect();
    (regular, destructive)
}

/// One status line per surface: sign, state and the attention reasons ("! Needs attention ·
/// check failed · unsent drafts"). The sign makes the state readable without colour.
pub fn status_line(summary: &ObjectiveSummary) -> (&'static str, String) {
    let (sign, text) = format::status_pill(summary.lifecycle, summary.attention);
    if summary.badges.is_empty() {
        (sign, text.to_owned())
    } else {
        (sign, format!("{text} · {}", summary.badge_text()))
    }
}

/// The always-visible essentials: agent model and execution state.
pub fn agent_line(detail: &ObjectiveDetail) -> String {
    let execution = format::execution_label_for(detail);
    match &detail.session {
        Some(session) => format!("{} · {execution}", session.model.label()),
        None => format!("No Agent Session yet · {execution}"),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailRow {
    pub key: &'static str,
    pub value: String,
    /// Full value (tooltip and accessible description) when `value` is shortened.
    pub full: Option<String>,
    pub mono: bool,
}

/// The collapsed Details: lifecycle, last update, Agent Session identity, changes, branch, target
/// and start.
pub fn details_rows(detail: &ObjectiveDetail, now: Timestamp) -> Vec<DetailRow> {
    let row = |key, value: String, full: Option<String>, mono| DetailRow {
        key,
        value,
        full,
        mono,
    };
    let mut rows = vec![row(
        "Lifecycle",
        format::lifecycle_label(detail.summary.lifecycle).to_owned(),
        None,
        false,
    )];
    rows.push(row(
        "Updated",
        format::relative_time(now, detail.summary.updated_at),
        None,
        false,
    ));
    if let Some(session) = &detail.session {
        rows.push(row(
            "Session",
            format!(
                "{} · generation {}",
                session.session_id.short(),
                session.execution_generation
            ),
            Some(format!(
                "Agent Session {} · association r{} · execution generation {}",
                session.session_id.as_str(),
                session.association_revision,
                session.execution_generation
            )),
            false,
        ));
    }
    let files = detail.change_counts.files;
    rows.push(row(
        "Changes",
        format!(
            "{files} file{} · +{} −{}",
            if files == 1 { "" } else { "s" },
            detail.change_counts.added,
            detail.change_counts.removed
        ),
        None,
        false,
    ));
    rows.push(row("Branch", detail.branch.clone(), None, true));
    rows.push(row(
        "Target",
        detail.integration_target.label(),
        Some(format!(
            "{} at {}",
            detail.integration_target.branch, detail.integration_target.commit.hex
        )),
        true,
    ));
    rows.push(row(
        "Started at",
        detail.start_commit.short().to_owned(),
        Some(detail.start_commit.hex.clone()),
        true,
    ));
    rows
}

/// The sidebar row's single secondary line: sign, status and attention reasons.
pub fn objective_row_line(summary: &ObjectiveSummary) -> String {
    let (sign, text) = status_line(summary);
    format!("{sign} {text}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation::model::{AvailableAction, Badge};
    use crate::presentation::preview;
    use crate::presentation::store::PresentationState;

    fn details() -> Vec<ObjectiveDetail> {
        let mut state =
            PresentationState::new(crate::presentation::model::ConnectionState::Connected);
        for update in preview::fixture_updates() {
            state.apply(update);
        }
        let details: Vec<ObjectiveDetail> = state
            .details
            .values()
            .filter_map(|d| d.loaded().cloned())
            .collect();
        assert!(
            details.len() > 1,
            "the preview fixture has several Objectives"
        );
        details
    }

    fn with(
        detail: &ObjectiveDetail,
        actions: &[(ActionName, bool, Option<&str>)],
    ) -> ObjectiveDetail {
        let mut d = detail.clone();
        d.available_actions = actions
            .iter()
            .map(|(action, enabled, reason)| AvailableAction {
                action: *action,
                enabled: *enabled,
                reason: reason.map(str::to_owned),
            })
            .collect();
        d
    }

    /// Every action the service offers, with the service's own projection (one entry each).
    fn all(enabled: &[ActionName]) -> Vec<(ActionName, bool, Option<&'static str>)> {
        MORE_ORDER
            .iter()
            .chain(DESTRUCTIVE.iter())
            .map(|a| {
                let on = enabled.contains(a);
                (*a, on, (!on).then_some("blocked by the service"))
            })
            .collect()
    }

    #[test]
    fn next_action_mirrors_the_services_projection() {
        for base in details() {
            let next = next_action(&base);
            if let Some(next) = &next {
                // Never more available than the service says, and the same reason.
                assert_eq!(next.availability, base.availability(next.action));
                if next.tone == Tone::Primary {
                    assert_eq!(next.availability, ActionAvailability::Enabled);
                }
            }
            // Nothing enabled: no primary next action at all.
            let none = with(&base, &all(&[]));
            match next_action(&none) {
                Some(n) => {
                    assert_ne!(n.availability, ActionAvailability::Enabled);
                    assert_ne!(n.tone, Tone::Primary);
                    assert!(
                        matches!(n.availability, ActionAvailability::Disabled { ref reason } if reason == "blocked by the service")
                    );
                }
                None => assert!(base.summary.lifecycle.is_archived()),
            }
        }
    }

    #[test]
    fn the_most_advanced_enabled_step_is_next_and_integrate_only_when_enabled() {
        let base = details().into_iter().next().unwrap();
        let d = with(
            &base,
            &all(&[ActionName::ReviewApprove, ActionName::ReviewFreeze]),
        );
        assert_eq!(next_action(&d).unwrap().action, ActionName::ReviewApprove);
        let d = with(
            &base,
            &all(&[ActionName::ReviewIntegrate, ActionName::ReviewReturn]),
        );
        let next = next_action(&d).unwrap();
        assert_eq!(next.action, ActionName::ReviewIntegrate);
        assert_eq!(next.tone, Tone::Primary);
        // Integrate disabled: never the primary, whatever the lifecycle.
        let mut approved = with(&base, &all(&[ActionName::ReviewReturn]));
        approved.summary.lifecycle = Lifecycle::Approved;
        let next = next_action(&approved).unwrap();
        assert_eq!(next.action, ActionName::ReviewIntegrate);
        assert_eq!(next.tone, Tone::Neutral);
        assert!(matches!(
            next.availability,
            ActionAvailability::Disabled { .. }
        ));
        // Return, Cancel and Complete are decisions, never suggested as the next step.
        let d = with(
            &base,
            &all(&[
                ActionName::ReviewReturn,
                ActionName::ExecutionCancel,
                ActionName::ObjectiveComplete,
                ActionName::ObjectiveDiscard,
            ]),
        );
        assert!(next_action(&d).is_none_or(|n| n.tone != Tone::Primary));
    }

    #[test]
    fn closed_objectives_suggest_only_an_enabled_clean_up_as_destructive() {
        let base = details().into_iter().next().unwrap();
        let mut d = with(&base, &all(&[ActionName::ObjectiveCleanup]));
        d.summary.lifecycle = Lifecycle::Integrated;
        let next = next_action(&d).unwrap();
        assert_eq!(next.action, ActionName::ObjectiveCleanup);
        assert_eq!(next.tone, Tone::Destructive);
        let mut d = with(&base, &all(&[ActionName::ObjectiveDeleteHistory]));
        d.summary.lifecycle = Lifecycle::Integrated;
        assert!(
            next_action(&d).is_none(),
            "Delete Git history is never suggested"
        );
    }

    #[test]
    fn every_offered_action_is_shown_once_with_its_reason() {
        for d in details() {
            let next = next_action(&d);
            let (regular, destructive) = more_actions(&d, next.as_ref());
            let shown: Vec<&PanelAction> =
                next.iter().chain(&regular).chain(&destructive).collect();
            for action in MORE_ORDER.iter().chain(DESTRUCTIVE.iter()) {
                let count = shown.iter().filter(|p| p.action == *action).count();
                let offered = d.availability(*action) != ActionAvailability::NotOffered;
                let deleted = d.history_deleted && *action == ActionName::ObjectiveDeleteHistory;
                assert_eq!(
                    count,
                    usize::from(offered && !deleted),
                    "{action:?} in {}",
                    d.summary.title
                );
            }
            for p in &shown {
                // Disabled reasons stay exposed exactly as the service gave them.
                assert_eq!(p.availability, d.availability(p.action));
                assert!(!p.label.is_empty());
            }
            assert!(destructive.iter().all(|p| p.tone == Tone::Destructive));
            assert!(regular.iter().all(|p| p.tone != Tone::Primary));
        }
    }

    #[test]
    fn enabled_notes_are_kept() {
        let base = details().into_iter().next().unwrap();
        let mut d = with(&base, &all(&[]));
        d.available_actions.push(AvailableAction {
            action: ActionName::ReviewApprove,
            enabled: true,
            reason: Some("verification missing; an override reason is required".into()),
        });
        d.available_actions
            .retain(|a| !(a.action == ActionName::ReviewApprove && !a.enabled));
        let next = next_action(&d).unwrap();
        assert_eq!(next.action, ActionName::ReviewApprove);
        assert_eq!(
            next.note.as_deref(),
            Some("verification missing; an override reason is required")
        );
        // Enabled, but with a caveat: the next step, never the primary treatment.
        assert_eq!(next.availability, ActionAvailability::Enabled);
        assert_eq!(next.tone, Tone::Neutral);
    }

    #[test]
    fn status_is_one_line_with_sign_and_reasons() {
        let base = details().into_iter().next().unwrap();
        let mut s = base.summary.clone();
        s.badges = vec![Badge::VerificationFailed, Badge::UnsentDrafts];
        let (sign, text) = status_line(&s);
        assert!(!sign.is_empty());
        assert!(text.contains(" · "));
        assert_eq!(objective_row_line(&s), format!("{sign} {text}"));
        assert!(!objective_row_line(&s).contains('\n'));
        s.badges.clear();
        assert!(!status_line(&s).1.contains(" · "));
    }

    #[test]
    fn details_keep_every_identity_with_the_full_value() {
        let d = details().into_iter().find(|d| d.session.is_some()).unwrap();
        let rows = details_rows(&d, d.summary.updated_at);
        let keys: Vec<&str> = rows.iter().map(|r| r.key).collect();
        for key in [
            "Lifecycle",
            "Updated",
            "Session",
            "Changes",
            "Branch",
            "Target",
            "Started at",
        ] {
            assert_eq!(keys.iter().filter(|k| **k == key).count(), 1, "{key}");
        }
        let session = rows.iter().find(|r| r.key == "Session").unwrap();
        let s = d.session.as_ref().unwrap();
        assert!(
            session
                .full
                .as_deref()
                .unwrap()
                .contains(s.session_id.as_str())
        );
        assert!(
            session
                .value
                .contains(&format!("generation {}", s.execution_generation))
        );
        let start = rows.iter().find(|r| r.key == "Started at").unwrap();
        assert_eq!(start.full.as_deref(), Some(d.start_commit.hex.as_str()));
    }
}
