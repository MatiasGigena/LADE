use std::collections::HashMap;

use super::model::{Attention, ObjectiveId, ObjectiveSummary};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttentionRequest {
    pub objective_id: ObjectiveId,
    pub attention: Attention,
    pub title: String,
    pub body: String,
}

pub trait Notifier {
    fn notify(&self, request: &AttentionRequest) -> Result<(), String>;
}

#[derive(Debug, Default)]
pub struct AttentionWatcher {
    seen: HashMap<ObjectiveId, Attention>,
    primed: bool,
}

impl AttentionWatcher {
    pub fn reset(&mut self) {
        self.seen.clear();
        self.primed = false;
    }

    pub fn observe(&mut self, objectives: &[ObjectiveSummary]) -> Vec<AttentionRequest> {
        let mut requests = Vec::new();
        for objective in objectives {
            let previous = self
                .seen
                .insert(objective.objective_id.clone(), objective.attention);
            let entered = previous != Some(objective.attention)
                && matches!(
                    objective.attention,
                    Attention::NeedsAttention | Attention::ReadyForReview
                );
            if self.primed && entered && !objective.is_archived() {
                requests.push(request_for(objective));
            }
        }
        self.primed = true;
        requests
    }
}

fn request_for(objective: &ObjectiveSummary) -> AttentionRequest {
    let (title, body) = match objective.attention {
        Attention::ReadyForReview => (
            "Ready for review".to_owned(),
            format!("“{}” is ready for you to review.", objective.title),
        ),
        _ => {
            let badges = objective.badge_text();
            (
                "Needs your attention".to_owned(),
                if badges.is_empty() {
                    format!("“{}” needs your attention.", objective.title)
                } else {
                    format!("“{}” needs your attention: {badges}.", objective.title)
                },
            )
        }
    };
    AttentionRequest {
        objective_id: objective.objective_id.clone(),
        attention: objective.attention,
        title,
        body,
    }
}

#[derive(Debug, Default)]
pub struct Delivery {
    unavailable_reported: bool,
}

impl Delivery {
    pub fn deliver(
        &mut self,
        requests: &[AttentionRequest],
        window_active: bool,
        notifier: &dyn Notifier,
    ) -> Option<String> {
        if window_active {
            return None;
        }
        let failure = requests
            .iter()
            .find_map(|request| notifier.notify(request).err())?;
        if std::mem::replace(&mut self.unavailable_reported, true) {
            return None;
        }
        Some(failure)
    }
}

pub fn activation_target(target: &str, objectives: &[ObjectiveSummary]) -> Option<ObjectiveId> {
    objectives
        .iter()
        .find(|o| o.objective_id.as_str() == target)
        .map(|o| o.objective_id.clone())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::presentation::state::model::{
        Badge, Execution, ExecutionState, Lifecycle, Timestamp, WorkspaceId,
    };

    fn summary(id: &str, attention: Attention) -> ObjectiveSummary {
        ObjectiveSummary {
            objective_id: ObjectiveId::new(id),
            workspace_id: WorkspaceId::new("w"),
            title: format!("Objective {id}"),
            outcome: String::new(),
            lifecycle: Lifecycle::Active,
            execution: Execution {
                state: ExecutionState::Idle,
                generation: 1,
                reason: None,
            },
            attention,
            badges: vec![Badge::VerificationFailed],
            updated_at: Timestamp(0),
            revision: 1,
        }
    }

    struct Recording {
        sent: RefCell<Vec<AttentionRequest>>,
        fail: Option<&'static str>,
    }

    impl Notifier for Recording {
        fn notify(&self, request: &AttentionRequest) -> Result<(), String> {
            match self.fail {
                Some(why) => Err(why.to_owned()),
                None => {
                    self.sent.borrow_mut().push(request.clone());
                    Ok(())
                }
            }
        }
    }

    #[test]
    fn only_transitions_after_priming_notify() {
        let mut watcher = AttentionWatcher::default();
        assert!(
            watcher
                .observe(&[summary("a", Attention::NeedsAttention)])
                .is_empty(),
            "existing attention at connect is not a new event"
        );
        assert!(
            watcher
                .observe(&[summary("a", Attention::NeedsAttention)])
                .is_empty()
        );
        let requests = watcher.observe(&[
            summary("a", Attention::Working),
            summary("b", Attention::ReadyForReview),
        ]);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].objective_id.as_str(), "b");
        assert!(requests[0].body.contains("ready for you to review"));
        let requests = watcher.observe(&[summary("a", Attention::NeedsAttention)]);
        assert_eq!(requests.len(), 1);
        assert!(requests[0].body.contains("needs your attention"));
        watcher.reset();
        assert!(
            watcher
                .observe(&[summary("c", Attention::NeedsAttention)])
                .is_empty()
        );
    }

    #[test]
    fn an_active_window_is_not_interrupted() {
        let notifier = Recording {
            sent: RefCell::default(),
            fail: None,
        };
        let request = request_for(&summary("a", Attention::NeedsAttention));
        let mut delivery = Delivery::default();
        assert_eq!(
            delivery.deliver(std::slice::from_ref(&request), true, &notifier),
            None
        );
        assert!(notifier.sent.borrow().is_empty());
        assert_eq!(delivery.deliver(&[request], false, &notifier), None);
        assert_eq!(notifier.sent.borrow().len(), 1);
    }

    #[test]
    fn notification_failure_keeps_in_app_attention_and_reports_once() {
        use crate::presentation::state::model::ConnectionState;
        use crate::presentation::state::store::{PresentationState, StoreUpdate};
        let mut state = PresentationState::new(ConnectionState::Connected);
        state.apply(StoreUpdate::ObjectiveChanged(summary(
            "a",
            Attention::NeedsAttention,
        )));
        let before = format!("{:?}", state.board());
        let failing = Recording {
            sent: RefCell::default(),
            fail: Some("no notification service on the session bus"),
        };
        let mut delivery = Delivery::default();
        let request = request_for(&summary("a", Attention::NeedsAttention));
        let reason = delivery
            .deliver(std::slice::from_ref(&request), false, &failing)
            .expect("fallback reason");
        assert!(reason.contains("no notification service"));
        let notices = state.notices.len();
        state.apply(StoreUpdate::NotificationsUnavailable { reason });
        assert_eq!(
            format!("{:?}", state.board()),
            before,
            "attention unchanged"
        );
        assert_eq!(
            state.notices.len(),
            notices,
            "no banner interrupts the window"
        );
        assert!(
            state
                .notifications_unavailable
                .as_deref()
                .is_some_and(|r| r.contains("no notification service"))
        );
        assert_eq!(delivery.deliver(&[request], false, &failing), None, "once");
    }

    #[test]
    fn activation_accepts_known_objectives_only() {
        let objectives = [summary("a", Attention::NeedsAttention)];
        assert_eq!(
            activation_target("a", &objectives),
            Some(ObjectiveId::new("a"))
        );
        assert_eq!(activation_target("zzz", &objectives), None);
    }
}
