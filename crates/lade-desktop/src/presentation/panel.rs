use super::state::agent_session::{Catalog, ProviderStatus};
use super::state::model::{
    ActionAvailability, ActionName, Lifecycle, ObjectiveDetail, ObjectiveSummary, Timestamp,
};
use super::text::format;

pub const MORE_ACTIONS_TITLE: &str = "More actions";
pub const DETAILS_TITLE: &str = "Details";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    Overview,
    Evidence,
    Feedback,
    Activity,
}

pub const ALL_ACTIONS: [ActionName; 16] = [
    ActionName::ObjectiveUpdateIntent,
    ActionName::ExecutionStart,
    ActionName::ExecutionCancel,
    ActionName::ExecutionResume,
    ActionName::MessageSend,
    ActionName::FeedbackSubmit,
    ActionName::ReviewFreeze,
    ActionName::ReviewReturn,
    ActionName::ReviewVerify,
    ActionName::ReviewApprove,
    ActionName::ReviewRefresh,
    ActionName::ReviewIntegrate,
    ActionName::ObjectiveComplete,
    ActionName::ObjectiveDiscard,
    ActionName::ObjectiveCleanup,
    ActionName::ObjectiveDeleteHistory,
];

pub fn home(action: ActionName) -> Surface {
    match action {
        ActionName::ExecutionStart
        | ActionName::ExecutionCancel
        | ActionName::ExecutionResume
        | ActionName::MessageSend => Surface::Activity,
        ActionName::ReviewVerify => Surface::Evidence,
        ActionName::FeedbackSubmit => Surface::Feedback,
        ActionName::ObjectiveUpdateIntent
        | ActionName::ReviewFreeze
        | ActionName::ReviewReturn
        | ActionName::ReviewApprove
        | ActionName::ReviewRefresh
        | ActionName::ReviewIntegrate
        | ActionName::ObjectiveComplete
        | ActionName::ObjectiveDiscard
        | ActionName::ObjectiveCleanup
        | ActionName::ObjectiveDeleteHistory => Surface::Overview,
    }
}

pub const ACTIVITY_CONTROLS: [ActionName; 3] = [
    ActionName::ExecutionStart,
    ActionName::ExecutionCancel,
    ActionName::ExecutionResume,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Primary,
    Neutral,
    Destructive,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelAction {
    pub action: ActionName,
    pub label: String,
    pub availability: ActionAvailability,
    pub note: Option<String>,
    pub tone: Tone,
}

const MORE_ORDER: [ActionName; 6] = [
    ActionName::ReviewFreeze,
    ActionName::ReviewReturn,
    ActionName::ReviewRefresh,
    ActionName::ReviewApprove,
    ActionName::ReviewIntegrate,
    ActionName::ObjectiveComplete,
];

pub const DESTRUCTIVE: [ActionName; 3] = [
    ActionName::ObjectiveDiscard,
    ActionName::ObjectiveCleanup,
    ActionName::ObjectiveDeleteHistory,
];

const PROGRESSION: [ActionName; 4] = [
    ActionName::ReviewIntegrate,
    ActionName::ReviewRefresh,
    ActionName::ReviewApprove,
    ActionName::ReviewFreeze,
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

pub fn enabled_note(detail: &ObjectiveDetail, action: ActionName) -> Option<String> {
    detail
        .available_actions
        .iter()
        .find(|a| a.action == action && a.enabled)
        .and_then(|a| a.reason.clone())
        .filter(|r| !r.trim().is_empty())
}

pub fn entry(detail: &ObjectiveDetail, action: ActionName, tone: Tone) -> PanelAction {
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

fn expected_step(detail: &ObjectiveDetail) -> Option<ActionName> {
    match detail.summary.lifecycle {
        Lifecycle::Active => Some(ActionName::ReviewFreeze),
        Lifecycle::Frozen => Some(ActionName::ReviewApprove),
        Lifecycle::Approved => Some(ActionName::ReviewIntegrate),
        Lifecycle::Defined
        | Lifecycle::Integrated
        | Lifecycle::CompletedNoChanges
        | Lifecycle::Discarded => None,
    }
}

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

pub fn overview_actions(detail: &ObjectiveDetail) -> Vec<ActionName> {
    let next = next_action(detail);
    let (regular, destructive) = more_actions(detail, next.as_ref());
    let mut actions: Vec<ActionName> = next
        .iter()
        .chain(&regular)
        .chain(&destructive)
        .map(|p| p.action)
        .collect();
    if detail.availability(ActionName::ObjectiveUpdateIntent) != ActionAvailability::NotOffered {
        actions.push(ActionName::ObjectiveUpdateIntent);
    }
    actions
}

pub fn activity_controls(detail: &ObjectiveDetail) -> Vec<PanelAction> {
    ACTIVITY_CONTROLS
        .iter()
        .filter(|a| detail.availability(**a) != ActionAvailability::NotOffered)
        .map(|a| {
            let tone = if *a == ActionName::ExecutionStart {
                Tone::Primary
            } else {
                Tone::Neutral
            };
            entry(detail, *a, tone)
        })
        .collect()
}

pub fn agent_pointer(detail: &ObjectiveDetail, start_blocker: Option<&str>) -> Option<String> {
    let enabled = |a| detail.availability(a) == ActionAvailability::Enabled;
    if enabled(ActionName::ExecutionStart) {
        Some(match start_blocker {
            Some(blocker) => format!(
                "The agent has not started — it cannot start yet: {}. Details are in Activity.",
                blocker.trim_end_matches('.')
            ),
            None => "The agent has not started yet — start it in Activity.".to_owned(),
        })
    } else if enabled(ActionName::ExecutionResume) {
        Some(format!(
            "The agent is stopped ({}) — resume it in Activity.",
            format::execution_label(&detail.summary.execution).to_lowercase()
        ))
    } else {
        None
    }
}

pub fn start_blocker(catalog: Option<&Catalog>) -> Option<String> {
    let catalog = catalog?;
    if catalog
        .providers
        .iter()
        .any(|p| p.status != ProviderStatus::MissingCredentials)
    {
        return None;
    }
    Some(
        catalog
            .error
            .clone()
            .filter(|e| !e.trim().is_empty())
            .unwrap_or_else(|| "no model provider has credentials".to_owned()),
    )
}

pub fn status_line(summary: &ObjectiveSummary) -> (&'static str, String) {
    let (sign, text) = format::status_pill(summary.lifecycle, summary.attention);
    if summary.badges.is_empty() {
        (sign, text.to_owned())
    } else {
        (sign, format!("{text} · {}", summary.badge_text()))
    }
}

pub fn objective_row_line(summary: &ObjectiveSummary) -> Option<String> {
    let (sign, text) = format::status_pill(summary.lifecycle, summary.attention);
    if !summary.badges.is_empty() {
        Some(format!("{sign} {}", summary.badge_text()))
    } else if summary.lifecycle.is_archived() {
        Some(format!("{sign} {text}"))
    } else {
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailRow {
    pub key: &'static str,
    pub value: String,
    pub full: Option<String>,
    pub mono: bool,
}

pub fn details_rows(detail: &ObjectiveDetail, now: Timestamp) -> Vec<DetailRow> {
    let row = |key, value: String, full: Option<String>, mono| DetailRow {
        key,
        value,
        full,
        mono,
    };
    vec![
        row(
            "Lifecycle",
            format::lifecycle_label(detail.summary.lifecycle).to_owned(),
            None,
            false,
        ),
        row(
            "Updated",
            format::relative_time(now, detail.summary.updated_at),
            None,
            false,
        ),
        row("Branch", detail.branch.clone(), None, true),
        row(
            "Started at",
            detail.start_commit.short().to_owned(),
            Some(detail.start_commit.hex.clone()),
            true,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation::state::model::{AvailableAction, Badge, ExecutionState};
    use crate::presentation::state::store::PresentationState;
    use crate::presentation::test_fixture;

    fn details() -> Vec<ObjectiveDetail> {
        let mut state =
            PresentationState::new(crate::presentation::state::model::ConnectionState::Connected);
        for update in test_fixture::fixture_updates() {
            state.apply(update);
        }
        let details: Vec<ObjectiveDetail> = state
            .details
            .values()
            .filter_map(|d| d.loaded().cloned())
            .collect();
        assert!(details.len() > 1, "the test data has several Objectives");
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

    fn all(enabled: &[ActionName]) -> Vec<(ActionName, bool, Option<&'static str>)> {
        ALL_ACTIONS
            .iter()
            .map(|a| {
                let on = enabled.contains(a);
                (*a, on, (!on).then_some("blocked by the service"))
            })
            .collect()
    }

    fn rendered(d: &ObjectiveDetail) -> Vec<(Surface, ActionName)> {
        let offered = |a: ActionName| d.availability(a) != ActionAvailability::NotOffered;
        let mut out: Vec<(Surface, ActionName)> = overview_actions(d)
            .into_iter()
            .map(|a| (Surface::Overview, a))
            .collect();
        out.extend(
            activity_controls(d)
                .into_iter()
                .map(|p| (Surface::Activity, p.action)),
        );
        for (surface, action) in [
            (Surface::Activity, ActionName::MessageSend),
            (Surface::Evidence, ActionName::ReviewVerify),
            (Surface::Feedback, ActionName::FeedbackSubmit),
        ] {
            if offered(action) {
                out.push((surface, action));
            }
        }
        out
    }

    #[test]
    fn every_action_is_rendered_in_exactly_one_surface() {
        let mut cases: Vec<ObjectiveDetail> = details();
        let base = cases[0].clone();
        for lifecycle in [
            Lifecycle::Defined,
            Lifecycle::Active,
            Lifecycle::Frozen,
            Lifecycle::Approved,
            Lifecycle::Integrated,
        ] {
            for enabled in [&[][..], &ALL_ACTIONS[..]] {
                let mut d = with(&base, &all(enabled));
                d.summary.lifecycle = lifecycle;
                cases.push(d);
            }
        }
        for d in &cases {
            let shown = rendered(d);
            for action in ALL_ACTIONS {
                let places: Vec<Surface> = shown
                    .iter()
                    .filter(|(_, a)| *a == action)
                    .map(|(s, _)| *s)
                    .collect();
                let offered = d.availability(action) != ActionAvailability::NotOffered;
                let deleted = d.history_deleted && action == ActionName::ObjectiveDeleteHistory;
                let expected = usize::from(offered && !deleted);
                assert_eq!(places.len(), expected, "{action:?} in {}", d.summary.title);
                assert!(places.iter().all(|s| *s == home(action)), "{action:?}");
            }
        }
    }

    #[test]
    fn next_action_mirrors_the_services_projection() {
        for base in details() {
            let next = next_action(&base);
            if let Some(next) = &next {
                assert_eq!(next.availability, base.availability(next.action));
                assert_eq!(home(next.action), Surface::Overview);
                if next.tone == Tone::Primary {
                    assert_eq!(next.availability, ActionAvailability::Enabled);
                }
            }
            let none = with(&base, &all(&[]));
            match next_action(&none) {
                Some(n) => {
                    assert_ne!(n.availability, ActionAvailability::Enabled);
                    assert_ne!(n.tone, Tone::Primary);
                    assert!(
                        matches!(n.availability, ActionAvailability::Disabled { ref reason } if reason == "blocked by the service")
                    );
                }
                None => assert!(matches!(
                    base.summary.lifecycle,
                    Lifecycle::Defined
                        | Lifecycle::Integrated
                        | Lifecycle::CompletedNoChanges
                        | Lifecycle::Discarded
                )),
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
        let mut approved = with(&base, &all(&[ActionName::ReviewReturn]));
        approved.summary.lifecycle = Lifecycle::Approved;
        let next = next_action(&approved).unwrap();
        assert_eq!(next.action, ActionName::ReviewIntegrate);
        assert_eq!(next.tone, Tone::Neutral);
        assert!(matches!(
            next.availability,
            ActionAvailability::Disabled { .. }
        ));
        let d = with(
            &base,
            &all(&[
                ActionName::ReviewReturn,
                ActionName::ExecutionCancel,
                ActionName::ExecutionStart,
                ActionName::ExecutionResume,
                ActionName::ObjectiveComplete,
                ActionName::ObjectiveDiscard,
            ]),
        );
        assert!(next_action(&d).is_none_or(|n| n.tone != Tone::Primary));
    }

    #[test]
    fn agent_controls_are_pointed_to_not_repeated() {
        let base = details().into_iter().next().unwrap();
        let mut d = with(&base, &all(&[ActionName::ExecutionStart]));
        d.summary.lifecycle = Lifecycle::Defined;
        assert!(next_action(&d).is_none());
        assert!(
            agent_pointer(&d, None)
                .unwrap()
                .contains("start it in Activity")
        );
        let blocked = agent_pointer(&d, Some("no model provider has credentials")).unwrap();
        assert!(blocked.contains("cannot start yet: no model provider has credentials."));
        assert!(!blocked.contains("start it in"));
        assert!(!overview_actions(&d).contains(&ActionName::ExecutionStart));
        let controls = activity_controls(&d);
        let start = controls
            .iter()
            .find(|p| p.action == ActionName::ExecutionStart)
            .unwrap();
        assert_eq!(start.tone, Tone::Primary);
        assert!(
            controls
                .iter()
                .filter(|p| p.action != ActionName::ExecutionStart)
                .all(|p| p.tone == Tone::Neutral && p.availability != ActionAvailability::Enabled)
        );
        let mut d = with(&base, &all(&[ActionName::ExecutionResume]));
        d.summary.lifecycle = Lifecycle::Active;
        d.summary.execution.state = ExecutionState::Cancelled;
        assert!(
            agent_pointer(&d, None)
                .unwrap()
                .contains("resume it in Activity")
        );
        let d = with(&base, &all(&[]));
        assert!(agent_pointer(&d, None).is_none());
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
    fn shown_actions_keep_the_services_reason() {
        for d in details() {
            let next = next_action(&d);
            let (regular, destructive) = more_actions(&d, next.as_ref());
            let shown: Vec<&PanelAction> =
                next.iter().chain(&regular).chain(&destructive).collect();
            for p in shown.iter().copied().chain(activity_controls(&d).iter()) {
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
        d.available_actions
            .retain(|a| a.action != ActionName::ReviewApprove);
        d.available_actions.push(AvailableAction {
            action: ActionName::ReviewApprove,
            enabled: true,
            reason: Some("verification missing; an override reason is required".into()),
        });
        let next = next_action(&d).unwrap();
        assert_eq!(next.action, ActionName::ReviewApprove);
        assert_eq!(
            next.note.as_deref(),
            Some("verification missing; an override reason is required")
        );
        assert_eq!(next.availability, ActionAvailability::Enabled);
        assert_eq!(next.tone, Tone::Neutral);
    }

    #[test]
    fn the_list_row_says_only_what_its_group_does_not() {
        let base = details().into_iter().next().unwrap();
        let mut s = base.summary.clone();
        s.badges = vec![Badge::VerificationFailed, Badge::UnsentDrafts];
        let line = objective_row_line(&s).unwrap();
        assert!(line.contains("check failed") && !line.contains('\n'));
        let (_, attention) = format::status_pill(s.lifecycle, s.attention);
        assert!(!line.contains(attention));
        assert!(status_line(&s).1.contains(" · "));
        s.badges.clear();
        s.lifecycle = Lifecycle::Active;
        assert!(objective_row_line(&s).is_none());
        s.lifecycle = Lifecycle::Integrated;
        assert!(objective_row_line(&s).unwrap().contains("Integrated"));
    }

    #[test]
    fn details_hold_only_what_no_other_surface_shows() {
        let d = details().into_iter().find(|d| d.session.is_some()).unwrap();
        let rows = details_rows(&d, d.summary.updated_at);
        let keys: Vec<&str> = rows.iter().map(|r| r.key).collect();
        assert_eq!(keys, ["Lifecycle", "Updated", "Branch", "Started at"]);
        let start = rows.iter().find(|r| r.key == "Started at").unwrap();
        assert_eq!(start.full.as_deref(), Some(d.start_commit.hex.as_str()));
    }
}
