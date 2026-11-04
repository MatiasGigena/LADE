//! How an Objective is shown to the client: summaries, details and Workspaces built from the
//! service's state, which actions each one offers (`actions`), and the reply budget that keeps
//! every detail and snapshot inside one frame (`budget`).

pub(super) mod actions;
pub(super) mod budget;
#[cfg(test)]
mod tests;

use lade_core::domain::feedback::DeliveryState;
use lade_core::domain::ids::{GitId, WorkspaceId};
use lade_core::domain::objectives::{
    AttentionFacts, Badge as CoreBadge, Lifecycle, derive_attention,
};
use lade_core::domain::review::Evidence;
use lade_protocol::client::{
    self as wire, Badge, ChangeCounts, Event, ObjectiveDetail, ObjectiveSummary, SessionInfo,
    TargetRef, Workspace,
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

    fn facts(st: &ObjState, target: Option<&GitId>) -> AttentionFacts {
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

    pub(crate) fn summary_with(st: &ObjState, target: Option<&GitId>) -> ObjectiveSummary {
        let objective = st.objective();
        let record = objective.record();
        let view = derive_attention(objective, &Self::facts(st, target));
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
        Self::summary_with(st, self.cached_target(st).as_ref())
    }

    pub(crate) fn detail_of(&mut self, st: &ObjState) -> ObjectiveDetail {
        let target = self.fresh_target(st);
        self.detail_with(st, target.as_ref())
    }

    pub(crate) fn detail_with(&self, st: &ObjState, target: Option<&GitId>) -> ObjectiveDetail {
        let summary = Self::summary_with(st, target);
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

    pub(crate) fn wire_workspace(ws: &WsState, max_branches: usize) -> Workspace {
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
