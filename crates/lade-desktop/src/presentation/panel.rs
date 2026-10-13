//! Where each action and fact lives, decided without GTK: every action of the service's
//! `available_actions` has exactly **one home surface** ([`home`]); the right panel's Overview shows
//! its one **next action** for the review/integration step, groups the other Overview actions
//! (More actions, destructive ones last) and keeps the collapsed Details; other surfaces get a
//! short pointer instead of a second control.
//!
//! Authority stays with the service: every action shown carries exactly the availability of
//! `available_actions` (`ObjectiveDetail::availability`). The client only chooses where an action
//! is drawn and which offered action to emphasise; it never enables, hides a disabled reason, or
//! invents an action.

use super::compat::{Catalog, ProviderStatus};
use super::format;
use super::model::{
    ActionAvailability, ActionName, Lifecycle, ObjectiveDetail, ObjectiveSummary, Timestamp,
};

/// Accessible name and visible label of the collapsed group of the other actions.
pub const MORE_ACTIONS_TITLE: &str = "More actions";
/// Accessible name and visible label of the collapsed metadata.
pub const DETAILS_TITLE: &str = "Details";

/// The surface that owns an action (its only control).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// Right panel, Overview tab: review, integration and Objective decisions; the intent.
    Overview,
    /// Right panel, Evidence tab: service-observed checks.
    Evidence,
    /// Right panel, Feedback tab: Contextual Feedback batches.
    Feedback,
    /// Activity: Agent Session control (start, cancel, resume) and free-form messages.
    Activity,
}

/// Every action the service projects.
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

/// The one surface that draws `action`.
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

/// Agent Session controls in Activity's header, in order (Send is the composer's).
pub const ACTIVITY_CONTROLS: [ActionName; 3] = [
    ActionName::ExecutionStart,
    ActionName::ExecutionCancel,
    ActionName::ExecutionResume,
];

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

/// Overview lifecycle actions in the order "More actions" lists them.
const MORE_ORDER: [ActionName; 6] = [
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

/// Review steps that move the collaboration forward, most advanced first: the first one the
/// service enables is the next action. Starting and resuming the agent are Activity's.
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

/// The review step the lifecycle is at, shown (disabled, with the service's reason) when the
/// service enables no step forward. A defined Objective's step is starting the agent (Activity).
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

/// The Overview's one next action: the most advanced review step the service enables; for a
/// closed Objective, an enabled Clean up (destructive treatment, confirmed in a dialog);
/// otherwise the lifecycle's own step with the service's reason. `None` when nothing applies.
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

/// Every other Overview action the service offers, for "More actions": lifecycle actions in a
/// stable order, then the destructive ones. The next action is not repeated; not-offered actions
/// are left out; Delete Git history disappears once the history is deleted (the panel says so).
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

/// Every action control the Overview draws: the next action, More actions and Edit intent.
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

/// Activity's header controls the service offers (Start, Cancel, Resume).
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

/// When an agent control (Start, Resume) is what the Objective waits for, the Overview names it
/// in one line and points to Activity instead of drawing a second control. `start_blocker` is
/// why the agent cannot start even though the service offers Start (for example no model
/// provider has credentials): the line then says so instead of suggesting a start.
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

/// Why the agent cannot start whatever the service offers, from the model catalogue: no provider
/// has credentials (the catalogue's own error, such as an unavailable sandbox, when it gives
/// one). `None` while the catalogue is not loaded or when a provider can be used.
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

/// Status, sign and the attention reasons ("! Needs attention · check failed · unsent drafts").
/// Its home is the Objectives list; the compact work area repeats it only because the list is
/// then another region.
pub fn status_line(summary: &ObjectiveSummary) -> (&'static str, String) {
    let (sign, text) = format::status_pill(summary.lifecycle, summary.attention);
    if summary.badges.is_empty() {
        (sign, text.to_owned())
    } else {
        (sign, format!("{text} · {}", summary.badge_text()))
    }
}

/// The Objectives row's secondary line: what the group heading does not already say — the
/// attention reasons with the sign, or the closed lifecycle in the archive. `None` otherwise.
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
    /// Full value (tooltip and accessible description) when `value` is shortened.
    pub full: Option<String>,
    pub mono: bool,
}

/// The collapsed Details: what no other surface shows — lifecycle, last update, branch and start
/// commit. (Agent Session identity is Activity's; changes and the comparison target are the
/// work area's.)
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
    use crate::presentation::model::{AvailableAction, Badge, ExecutionState};
    use crate::presentation::store::PresentationState;
    use crate::presentation::test_fixture;

    fn details() -> Vec<ObjectiveDetail> {
        let mut state =
            PresentationState::new(crate::presentation::model::ConnectionState::Connected);
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

    /// Every action, offered by the service with its own projection (one entry each, as Rust
    /// projects them).
    fn all(enabled: &[ActionName]) -> Vec<(ActionName, bool, Option<&'static str>)> {
        ALL_ACTIONS
            .iter()
            .map(|a| {
                let on = enabled.contains(a);
                (*a, on, (!on).then_some("blocked by the service"))
            })
            .collect()
    }

    /// The action controls each surface draws for `d` (the Evidence and Feedback tabs draw their
    /// one action whenever the service offers it).
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
                // Never more available than the service says, and the same reason.
                assert_eq!(next.availability, base.availability(next.action));
                assert_eq!(home(next.action), Surface::Overview);
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
        // Return, Cancel, Start, Resume and Complete are never suggested as the Overview's step.
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
        // Offered, but it cannot start (no provider): the line does not suggest starting.
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
        // Enabled, but with a caveat: the next step, never the primary treatment.
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
        // The attention word is the group heading's, not repeated in the row.
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
