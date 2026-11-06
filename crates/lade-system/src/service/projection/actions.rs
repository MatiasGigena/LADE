//! Which actions an Objective offers, and the reason each disabled one gives. Request handlers
//! reuse the same blockers and messages, so a refusal reads like the disabled action's reason.

use lade_core::domain::execution::ExecutionState;
use lade_core::domain::ids::GitId;
use lade_core::domain::objectives::{Lifecycle, ObjectiveRecord};
use lade_core::domain::review::{
    Candidate, CandidateStatus, Evidence, Review, VerificationOutcome,
};
use lade_protocol::client::{ActionName, AvailableAction};

use crate::service::Service;
use crate::service::state::{Busy, ObjState};

pub(crate) const NO_CHANGES_TO_FREEZE: &str =
    "There are no changes to freeze. Complete without changes instead.";
pub(crate) const VERIFICATION_SANDBOX_UNAVAILABLE: &str =
    "Checks can't run because the sandbox check failed on this computer.";
pub(crate) const TERMINAL_SANDBOX_UNAVAILABLE: &str =
    "Terminals can't open because the sandbox check failed on this computer.";
pub(crate) const NO_WORKTREE_YET: &str =
    "Start the agent once to create this Objective's worktree.";
pub(crate) const BUSY: &str = "Another operation is in progress. Wait for it to finish.";
const CLOSED: &str = "This Objective is closed.";
const STOP_AGENT_FIRST: &str = "Stop the agent first.";
const CHECK_RUNNING: &str = "Wait for the running check to finish.";
const NO_CURRENT_CANDIDATE: &str = "There is no current candidate.";

fn input_blocker(
    lifecycle: Lifecycle,
    has_session: bool,
    not_started: &'static str,
) -> Option<&'static str> {
    match lifecycle {
        Lifecycle::Defined => Some(not_started),
        Lifecycle::Active if !has_session => Some(not_started),
        Lifecycle::Active => None,
        Lifecycle::Frozen | Lifecycle::Approved => {
            Some("Return the candidate for revision before sending more input.")
        }
        _ => Some(CLOSED),
    }
}

pub(crate) fn terminal_blocker(st: &ObjState) -> Option<&'static str> {
    let record = st.row.objective.record();
    if record.cleaned_up {
        Some("This Objective was cleaned up, so it has no worktree for a terminal.")
    } else if record.lifecycle.is_terminal() {
        Some("Terminals open only while the Objective is in progress or under review.")
    } else if !st.row.worktree.as_ref().is_some_and(|w| w.is_dir()) {
        Some(NO_WORKTREE_YET)
    } else {
        None
    }
}

fn enabled(action: ActionName) -> AvailableAction {
    AvailableAction {
        action,
        enabled: true,
        reason: None,
    }
}

fn disabled(action: ActionName, reason: &str) -> AvailableAction {
    AvailableAction {
        action,
        enabled: false,
        reason: Some(reason.to_string()),
    }
}

fn gate(action: ActionName, blocker: Option<&str>) -> AvailableAction {
    blocker.map_or_else(|| enabled(action), |reason| disabled(action, reason))
}

impl Service {
    pub(super) fn actions(&self, st: &ObjState, target: Option<&GitId>) -> Vec<AvailableAction> {
        let facts = ActionFacts::new(self, st, target);
        let mut out = Vec::with_capacity(17);
        facts.execution_actions(&mut out);
        facts.review_actions(&mut out);
        facts.closing_actions(&mut out);
        out
    }
}

/// What the availability of every action depends on, gathered once per Objective summary.
struct ActionFacts<'a> {
    service: &'a Service,
    st: &'a ObjState,
    record: &'a ObjectiveRecord,
    target: Option<&'a GitId>,
    lifecycle: Lifecycle,
    state: &'a ExecutionState,
    /// `BUSY` while any operation runs on the Objective.
    busy_reason: Option<&'static str>,
    /// The running operation is a freeze, which already leaves the worktree read-only.
    freezing: bool,
    review: &'a Review,
    current: Option<&'a Candidate>,
    running_verification: bool,
    target_moved: bool,
    worker_blocker: Option<String>,
    storage_blocker: Option<String>,
}

impl<'a> ActionFacts<'a> {
    fn new(service: &'a Service, st: &'a ObjState, target: Option<&'a GitId>) -> Self {
        let oid = st.row.objective.id();
        let record = st.row.objective.record();
        let state = record.execution.state();
        let busy = service.runtime.get(&oid).and_then(|r| r.busy);
        let review = &st.review;
        let current = review.current_candidate();
        Self {
            service,
            st,
            record,
            target,
            lifecycle: record.lifecycle,
            state,
            busy_reason: busy.map(|_| BUSY),
            freezing: busy == Some(Busy::Freeze),
            review,
            current,
            running_verification: review
                .verifications()
                .iter()
                .any(|v| v.outcome == VerificationOutcome::Running),
            target_moved: match (current, target) {
                (Some(c), Some(t)) => c.base_target_commit != *t,
                _ => false,
            },
            worker_blocker: service.worker_blocker(oid),
            storage_blocker: service.storage_blocker(),
        }
    }

    /// Intent editing, the agent's execution and input to it.
    fn execution_actions(&self, out: &mut Vec<AvailableAction>) {
        out.push(gate(
            ActionName::ObjectiveUpdateIntent,
            self.busy_reason.or(match self.lifecycle {
                Lifecycle::Defined | Lifecycle::Active | Lifecycle::Frozen => None,
                Lifecycle::Approved => Some(
                    "The intent can't change after approval. Return the candidate for revision first.",
                ),
                _ => Some(CLOSED),
            }),
        ));
        out.push(gate(
            ActionName::ExecutionStart,
            self.busy_reason.or(match self.lifecycle {
                Lifecycle::Defined | Lifecycle::Active if *self.state == ExecutionState::None => {
                    self.worker_blocker.as_deref()
                }
                Lifecycle::Defined | Lifecycle::Active if self.state.is_live() => {
                    Some("The agent is already running.")
                }
                Lifecycle::Defined | Lifecycle::Active => {
                    Some("The agent has already started. Use Resume instead.")
                }
                Lifecycle::Frozen | Lifecycle::Approved => {
                    Some("Return the candidate for revision before starting the agent.")
                }
                _ => Some(CLOSED),
            }),
        ));
        out.push(gate(
            ActionName::ExecutionCancel,
            match self.state {
                ExecutionState::Starting | ExecutionState::Running | ExecutionState::Idle => None,
                ExecutionState::Stopping => Some("The agent is already stopping."),
                _ => Some("The agent isn't running."),
            },
        ));
        out.push(gate(
            ActionName::ExecutionResume,
            self.busy_reason.or(match self.lifecycle {
                Lifecycle::Active
                    if matches!(
                        self.state,
                        ExecutionState::Cancelled { .. } | ExecutionState::Failed { .. }
                    ) =>
                {
                    self.worker_blocker.as_deref()
                }
                Lifecycle::Active if self.state.is_live() => Some("The agent is already running."),
                Lifecycle::Defined | Lifecycle::Active => Some("Start the agent first."),
                Lifecycle::Frozen | Lifecycle::Approved => {
                    Some("Return the candidate for revision before resuming the agent.")
                }
                _ => Some(CLOSED),
            }),
        ));
        let has_session = self.record.execution.session().is_some();
        let input_note =
            (!self.state.is_live() && self.lifecycle == Lifecycle::Active && has_session)
                .then_some(
                    "The agent is stopped. Your input is kept and delivered after you resume it.",
                );
        let input = |action: ActionName, blocker: Option<&str>| match (blocker, input_note) {
            (None, Some(note)) => AvailableAction {
                action,
                enabled: true,
                reason: Some(note.into()),
            },
            (blocker, _) => gate(action, blocker),
        };
        out.push(input(
            ActionName::MessageSend,
            input_blocker(
                self.lifecycle,
                has_session,
                "Start the agent before sending messages.",
            ),
        ));
        out.push(input(
            ActionName::FeedbackSubmit,
            input_blocker(
                self.lifecycle,
                has_session,
                "Start the agent before submitting feedback.",
            )
            .or(self
                .st
                .row
                .worktree
                .is_none()
                .then_some("There is no agent work to give feedback on yet."))
            .or((self.st.unsent_drafts() == 0).then_some("There are no saved drafts to submit.")),
        ));
    }

    /// Freezing, checking, approving, refreshing and integrating a candidate.
    fn review_actions(&self, out: &mut Vec<AvailableAction>) {
        let stopping = (*self.state == ExecutionState::Stopping)
            .then_some("Wait until the agent has stopped.");
        out.push(gate(
            ActionName::ReviewFreeze,
            self.busy_reason.or(freeze_blocker(self.st)).or(stopping),
        ));
        let latest_returnable = self
            .review
            .latest_candidate()
            .is_some_and(|c| c.status != CandidateStatus::Historical);
        out.push(gate(
            ActionName::ReviewReturn,
            self.busy_reason.or(
                if !matches!(self.lifecycle, Lifecycle::Frozen | Lifecycle::Approved) {
                    Some("There is no frozen candidate to return.")
                } else if !latest_returnable {
                    Some(NO_CURRENT_CANDIDATE)
                } else {
                    None
                },
            ),
        ));
        out.push(gate(
            ActionName::ReviewVerify,
            self.busy_reason.or(match self.lifecycle {
                Lifecycle::Frozen if self.current.is_none() => Some(NO_CURRENT_CANDIDATE),
                Lifecycle::Frozen => match self.storage_blocker.as_deref() {
                    Some(why) => Some(why),
                    None if self.service.sandbox_tools().is_none() => {
                        Some(VERIFICATION_SANDBOX_UNAVAILABLE)
                    }
                    None => None,
                },
                Lifecycle::Approved => Some(
                    "The candidate is already approved. Return it for revision to run more checks.",
                ),
                Lifecycle::Defined | Lifecycle::Active => {
                    Some("Freeze the work for review before running checks.")
                }
                _ => Some(CLOSED),
            }),
        ));
        out.push(self.approve_action());
        out.push(gate(
            ActionName::ReviewRefresh,
            self.busy_reason.or(
                if !matches!(self.lifecycle, Lifecycle::Frozen | Lifecycle::Approved) {
                    Some("Only a frozen candidate can be refreshed.")
                } else if self.current.is_none() {
                    Some(NO_CURRENT_CANDIDATE)
                } else if !self.target_moved {
                    Some("The Integration Target hasn't moved, so there is nothing to refresh.")
                } else {
                    None
                },
            ),
        ));
        out.push(gate(
            ActionName::ReviewIntegrate,
            self.busy_reason.or(if self.lifecycle != Lifecycle::Approved {
                Some("Approve the candidate before integrating.")
            } else if self.review.valid_approval().is_none() {
                Some("The approval is no longer valid. Approve again.")
            } else if self.target_moved
                || self.review
                    .valid_approval()
                    .zip(self.target)
                    .is_some_and(|(a, t)| a.target_commit != *t)
            {
                Some("The Integration Target moved after approval. Refresh the candidate and approve again.")
            } else {
                None
            }),
        ));
    }

    /// Approving the current candidate, which a missing or failed check only qualifies.
    fn approve_action(&self) -> AvailableAction {
        match (self.lifecycle, self.current) {
            (Lifecycle::Frozen, Some(c)) if self.busy_reason.is_none() => {
                if self.target_moved {
                    disabled(
                        ActionName::ReviewApprove,
                        "The Integration Target moved. Refresh the candidate first.",
                    )
                } else {
                    match self.review.evidence(c.id) {
                        Evidence::Passed => enabled(ActionName::ReviewApprove),
                        Evidence::Running => disabled(ActionName::ReviewApprove, CHECK_RUNNING),
                        Evidence::Missing => AvailableAction {
                            action: ActionName::ReviewApprove,
                            enabled: true,
                            reason: Some(
                                "No checks have run. Give a reason to approve anyway.".into(),
                            ),
                        },
                        Evidence::Failed => AvailableAction {
                            action: ActionName::ReviewApprove,
                            enabled: true,
                            reason: Some("A check failed. Give a reason to approve anyway.".into()),
                        },
                    }
                }
            }
            (Lifecycle::Frozen, Some(_)) => disabled(ActionName::ReviewApprove, BUSY),
            (Lifecycle::Frozen, None) => disabled(ActionName::ReviewApprove, NO_CURRENT_CANDIDATE),
            (Lifecycle::Approved, _) => disabled(
                ActionName::ReviewApprove,
                "Already approved. Integrate it, or return it for revision.",
            ),
            (Lifecycle::Defined | Lifecycle::Active, _) => disabled(
                ActionName::ReviewApprove,
                "Freeze the work for review before approving.",
            ),
            _ => disabled(ActionName::ReviewApprove, CLOSED),
        }
    }

    /// Ending the Objective, cleaning up after it, and opening a terminal.
    fn closing_actions(&self, out: &mut Vec<AvailableAction>) {
        let has_changes = self.lifecycle == Lifecycle::Frozen
            || self.st.capture.as_ref().is_some_and(|c| c.counts.files > 0);
        out.push(gate(
            ActionName::ObjectiveComplete,
            self.busy_reason.or(match self.lifecycle {
                Lifecycle::Defined | Lifecycle::Active | Lifecycle::Frozen => {
                    if self.state.is_live() {
                        Some(STOP_AGENT_FIRST)
                    } else if self.running_verification {
                        Some(CHECK_RUNNING)
                    } else if has_changes {
                        Some("There are changes. Freeze them for review, or discard the Objective.")
                    } else {
                        None
                    }
                }
                Lifecycle::Approved => {
                    Some("The candidate is approved. Integrate it, or discard the Objective.")
                }
                _ => Some(CLOSED),
            }),
        ));
        out.push(gate(
            ActionName::ObjectiveDiscard,
            self.busy_reason.or(if self.lifecycle.is_terminal() {
                Some(CLOSED)
            } else if self.state.is_live() {
                Some(STOP_AGENT_FIRST)
            } else if self.running_verification {
                Some(CHECK_RUNNING)
            } else {
                None
            }),
        ));
        out.push(gate(
            ActionName::ObjectiveCleanup,
            self.busy_reason.or(if !self.lifecycle.is_terminal() {
                Some("Clean up becomes available once the Objective is integrated, completed or discarded.")
            } else if self.record.cleaned_up {
                Some("This Objective is already cleaned up.")
            } else if self.state.is_live() {
                Some(STOP_AGENT_FIRST)
            } else {
                None
            }),
        ));
        out.push(gate(
            ActionName::ObjectiveDeleteHistory,
            self.busy_reason.or(delete_history_blocker(self.st)),
        ));
        out.push(match terminal_blocker(self.st) {
            Some(why) => disabled(ActionName::TerminalOpen, why),
            None if self.service.sandbox_tools().is_none() => {
                disabled(ActionName::TerminalOpen, TERMINAL_SANDBOX_UNAVAILABLE)
            }
            None if matches!(self.lifecycle, Lifecycle::Frozen | Lifecycle::Approved)
                || self.freezing =>
            {
                AvailableAction {
                    action: ActionName::TerminalOpen,
                    enabled: true,
                    reason: Some("Opens read-only while the work is frozen for review.".into()),
                }
            }
            None => enabled(ActionName::TerminalOpen),
        });
    }
}

/// Why `review.freeze` is refused for this Objective, if it is. The action list and the
/// request handler both use it, so the disabled reason is the rejection detail.
pub(crate) fn freeze_blocker(st: &ObjState) -> Option<&'static str> {
    match st.row.objective.lifecycle() {
        Lifecycle::Active if st.row.worktree.is_none() => {
            Some("There is no agent work to freeze yet.")
        }
        Lifecycle::Active if st.capture.as_ref().is_some_and(|c| c.counts.files == 0) => {
            Some(NO_CHANGES_TO_FREEZE)
        }
        Lifecycle::Active => None,
        Lifecycle::Defined => Some("Start the agent first; there is nothing to freeze yet."),
        Lifecycle::Frozen | Lifecycle::Approved => Some("The work is already frozen for review."),
        _ => Some(CLOSED),
    }
}

pub(crate) fn delete_history_blocker(st: &ObjState) -> Option<&'static str> {
    let record = st.row.objective.record();
    if !record.cleaned_up {
        Some("Clean up the Objective before deleting its Git history.")
    } else if st.row.history_deleted {
        Some("The Git history was already deleted.")
    } else {
        None
    }
}
