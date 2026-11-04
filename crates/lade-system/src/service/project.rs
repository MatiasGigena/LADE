use lade_core::domain::execution::ExecutionState;
use lade_core::domain::feedback::DeliveryState;
use lade_core::domain::ids::{GitId, WorkspaceId};
use lade_core::domain::objectives::{
    AttentionFacts, Badge as CoreBadge, Lifecycle, derive_attention,
};
use lade_core::domain::review::{CandidateStatus, Evidence, VerificationOutcome};
use lade_protocol::client::{
    self as wire, ActionName, AvailableAction, Badge, ChangeCounts, Event, ObjectiveDetail,
    ObjectiveSummary, SessionInfo, TargetRef, Workspace,
};
use lade_protocol::types::{Counter, Model};

use super::Service;
use super::convert::{
    preview, wire_approval, wire_candidate, wire_execution_state, wire_git, wire_id,
    wire_lifecycle, wire_path, wire_verification,
};
use super::state::{ObjState, WsState};
use crate::support::wire_ts;

const SUMMARY_OUTCOME_CHARS: usize = 512;
pub(super) const DETAIL_LIST_MAX: usize = 64;
// At most one of failed/missing verification evidence applies. The remaining
// compatible badges total seven, including unsent drafts and execution state.
pub(super) const MAX_SUMMARY_BADGES: usize = 7;
pub const NO_CHANGES_TO_FREEZE: &str =
    "There are no changes to freeze. Complete without changes instead.";
pub const VERIFICATION_SANDBOX_UNAVAILABLE: &str =
    "Checks can't run because the sandbox check failed on this computer.";
pub const TERMINAL_SANDBOX_UNAVAILABLE: &str =
    "Terminals can't open because the sandbox check failed on this computer.";
pub const NO_WORKTREE_YET: &str = "Start the agent once to create this Objective's worktree.";
pub const BUSY: &str = "Another operation is in progress. Wait for it to finish.";
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
    let record = st.objective().record();
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

fn wire_badge(badge: CoreBadge) -> Option<Badge> {
    Some(match badge {
        CoreBadge::Interrupted => Badge::Interrupted,
        CoreBadge::DeliveryUncertain => Badge::DeliveryUncertain,
        CoreBadge::StaleFeedback => Badge::StaleFeedback,
        CoreBadge::VerificationFailed => Badge::VerificationFailed,
        CoreBadge::IntegrationConflict => Badge::IntegrationConflict,
        CoreBadge::StaleTarget => Badge::StaleTarget,
        CoreBadge::CleanedUp => Badge::CleanedUp,
        CoreBadge::Failed | CoreBadge::Cancelled => return None,
    })
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
    pub(crate) fn resolve_target(&mut self, workspace: WorkspaceId, branch: &str) -> Option<GitId> {
        let ws = self.workspaces.get(&workspace)?;
        let target = super::jobs::target_commit(&ws.repo, branch).ok();
        let key = (workspace, branch.to_string());
        match &target {
            Some(id) => {
                self.integrations.targets.insert(key, id.clone());
            }
            None => {
                self.integrations.targets.remove(&key);
            }
        }
        target
    }

    pub(crate) fn cached_target(&self, st: &ObjState) -> Option<GitId> {
        let record = st.objective().record();
        self.integrations
            .targets
            .get(&(
                record.workspace_id,
                record.integration_target.as_str().to_string(),
            ))
            .cloned()
    }

    pub(crate) fn fresh_target(&mut self, st: &ObjState) -> Option<GitId> {
        let record = st.objective().record();
        let branch = record.integration_target.as_str().to_string();
        self.resolve_target(record.workspace_id, &branch)
    }

    fn facts(&self, st: &ObjState, target: Option<&GitId>) -> AttentionFacts {
        AttentionFacts {
            delivery_uncertain: st
                .inputs
                .iter()
                .any(|(_, i)| matches!(i.delivery, DeliveryState::Uncertain { .. })),
            stale_feedback: st
                .drafts
                .iter()
                .any(|d| d.is_unsent() && st.draft_stale.get(&d.id()).copied().unwrap_or(false)),
            verification_failed: st.review.verification_failed(),
            integration_conflict: st.row.integration_conflict,
            target_moved: target.is_some_and(|t| st.review.target_stale(t)),
            change_set_non_empty: st.capture.as_ref().is_some_and(|c| c.counts.files > 0),
        }
    }

    pub(crate) fn summary_with(&self, st: &ObjState, target: Option<&GitId>) -> ObjectiveSummary {
        let objective = st.objective();
        let record = objective.record();
        let view = derive_attention(objective, &self.facts(st, target));
        let mut badges: Vec<Badge> = view.badges.into_iter().filter_map(wire_badge).collect();
        if matches!(
            record.execution.state(),
            lade_core::domain::execution::ExecutionState::Failed { reason }
                if reason.starts_with(super::execution::LIMIT_REACHED)
        ) {
            badges.insert(0, Badge::LimitReached);
        }
        if record.lifecycle == Lifecycle::Frozen
            && st
                .review
                .current_candidate()
                .is_some_and(|c| st.review.evidence(c.id) == Evidence::Missing)
        {
            badges.push(Badge::VerificationMissing);
        }
        if st.unsent_drafts() > 0 && !record.lifecycle.is_terminal() {
            badges.push(Badge::UnsentDrafts);
        }
        badges.truncate(16);
        let (state, reason) = wire_execution_state(record.execution.state());
        ObjectiveSummary {
            objective_id: wire_id(record.id.uuid()),
            workspace_id: wire_id(record.workspace_id.uuid()),
            title: record.title.as_str().to_string(),
            outcome: preview(record.intent.outcome(), SUMMARY_OUTCOME_CHARS),
            lifecycle: wire_lifecycle(record.lifecycle),
            execution: wire::Execution {
                state,
                generation: Counter::new(record.execution.generation().value()),
                reason,
            },
            attention: match view.attention {
                lade_core::domain::objectives::Attention::NeedsAttention => {
                    wire::Attention::NeedsAttention
                }
                lade_core::domain::objectives::Attention::ReadyForReview => {
                    wire::Attention::ReadyForReview
                }
                lade_core::domain::objectives::Attention::Working => wire::Attention::Working,
                lade_core::domain::objectives::Attention::Idle => wire::Attention::Idle,
                lade_core::domain::objectives::Attention::Archived => wire::Attention::Archived,
            },
            badges,
            updated_at: wire_ts(record.updated_at.unix_ms()),
            revision: Counter::new(record.revision.value()),
        }
    }

    pub(crate) fn summary(&self, st: &ObjState) -> ObjectiveSummary {
        self.summary_with(st, self.cached_target(st).as_ref())
    }

    pub(crate) fn detail_of(&mut self, st: &ObjState) -> ObjectiveDetail {
        let target = self.fresh_target(st);
        self.detail_with(st, target.as_ref())
    }

    pub(crate) fn detail_with(&self, st: &ObjState, target: Option<&GitId>) -> ObjectiveDetail {
        let summary = self.summary_with(st, target);
        let record = st.objective().record();
        let review = &st.review;
        let session = record.execution.session().map(|s| SessionInfo {
            session_id: wire_id(s.session_id.uuid()),
            association_revision: Counter::new(s.association_revision.value()),
            execution_generation: Counter::new(record.execution.generation().value()),
            model: Model {
                provider: preview(s.model.provider(), 128),
                id: preview(s.model.id(), 256),
            },
            thinking: self.thinking_of(st.objective().id()),
        });
        let skip = |len: usize| len.saturating_sub(DETAIL_LIST_MAX);
        let candidates = review
            .candidates()
            .iter()
            .skip(skip(review.candidates().len()))
            .map(wire_candidate)
            .collect();
        let verifications = review
            .verifications()
            .iter()
            .skip(skip(review.verifications().len()))
            .map(|v| wire_verification(st, v))
            .collect();
        let approval = review
            .valid_approval()
            .or_else(|| review.approvals().last())
            .map(wire_approval);
        let counts = match (record.lifecycle, review.current_candidate()) {
            (Lifecycle::Frozen | Lifecycle::Approved, Some(c)) => c.changes,
            _ => st.capture.as_ref().map(|c| c.counts).unwrap_or_default(),
        };
        let target_ref = TargetRef {
            branch: record.integration_target.as_str().to_string(),
            commit: wire_git(target.unwrap_or(&record.start_commit)),
        };
        self.budget_detail(ObjectiveDetail {
            objective_id: summary.objective_id,
            workspace_id: summary.workspace_id,
            title: summary.title,
            outcome: summary.outcome,
            lifecycle: summary.lifecycle,
            execution: summary.execution,
            attention: summary.attention,
            badges: summary.badges,
            updated_at: summary.updated_at,
            revision: summary.revision,
            intent: wire::Intent {
                outcome: record.intent.outcome().to_string(),
                constraints: record.intent.constraints().to_string(),
                acceptance: record.intent.acceptance().to_string(),
            },
            intent_revision: Counter::new(record.intent_revision.value()),
            integration_target: target_ref,
            start_commit: wire_git(&record.start_commit),
            branch: st
                .row
                .git_branch
                .clone()
                .unwrap_or_else(|| record.branch.as_str().to_string()),
            candidates,
            verifications,
            change_counts: ChangeCounts {
                files: Counter::new(u64::from(counts.files)),
                added: Counter::new(counts.added),
                removed: Counter::new(counts.removed),
            },
            unsent_drafts: Counter::new(st.unsent_drafts() as u64),
            available_actions: self.actions(st, target),
            session,
            approval,
            history_deleted: st.row.history_deleted.then_some(true),
        })
    }

    fn actions(&self, st: &ObjState, target: Option<&GitId>) -> Vec<AvailableAction> {
        let oid = st.objective().id();
        let record = st.objective().record();
        let lifecycle = record.lifecycle;
        let state = record.execution.state();
        let live = state.is_live();
        let busy = self.runtime.get(&oid).and_then(|r| r.busy);
        let review = &st.review;
        let current = review.current_candidate();
        let running_verification = review
            .verifications()
            .iter()
            .any(|v| v.outcome == VerificationOutcome::Running);
        let busy_reason = busy.map(|_| BUSY);
        let terminal = lifecycle.is_terminal();
        let target_moved = match (current, target) {
            (Some(c), Some(t)) => c.base_target_commit != *t,
            _ => false,
        };
        let worker_blocker = self.worker_blocker(oid);
        let storage_blocker = self.storage_blocker();
        let mut out = Vec::with_capacity(17);
        out.push(gate(
            ActionName::ObjectiveUpdateIntent,
            busy_reason.or(match lifecycle {
                Lifecycle::Defined | Lifecycle::Active | Lifecycle::Frozen => None,
                Lifecycle::Approved => Some(
                    "The intent can't change after approval. Return the candidate for revision first.",
                ),
                _ => Some(CLOSED),
            }),
        ));
        out.push(gate(
            ActionName::ExecutionStart,
            busy_reason.or(match lifecycle {
                Lifecycle::Defined | Lifecycle::Active if *state == ExecutionState::None => {
                    worker_blocker.as_deref()
                }
                Lifecycle::Defined | Lifecycle::Active if live => {
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
            match state {
                ExecutionState::Starting | ExecutionState::Running | ExecutionState::Idle => None,
                ExecutionState::Stopping => Some("The agent is already stopping."),
                _ => Some("The agent isn't running."),
            },
        ));
        out.push(gate(
            ActionName::ExecutionResume,
            busy_reason.or(match lifecycle {
                Lifecycle::Active
                    if matches!(
                        state,
                        ExecutionState::Cancelled { .. } | ExecutionState::Failed { .. }
                    ) =>
                {
                    worker_blocker.as_deref()
                }
                Lifecycle::Active if live => Some("The agent is already running."),
                Lifecycle::Defined | Lifecycle::Active => Some("Start the agent first."),
                Lifecycle::Frozen | Lifecycle::Approved => {
                    Some("Return the candidate for revision before resuming the agent.")
                }
                _ => Some(CLOSED),
            }),
        ));
        let has_session = record.execution.session().is_some();
        let input_note = (!live && lifecycle == Lifecycle::Active && has_session).then_some(
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
                lifecycle,
                has_session,
                "Start the agent before sending messages.",
            ),
        ));
        out.push(input(
            ActionName::FeedbackSubmit,
            input_blocker(
                lifecycle,
                has_session,
                "Start the agent before submitting feedback.",
            )
            .or(st
                .row
                .worktree
                .is_none()
                .then_some("There is no agent work to give feedback on yet."))
            .or((st.unsent_drafts() == 0).then_some("There are no saved drafts to submit.")),
        ));
        out.push(gate(
            ActionName::ReviewFreeze,
            busy_reason.or(match lifecycle {
                Lifecycle::Active if st.row.worktree.is_none() => {
                    Some("There is no agent work to freeze yet.")
                }
                Lifecycle::Active if *state == ExecutionState::Stopping => {
                    Some("Wait until the agent has stopped.")
                }
                Lifecycle::Active if st.capture.as_ref().is_some_and(|c| c.counts.files == 0) => {
                    Some(NO_CHANGES_TO_FREEZE)
                }
                Lifecycle::Active => None,
                Lifecycle::Defined => {
                    Some("Start the agent first; there is nothing to freeze yet.")
                }
                Lifecycle::Frozen | Lifecycle::Approved => {
                    Some("The work is already frozen for review.")
                }
                _ => Some(CLOSED),
            }),
        ));
        let latest_returnable = review
            .latest_candidate()
            .is_some_and(|c| c.status != CandidateStatus::Historical);
        out.push(gate(
            ActionName::ReviewReturn,
            busy_reason.or(
                if !matches!(lifecycle, Lifecycle::Frozen | Lifecycle::Approved) {
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
            busy_reason.or(match lifecycle {
                Lifecycle::Frozen if current.is_none() => Some(NO_CURRENT_CANDIDATE),
                Lifecycle::Frozen => match storage_blocker.as_deref() {
                    Some(why) => Some(why),
                    None if self.sandbox_tools().is_none() => {
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
        let approve = match (lifecycle, current) {
            (Lifecycle::Frozen, Some(c)) if busy.is_none() => {
                if target_moved {
                    disabled(
                        ActionName::ReviewApprove,
                        "The Integration Target moved. Refresh the candidate first.",
                    )
                } else {
                    match review.evidence(c.id) {
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
        };
        out.push(approve);
        out.push(gate(
            ActionName::ReviewRefresh,
            busy_reason.or(
                if !matches!(lifecycle, Lifecycle::Frozen | Lifecycle::Approved) {
                    Some("Only a frozen candidate can be refreshed.")
                } else if current.is_none() {
                    Some(NO_CURRENT_CANDIDATE)
                } else if !target_moved {
                    Some("The Integration Target hasn't moved, so there is nothing to refresh.")
                } else {
                    None
                },
            ),
        ));
        out.push(gate(
            ActionName::ReviewIntegrate,
            busy_reason.or(if lifecycle != Lifecycle::Approved {
                Some("Approve the candidate before integrating.")
            } else if review.valid_approval().is_none() {
                Some("The approval is no longer valid. Approve again.")
            } else if target_moved
                || review
                    .valid_approval()
                    .zip(target)
                    .is_some_and(|(a, t)| a.target_commit != *t)
            {
                Some("The Integration Target moved after approval. Refresh the candidate and approve again.")
            } else {
                None
            }),
        ));
        let has_changes = lifecycle == Lifecycle::Frozen
            || st.capture.as_ref().is_some_and(|c| c.counts.files > 0);
        out.push(gate(
            ActionName::ObjectiveComplete,
            busy_reason.or(match lifecycle {
                Lifecycle::Defined | Lifecycle::Active | Lifecycle::Frozen => {
                    if live {
                        Some(STOP_AGENT_FIRST)
                    } else if running_verification {
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
            busy_reason.or(if terminal {
                Some(CLOSED)
            } else if live {
                Some(STOP_AGENT_FIRST)
            } else if running_verification {
                Some(CHECK_RUNNING)
            } else {
                None
            }),
        ));
        out.push(gate(
            ActionName::ObjectiveCleanup,
            busy_reason.or(if !terminal {
                Some("Clean up becomes available once the Objective is integrated, completed or discarded.")
            } else if record.cleaned_up {
                Some("This Objective is already cleaned up.")
            } else if live {
                Some(STOP_AGENT_FIRST)
            } else {
                None
            }),
        ));
        out.push(gate(
            ActionName::ObjectiveDeleteHistory,
            busy_reason.or(delete_history_blocker(st)),
        ));
        out.push(match terminal_blocker(st) {
            Some(why) => disabled(ActionName::TerminalOpen, why),
            None if self.sandbox_tools().is_none() => {
                disabled(ActionName::TerminalOpen, TERMINAL_SANDBOX_UNAVAILABLE)
            }
            None if matches!(lifecycle, Lifecycle::Frozen | Lifecycle::Approved)
                || busy == Some("freeze") =>
            {
                AvailableAction {
                    action: ActionName::TerminalOpen,
                    enabled: true,
                    reason: Some("Opens read-only while the work is frozen for review.".into()),
                }
            }
            None => enabled(ActionName::TerminalOpen),
        });
        out
    }

    pub(crate) fn wire_workspace(&self, ws: &WsState, max_branches: usize) -> Workspace {
        let mut branches: Vec<String> = if max_branches == 0 {
            Vec::new()
        } else {
            ws.repo
                .list_branches()
                .map(|b| b.into_iter().map(|b| b.name).collect())
                .unwrap_or_default()
        };
        branches.retain(|b| !b.starts_with("lade/verify/") && !b.is_empty() && b.len() <= 255);
        if let Some(default) = &ws.default_branch
            && let Some(index) = branches.iter().position(|b| b == default)
        {
            let default = branches.remove(index);
            branches.insert(0, default);
        }
        branches.truncate(max_branches);
        Workspace {
            workspace_id: wire_id(ws.id.uuid()),
            path: wire_path(ws.path.as_os_str().as_encoded_bytes()),
            name: preview(&ws.name, 256),
            branches,
            default_branch: ws.default_branch.clone().filter(|b| !b.is_empty()),
        }
    }

    pub(crate) fn emit_objective(&mut self, oid: lade_core::domain::ids::ObjectiveId) {
        let Some(st) = self.objectives.get(&oid).cloned() else {
            return;
        };
        let detail = self.detail_of(&st);
        self.emit(Event::ObjectiveChanged(detail.summary()));
        self.emit(Event::ObjectiveDetail(Box::new(detail)));
        self.enforce_terminal_scope(oid);
    }
}

pub(crate) fn delete_history_blocker(st: &ObjState) -> Option<&'static str> {
    let record = st.objective().record();
    if !record.cleaned_up {
        Some("Clean up the Objective before deleting its Git history.")
    } else if st.row.history_deleted {
        Some("The Git history was already deleted.")
    } else {
        None
    }
}
