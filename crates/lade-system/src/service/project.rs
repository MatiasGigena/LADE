//! Projection of service state onto wire DTOs, including `available_actions` — the client's only
//! authority source. Every action's enablement is derived from the same domain rules the commands
//! enforce; a disabled action carries a human-readable reason.

use lade_core::domain::execution::ExecutionState;
use lade_core::domain::feedback::{AcceptedInput, DeliveryState, Draft, DraftState};
use lade_core::domain::ids::{GitId, WorkspaceId};
use lade_core::domain::objectives::{
    AttentionFacts, Badge as CoreBadge, Lifecycle, derive_attention,
};
use lade_core::domain::review::{
    Approval, ApprovalState, Candidate, CandidateStatus, Evidence, Verification,
    VerificationOutcome,
};
use lade_protocol::client::{
    self as wire, ActionName, ApprovalSummary, AvailableAction, Badge, CandidateSummary,
    ChangeCounts, DeliveryState as WireDelivery, DraftState as WireDraftState, Event, InputSummary,
    ObjectiveDetail, ObjectiveSummary, SessionInfo, TargetRef, VerificationState,
    VerificationSummary, Workspace,
};
use lade_protocol::types::{DeliveryMode, Model};

use super::Service;
use super::convert::{
    counter, preview, wire_execution_state, wire_git, wire_id, wire_lifecycle, wire_path,
    wire_reference, wire_ts,
};
use super::state::{ObjState, WsState};

const SUMMARY_OUTCOME_CHARS: usize = 512;
const DETAIL_LIST_MAX: usize = 64;
pub const FIXTURE_PROVIDER: &str = "lade-fixture";
/// Shared by the `review.freeze` projection and its rejection.
pub const NO_CHANGES_TO_FREEZE: &str = "No changes to freeze — complete without changes instead";
/// Shared by the `review.verify` projection and its rejection.
pub const VERIFICATION_SANDBOX_UNAVAILABLE: &str =
    "verification sandbox unavailable (bubblewrap/prlimit)";

pub fn wire_mode(mode: lade_core::domain::feedback::InputMode) -> DeliveryMode {
    use lade_core::domain::feedback::InputMode;
    match mode {
        InputMode::Prompt => DeliveryMode::Prompt,
        InputMode::Steer => DeliveryMode::Steer,
        InputMode::FollowUp => DeliveryMode::FollowUp,
    }
}

pub fn core_mode(mode: DeliveryMode) -> lade_core::domain::feedback::InputMode {
    use lade_core::domain::feedback::InputMode;
    match mode {
        DeliveryMode::Prompt => InputMode::Prompt,
        DeliveryMode::Steer => InputMode::Steer,
        DeliveryMode::FollowUp => InputMode::FollowUp,
    }
}

pub fn wire_input(input: &AcceptedInput) -> InputSummary {
    let (delivery, blocked_reason) = match input.delivery {
        DeliveryState::Queued => (WireDelivery::Queued, None),
        DeliveryState::Dispatched { .. } => (WireDelivery::Dispatched, None),
        DeliveryState::Received { .. } => (WireDelivery::Received, None),
        DeliveryState::Uncertain { .. } => (WireDelivery::Uncertain, None),
        DeliveryState::Blocked { reason } => {
            (WireDelivery::Blocked, Some(reason.code().to_string()))
        }
    };
    InputSummary {
        input_id: wire_id(input.id.uuid()),
        objective_id: wire_id(input.objective_id.uuid()),
        operation_id: wire_id(input.operation_id.uuid()),
        mode: wire_mode(input.mode),
        text_preview: preview(&input.rendered_text, 1024),
        accepted_at: wire_ts(input.accepted_at.unix_ms()),
        delivery,
        blocked_reason,
    }
}

pub fn wire_draft(st: &ObjState, draft: &Draft) -> wire::Draft {
    let record = draft.record();
    let (state, input_id) = match record.state {
        DraftState::Unsent => (WireDraftState::Unsent, None),
        DraftState::Submitted { input_id } => {
            (WireDraftState::Submitted, Some(wire_id(input_id.uuid())))
        }
    };
    wire::Draft {
        draft_id: wire_id(record.id.uuid()),
        objective_id: wire_id(record.objective_id.uuid()),
        destination_session_id: wire_id(record.destination_session_id.uuid()),
        reference: wire_reference(&record.reference),
        body: record.body.clone(),
        revision: counter(record.revision.value()),
        state,
        stale: draft.is_unsent() && st.draft_stale.get(&record.id).copied().unwrap_or(false),
        input_id,
    }
}

pub fn wire_candidate(candidate: &Candidate) -> CandidateSummary {
    CandidateSummary {
        candidate_id: wire_id(candidate.id.uuid()),
        ordinal: counter(u64::from(candidate.ordinal)),
        commit: wire_git(&candidate.commit),
        base_target_commit: wire_git(&candidate.base_target_commit),
        status: match candidate.status {
            CandidateStatus::Current => wire::CandidateStatus::Current,
            CandidateStatus::Historical => wire::CandidateStatus::Historical,
            CandidateStatus::Invalidated => wire::CandidateStatus::Invalidated,
        },
        files: counter(u64::from(candidate.changes.files)),
        added: counter(candidate.changes.added),
        removed: counter(candidate.changes.removed),
        created_at: wire_ts(candidate.created_at.unix_ms()),
    }
}

pub fn wire_verification(st: &ObjState, verification: &Verification) -> VerificationSummary {
    VerificationSummary {
        verification_id: wire_id(verification.id.uuid()),
        objective_id: wire_id(st.objective().id().uuid()),
        candidate_id: wire_id(verification.candidate_id.uuid()),
        command: verification
            .argv
            .iter()
            .take(256)
            .map(|a| preview(a, 4096))
            .collect(),
        state: match verification.outcome {
            VerificationOutcome::Running => VerificationState::Running,
            VerificationOutcome::Passed => VerificationState::Passed,
            VerificationOutcome::Failed => VerificationState::Failed,
            VerificationOutcome::Interrupted => VerificationState::Interrupted,
            VerificationOutcome::Error => VerificationState::Error,
        },
        started_at: wire_ts(verification.started_at.unix_ms()),
        output_tail: st.tails.get(&verification.id).cloned().unwrap_or_default(),
        truncated: verification.truncated,
        provenance: wire::Provenance::ServiceObserved,
        exit_code: verification
            .exit_code
            .and_then(|c| u64::try_from(c).ok())
            .map(counter),
        ended_at: verification.ended_at.map(|t| wire_ts(t.unix_ms())),
    }
}

pub fn wire_approval(approval: &Approval) -> ApprovalSummary {
    ApprovalSummary {
        approval_id: wire_id(approval.id.uuid()),
        candidate_id: wire_id(approval.candidate_id.uuid()),
        target_commit: wire_git(&approval.target_commit),
        state: match approval.state {
            ApprovalState::Valid => wire::ApprovalState::Valid,
            ApprovalState::Consumed => wire::ApprovalState::Consumed,
            ApprovalState::Invalidated { .. } => wire::ApprovalState::Invalidated,
        },
        override_reason: approval
            .override_reason
            .as_ref()
            .map(|r| preview(r, 2048))
            .filter(|r| !r.is_empty()),
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
    /// Resolves the Integration Target commit fresh and refreshes the cache.
    pub(crate) fn resolve_target(&mut self, workspace: WorkspaceId, branch: &str) -> Option<GitId> {
        let ws = self.workspaces.get(&workspace)?;
        let target = super::jobs::target_commit(&ws.repo, branch).ok();
        let key = (workspace, branch.to_string());
        match &target {
            Some(id) => {
                self.target_cache.insert(key, id.clone());
            }
            None => {
                self.target_cache.remove(&key);
            }
        }
        target
    }

    pub(crate) fn cached_target(&self, st: &ObjState) -> Option<GitId> {
        let record = st.objective().record();
        self.target_cache
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
        if record
            .execution
            .session()
            .is_some_and(|s| s.model.provider() == FIXTURE_PROVIDER)
        {
            badges.push(Badge::FixtureModel);
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
                generation: counter(record.execution.generation().value()),
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
            revision: counter(record.revision.value()),
        }
    }

    pub(crate) fn summary(&self, st: &ObjState) -> ObjectiveSummary {
        self.summary_with(st, self.cached_target(st).as_ref())
    }

    /// Full detail with a freshly resolved target commit.
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
            association_revision: counter(s.association_revision.value()),
            execution_generation: counter(record.execution.generation().value()),
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
        ObjectiveDetail {
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
            intent_revision: counter(record.intent_revision.value()),
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
                files: counter(u64::from(counts.files)),
                added: counter(counts.added),
                removed: counter(counts.removed),
            },
            unsent_drafts: counter(st.unsent_drafts() as u64),
            available_actions: self.actions(st, target),
            session,
            approval,
        }
    }

    /// The authority projection: one entry per [`ActionName`].
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
        let busy_reason = busy.map(|_| "another operation is in progress");
        let terminal = lifecycle.is_terminal();
        let target_moved = match (current, target) {
            (Some(c), Some(t)) => c.base_target_commit != *t,
            _ => false,
        };
        let mut out = Vec::with_capacity(15);
        out.push(gate(
            ActionName::ObjectiveUpdateIntent,
            busy_reason.or((!matches!(
                lifecycle,
                Lifecycle::Defined | Lifecycle::Active | Lifecycle::Frozen
            ))
            .then_some("intent is fixed once approved or closed; return for revision first")),
        ));
        out.push(gate(
            ActionName::ExecutionStart,
            busy_reason.or(
                if !matches!(lifecycle, Lifecycle::Defined | Lifecycle::Active) {
                    Some("execution starts only before review")
                } else if *state != ExecutionState::None {
                    Some("execution already started; resume instead")
                } else {
                    None
                },
            ),
        ));
        out.push(gate(
            ActionName::ExecutionCancel,
            match state {
                ExecutionState::Starting | ExecutionState::Running | ExecutionState::Idle => None,
                ExecutionState::Stopping => Some("execution is already stopping"),
                _ => Some("no live execution"),
            },
        ));
        out.push(gate(
            ActionName::ExecutionResume,
            busy_reason.or(if lifecycle != Lifecycle::Active {
                Some("resume requires an active objective; return a frozen candidate first")
            } else if !matches!(
                state,
                ExecutionState::Cancelled { .. } | ExecutionState::Failed { .. }
            ) {
                Some("only a cancelled or failed execution can resume")
            } else {
                None
            }),
        ));
        // Input to a stopped execution is accepted and waits (blocked) until an explicit resume.
        let input_blocker = if lifecycle != Lifecycle::Active {
            Some("input requires an active objective")
        } else if record.execution.session().is_none() {
            Some("start execution first")
        } else {
            None
        };
        let input_note = (!live && input_blocker.is_none())
            .then_some("execution is stopped; input is kept and delivered after Resume");
        let input = |action: ActionName, blocker: Option<&str>| match (blocker, input_note) {
            (None, Some(note)) => AvailableAction {
                action,
                enabled: true,
                reason: Some(note.into()),
            },
            (blocker, _) => gate(action, blocker),
        };
        out.push(input(ActionName::MessageSend, input_blocker));
        out.push(input(
            ActionName::FeedbackSubmit,
            input_blocker
                .or(st.row.worktree.is_none().then_some("no agent work yet"))
                .or((st.unsent_drafts() == 0).then_some("no unsent drafts")),
        ));
        out.push(gate(
            ActionName::ReviewFreeze,
            busy_reason.or(if lifecycle != Lifecycle::Active {
                Some("freeze requires an active objective")
            } else if st.row.worktree.is_none() {
                Some("no agent work yet")
            } else if *state == ExecutionState::Stopping {
                Some("execution is stopping")
            } else if st.capture.as_ref().is_some_and(|c| c.counts.files == 0) {
                Some(NO_CHANGES_TO_FREEZE)
            } else {
                None
            }),
        ));
        let latest_returnable = review
            .latest_candidate()
            .is_some_and(|c| c.status != CandidateStatus::Historical);
        out.push(gate(
            ActionName::ReviewReturn,
            busy_reason.or(
                if !matches!(lifecycle, Lifecycle::Frozen | Lifecycle::Approved) {
                    Some("nothing frozen to return")
                } else if !latest_returnable {
                    Some("no current candidate")
                } else {
                    None
                },
            ),
        ));
        out.push(gate(
            ActionName::ReviewVerify,
            busy_reason.or(if lifecycle != Lifecycle::Frozen {
                Some("verification requires a frozen, unapproved candidate")
            } else if current.is_none() {
                Some("no current candidate")
            } else if self.sandbox_tools().is_none() {
                Some(VERIFICATION_SANDBOX_UNAVAILABLE)
            } else {
                None
            }),
        ));
        let approve = match (lifecycle, current) {
            (Lifecycle::Frozen, Some(c)) if busy.is_none() => {
                if target_moved {
                    disabled(
                        ActionName::ReviewApprove,
                        "target moved; refresh the candidate",
                    )
                } else {
                    match review.evidence(c.id) {
                        Evidence::Passed => enabled(ActionName::ReviewApprove),
                        Evidence::Running => {
                            disabled(ActionName::ReviewApprove, "verification still running")
                        }
                        Evidence::Missing => AvailableAction {
                            action: ActionName::ReviewApprove,
                            enabled: true,
                            reason: Some(
                                "verification missing; an override reason is required".into(),
                            ),
                        },
                        Evidence::Failed => AvailableAction {
                            action: ActionName::ReviewApprove,
                            enabled: true,
                            reason: Some(
                                "verification failed; an override reason is required".into(),
                            ),
                        },
                    }
                }
            }
            (Lifecycle::Frozen, Some(_)) => disabled(
                ActionName::ReviewApprove,
                "another operation is in progress",
            ),
            _ if lifecycle == Lifecycle::Approved => disabled(
                ActionName::ReviewApprove,
                "already approved — integrate, or return for revision",
            ),
            _ => disabled(
                ActionName::ReviewApprove,
                "approval requires a frozen candidate",
            ),
        };
        out.push(approve);
        out.push(gate(
            ActionName::ReviewRefresh,
            busy_reason.or(
                if !matches!(lifecycle, Lifecycle::Frozen | Lifecycle::Approved) {
                    Some("refresh requires a frozen candidate")
                } else if current.is_none() {
                    Some("no current candidate")
                } else if !target_moved {
                    Some("target has not moved")
                } else {
                    None
                },
            ),
        ));
        out.push(gate(
            ActionName::ReviewIntegrate,
            busy_reason.or(if lifecycle != Lifecycle::Approved {
                Some("integration requires an approved candidate")
            } else if review.valid_approval().is_none() {
                Some("approval is no longer valid")
            } else if target_moved
                || review
                    .valid_approval()
                    .zip(target)
                    .is_some_and(|(a, t)| a.target_commit != *t)
            {
                Some("target moved since approval; refresh and approve again")
            } else {
                None
            }),
        ));
        // A frozen objective always holds a non-empty candidate (current or invalidated).
        let has_changes = lifecycle == Lifecycle::Frozen
            || st.capture.as_ref().is_some_and(|c| c.counts.files > 0);
        out.push(gate(
            ActionName::ObjectiveComplete,
            busy_reason.or(
                if !matches!(
                    lifecycle,
                    Lifecycle::Defined | Lifecycle::Active | Lifecycle::Frozen
                ) {
                    Some("objective is approved or closed")
                } else if live {
                    Some("cancel execution first")
                } else if running_verification {
                    Some("verification still running")
                } else if has_changes {
                    Some("the change set is not empty")
                } else {
                    None
                },
            ),
        ));
        out.push(gate(
            ActionName::ObjectiveDiscard,
            busy_reason.or(if terminal {
                Some("objective is already closed")
            } else if live {
                Some("cancel execution first")
            } else if running_verification {
                Some("verification still running")
            } else {
                None
            }),
        ));
        out.push(gate(
            ActionName::ObjectiveCleanup,
            busy_reason.or(if !terminal {
                Some("cleanup requires a completed or discarded objective")
            } else if record.cleaned_up {
                Some("already cleaned up")
            } else if live {
                Some("execution still live")
            } else {
                None
            }),
        ));
        out
    }

    pub(crate) fn wire_workspace(&self, ws: &WsState, max_branches: usize) -> Workspace {
        let mut branches: Vec<String> = ws
            .repo
            .list_branches()
            .map(|b| b.into_iter().map(|b| b.name).collect())
            .unwrap_or_default();
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

    /// `objective.changed` and `objective.detail` for the current state of `oid`.
    pub(crate) fn emit_objective(&mut self, oid: lade_core::domain::ids::ObjectiveId) {
        let Some(st) = self.objectives.get(&oid).cloned() else {
            return;
        };
        let detail = self.detail_of(&st);
        self.emit(Event::ObjectiveChanged(detail.summary()));
        self.emit(Event::ObjectiveDetail(Box::new(detail)));
    }
}
