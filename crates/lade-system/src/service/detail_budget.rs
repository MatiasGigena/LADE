use lade_protocol::client::{
    ApprovalState, ApprovalSummary, Attention, Badge, CandidateStatus, CandidateSummary,
    ChangeCounts, ClientBody, ClientEnvelope, Event, EventFrame, ExecutionState, Lifecycle,
    ObjectiveDetail, OperationState, OperationStatus, Outcome, Provenance, Rejection, Reply,
    ReplyData, SessionInfo, Settings, SnapshotData, VerificationState, VerificationSummary,
};
use lade_protocol::types::{Counter, Id, Model, ThinkingLevel};
use lade_protocol::{Framing, MAX_FRAME, MAX_NODES, encode, json_len};
use serde_json::Value;

use super::Service;
use super::handlers::SNAPSHOT_OBJECTIVES;
use super::project::{DETAIL_LIST_MAX, MAX_SUMMARY_BADGES};
use super::state::{ObjState, WsState};

fn nodes(value: &Value) -> usize {
    1 + match value {
        Value::Array(items) => items.iter().map(nodes).sum(),
        Value::Object(fields) => fields.values().map(nodes).sum(),
        _ => 0,
    }
}

// The longest detail-bearing reply name also bounds other lifecycle replies.
// Recovery wraps the original result, so reserve that structure too.
fn frames(detail: &ObjectiveDetail, epoch: &Id) -> Vec<ClientEnvelope> {
    let recovered = ReplyData::OperationGet(OperationStatus {
        state: OperationState::Completed,
        outcome: Some(Outcome::Ok(Box::new(ReplyData::ObjectiveDeleteHistory(
            Box::new(detail.clone()),
        )))),
    });
    [
        ReplyData::ObjectiveCreate(Box::new(detail.clone())),
        ReplyData::ObjectiveUpdateIntent(Box::new(detail.clone())),
        ReplyData::ObjectiveDeleteHistory(Box::new(detail.clone())),
        recovered,
    ]
    .into_iter()
    .map(|data| {
        ClientEnvelope::new(ClientBody::Reply(Reply::ok(
            detail.objective_id.clone(),
            data,
        )))
    })
    .chain(std::iter::once(ClientEnvelope::new(ClientBody::Event(
        EventFrame {
            service_epoch: epoch.clone(),
            sequence: Counter::new(u64::MAX),
            event: Event::ObjectiveDetail(Box::new(detail.clone())),
        },
    ))))
    .collect()
}

fn capacity(frames: Vec<ClientEnvelope>) -> Option<(usize, usize)> {
    frames
        .into_iter()
        .try_fold((MAX_FRAME, MAX_NODES), |(bytes, remaining_nodes), frame| {
            let encoded = encode(&frame, Framing::Client).ok()?;
            let value = serde_json::to_value(frame).ok()?;
            Some((
                bytes.min(MAX_FRAME - (encoded.len() - 4)),
                remaining_nodes.min(MAX_NODES - nodes(&value)),
            ))
        })
}

// Existing projection/schema maxima determine the reserve, rather than a
// smaller intent quota. Verification text spends the remaining capacity.
fn future_metadata(detail: &ObjectiveDetail) -> ObjectiveDetail {
    let mut future = detail.clone();
    let max = Counter::new(u64::MAX);
    future.lifecycle = Lifecycle::CompletedNoChanges;
    future.attention = Attention::ReadyForReview;
    future.execution.state = ExecutionState::Cancelled;
    future.execution.generation = max.clone();
    future.execution.reason = Some("\u{1}".repeat(512)); // ShortText
    future.badges = vec![Badge::IntegrationConflict; MAX_SUMMARY_BADGES];
    future.revision = max.clone();
    future.intent_revision = max.clone();
    future.unsent_drafts = max.clone();
    future.change_counts = ChangeCounts {
        files: max.clone(),
        added: max.clone(),
        removed: max.clone(),
    };
    for action in &mut future.available_actions {
        action.enabled = false;
        action.reason = Some("\u{1}".repeat(512)); // ShortText
    }
    future.session = Some(SessionInfo {
        session_id: detail.objective_id.clone(),
        association_revision: max.clone(),
        execution_generation: max.clone(),
        model: Model {
            provider: "\"".repeat(128),
            id: "\"".repeat(256),
        },
        thinking: Some(ThinkingLevel::Minimal),
    });
    future.approval = Some(ApprovalSummary {
        approval_id: detail.objective_id.clone(),
        candidate_id: detail.objective_id.clone(),
        target_commit: detail.start_commit.clone(),
        state: ApprovalState::Invalidated,
        override_reason: Some("\u{1}".repeat(2048)),
    });
    future.history_deleted = Some(false);
    future.candidates = vec![
        CandidateSummary {
            candidate_id: detail.objective_id.clone(),
            ordinal: max.clone(),
            commit: detail.start_commit.clone(),
            base_target_commit: detail.start_commit.clone(),
            status: CandidateStatus::Invalidated,
            files: max.clone(),
            added: max.clone(),
            removed: max.clone(),
            created_at: detail.updated_at.clone(),
        };
        DETAIL_LIST_MAX
    ];
    future.verifications = vec![
        VerificationSummary {
            verification_id: detail.objective_id.clone(),
            objective_id: detail.objective_id.clone(),
            candidate_id: detail.objective_id.clone(),
            command: vec![String::new()],
            state: VerificationState::Interrupted,
            started_at: detail.updated_at.clone(),
            output_tail: String::new(),
            truncated: false,
            provenance: Provenance::ServiceObserved,
            exit_code: Some(max),
            ended_at: Some(detail.updated_at.clone()),
        };
        DETAIL_LIST_MAX
    ];
    future
}

fn bounded_text(text: &str, budget: usize, tail: bool) -> (String, usize) {
    let mut cost = 0;
    let mut take = |ch: &char| {
        let next = match ch {
            '\"' | '\\' | '\u{8}' | '\t' | '\n' | '\u{c}' | '\r' => 2,
            '\u{0}'..='\u{1f}' => 6,
            _ => ch.len_utf8(),
        };
        if cost + next > budget {
            false
        } else {
            cost += next;
            true
        }
    };
    let result = if tail {
        let suffix: Vec<char> = text.chars().rev().take_while(&mut take).collect();
        suffix.into_iter().rev().collect()
    } else {
        text.chars().take_while(&mut take).collect()
    };
    (result, cost)
}

fn take_previews(rows: &mut [VerificationSummary]) -> Vec<(Vec<String>, String)> {
    rows.iter_mut()
        .map(|row| {
            (
                std::mem::replace(&mut row.command, vec![String::new()]),
                std::mem::take(&mut row.output_tail),
            )
        })
        .collect()
}

fn restore_previews(
    rows: &mut [VerificationSummary],
    previews: Vec<(Vec<String>, String)>,
    capacity: Option<(usize, usize)>,
) {
    let Some((mut bytes, mut remaining_nodes)) = capacity else {
        for row in rows {
            row.truncated = true;
        }
        return;
    };
    let total = previews.len();
    for (index, (row, (command, tail))) in rows.iter_mut().zip(previews).enumerate() {
        let mut row_bytes = bytes / (total - index);
        let mut row_nodes = remaining_nodes / (total - index);
        let initial_bytes = row_bytes;
        let initial_nodes = row_nodes;
        row.command.clear();
        for (arg_index, arg) in command.iter().enumerate() {
            if arg_index != 0 {
                if row_bytes < 3 || row_nodes == 0 {
                    break;
                }
                row_bytes -= 3; // comma and empty-string quotes
                row_nodes -= 1;
            }
            let (preview, cost) = bounded_text(arg, row_bytes, false);
            row.command.push(preview);
            row_bytes -= cost;
        }
        let (preview, cost) = bounded_text(&tail, row_bytes, true);
        row.output_tail = preview;
        row_bytes -= cost;
        row.truncated |= row.command != command || row.output_tail != tail;
        bytes -= initial_bytes - row_bytes;
        remaining_nodes -= initial_nodes - row_nodes;
    }
}

fn reply_frames(data: ReplyData, request_id: &Id) -> Vec<ClientEnvelope> {
    [
        data.clone(),
        ReplyData::OperationGet(OperationStatus {
            state: OperationState::Completed,
            outcome: Some(Outcome::Ok(Box::new(data))),
        }),
    ]
    .into_iter()
    .map(|data| ClientEnvelope::new(ClientBody::Reply(Reply::ok(request_id.clone(), data))))
    .collect()
}

pub(super) fn budget_verification(mut row: VerificationSummary) -> VerificationSummary {
    let previews = take_previews(std::slice::from_mut(&mut row));
    let mut frames = reply_frames(ReplyData::ReviewVerify(row.clone()), &row.objective_id);
    frames.push(ClientEnvelope::new(ClientBody::Event(EventFrame {
        service_epoch: row.objective_id.clone(),
        sequence: Counter::new(u64::MAX),
        event: Event::VerificationChanged(row.clone()),
    })));
    restore_previews(std::slice::from_mut(&mut row), previews, capacity(frames));
    row
}

fn minimal_snapshot(mut data: SnapshotData, future: bool) -> SnapshotData {
    for workspace in &mut data.workspaces {
        if future {
            // Branch previews must keep an actual target usable. Reserve one
            // worst-case 255-byte valid label even for a currently empty repo.
            workspace.branches = vec!["\"".repeat(255)];
        } else {
            workspace.branches.truncate(1);
        }
    }
    for objective in &mut data.objectives {
        objective.outcome.clear();
        if future {
            objective.lifecycle = Lifecycle::CompletedNoChanges;
            objective.attention = Attention::ReadyForReview;
            objective.badges = vec![Badge::IntegrationConflict; MAX_SUMMARY_BADGES];
            objective.revision = Counter::new(u64::MAX);
            objective.execution.generation = Counter::new(u64::MAX);
            objective.execution.state = ExecutionState::Cancelled;
            objective.execution.reason = Some(String::new());
        } else if let Some(reason) = &mut objective.execution.reason {
            reason.clear();
        }
    }
    data.sequence = Counter::new(u64::MAX);
    data
}

fn snapshot_frames(data: SnapshotData) -> Vec<ClientEnvelope> {
    let id = data.service_epoch.clone();
    vec![ClientEnvelope::new(ClientBody::Reply(Reply::ok(
        id,
        ReplyData::Snapshot(data),
    )))]
}

impl Service {
    pub(crate) fn check_intent_representation(
        &self,
        detail: &ObjectiveDetail,
    ) -> Result<(), Rejection> {
        if capacity(frames(detail, &self.epoch)).is_some()
            && capacity(frames(&future_metadata(detail), &self.epoch)).is_some()
        {
            Ok(())
        } else {
            Err(Rejection::new(
                lade_protocol::client::ErrorCode::LimitReached,
                lade_protocol::client::SafeAction::None,
                "This intent is too large to save. Shorten it and try again.",
            ))
        }
    }

    pub(crate) fn budget_detail(&self, mut detail: ObjectiveDetail) -> ObjectiveDetail {
        let previews = take_previews(&mut detail.verifications);
        let capacity = capacity(frames(&detail, &self.epoch));
        restore_previews(&mut detail.verifications, previews, capacity);
        detail
    }

    pub(crate) fn check_snapshot_representation(
        &self,
        settings: &Settings,
        objective: Option<&ObjState>,
        workspace: Option<&WsState>,
    ) -> Result<(), Rejection> {
        let data = self.snapshot_data(settings, objective, workspace, 0);
        let mut future = minimal_snapshot(data, true);
        // An older record can become recent after any lifecycle change. Reserve
        // the largest immutable titles, independent of today's recency window.
        let mut summaries: Vec<_> = self
            .objectives
            .values()
            .chain(objective)
            .map(|state| {
                let mut summary = self.summary(state);
                summary.outcome.clear();
                summary.lifecycle = Lifecycle::CompletedNoChanges;
                summary.attention = Attention::ReadyForReview;
                summary.badges = vec![Badge::IntegrationConflict; MAX_SUMMARY_BADGES];
                summary.revision = Counter::new(u64::MAX);
                summary.execution.generation = Counter::new(u64::MAX);
                summary.execution.state = ExecutionState::Cancelled;
                summary.execution.reason = Some(String::new());
                let bytes = json_len(&summary);
                (bytes, summary)
            })
            .collect();
        summaries.sort_by_key(|(bytes, _)| std::cmp::Reverse(*bytes));
        future.objectives = summaries
            .into_iter()
            .take(SNAPSHOT_OBJECTIVES)
            .map(|(_, summary)| summary)
            .collect();
        if capacity(snapshot_frames(future)).is_some()
            && capacity(reply_frames(
                ReplyData::SettingsUpdate(settings.clone()),
                &self.epoch,
            ))
            .is_some()
        {
            Ok(())
        } else {
            Err(Rejection::new(
                lade_protocol::client::ErrorCode::LimitReached,
                lade_protocol::client::SafeAction::None,
                "The saved workspace state would be too large to load. Shorten the verification settings before trying again.",
            ))
        }
    }

    pub(crate) fn budget_snapshot(data: SnapshotData) -> SnapshotData {
        let mut result = minimal_snapshot(data.clone(), false);
        let Some((mut bytes, mut remaining_nodes)) = capacity(snapshot_frames(result.clone()))
        else {
            // Existing unrepresentable durable data is reported explicitly;
            // never omit required rows or alter the stored Settings to fit.
            return data;
        };
        for (index, (row, source)) in result
            .objectives
            .iter_mut()
            .zip(&data.objectives)
            .enumerate()
        {
            let budget = bytes / (data.objectives.len() - index);
            let (preview, used) = bounded_text(&source.outcome, budget, false);
            row.outcome = preview;
            bytes -= used;
            if let (Some(target), Some(reason)) =
                (&mut row.execution.reason, &source.execution.reason)
            {
                let (preview, used) =
                    bounded_text(reason, bytes / (data.objectives.len() - index), false);
                *target = preview;
                bytes -= used;
            }
        }
        for (index, (row, source)) in result
            .workspaces
            .iter_mut()
            .zip(&data.workspaces)
            .enumerate()
        {
            let mut budget = bytes / (data.workspaces.len() - index);
            let mut node_budget = remaining_nodes / (data.workspaces.len() - index);
            for branch in source.branches.iter().skip(row.branches.len()) {
                let cost = json_len(branch).saturating_add(usize::from(!row.branches.is_empty()));
                if cost > budget || node_budget == 0 {
                    break;
                }
                row.branches.push(branch.clone());
                budget -= cost;
                node_budget -= 1;
                bytes -= cost;
                remaining_nodes -= 1;
            }
        }
        result.sequence = data.sequence;
        result
    }
}

#[cfg(test)]
mod tests {
    use lade_protocol::MAX_STRING;
    use lade_protocol::client::{ObjectiveSelector, Request};

    use super::*;
    use crate::service::test_harness::Harness;

    #[test]
    fn json_nodes_count_every_value() {
        assert_eq!(nodes(&serde_json::json!(1)), 1);
        assert_eq!(nodes(&serde_json::json!([1, [2, 3]])), 5);
        assert_eq!(nodes(&serde_json::json!({"a": {"b": null}, "c": []})), 4);
    }

    #[test]
    fn bounded_text_charges_json_escapes_and_keeps_the_head_or_tail() {
        assert_eq!(bounded_text("abc", 10, false), ("abc".into(), 3));
        assert_eq!(bounded_text("abcdef", 4, false), ("abcd".into(), 4));
        assert_eq!(bounded_text("abcdef", 4, true), ("cdef".into(), 4));
        assert_eq!(
            bounded_text("a\"b", 3, false),
            ("a\"".into(), 3),
            "a quote costs two"
        );
        assert_eq!(
            bounded_text("\u{1}x", 6, false),
            ("\u{1}".into(), 6),
            "control chars cost six"
        );
        assert_eq!(
            bounded_text("é", 1, false),
            (String::new(), 0),
            "never splits a character"
        );
        assert_eq!(bounded_text("x\ny", 3, true), ("\ny".into(), 3));
    }

    fn verification(tail: String, command: Vec<String>) -> VerificationSummary {
        let id = crate::service::convert::wire_id(crate::support::new_uuid());
        VerificationSummary {
            verification_id: id.clone(),
            objective_id: id.clone(),
            candidate_id: id,
            command,
            state: VerificationState::Failed,
            started_at: crate::support::wire_ts(0),
            output_tail: tail,
            truncated: false,
            provenance: Provenance::ServiceObserved,
            exit_code: None,
            ended_at: None,
        }
    }

    #[test]
    fn an_oversized_verification_command_is_cut_to_fit_and_marked_truncated() {
        let small = budget_verification(verification("ok\n".into(), vec!["true".into()]));
        assert_eq!(small.output_tail, "ok\n");
        assert!(!small.truncated);

        let arg = "\u{1}".repeat(4096);
        let command = vec![arg.clone(); 256];
        let budgeted = budget_verification(verification("the end".into(), command));
        assert!(budgeted.truncated);
        assert_eq!(budgeted.command[0], arg, "earlier arguments are kept whole");
        assert!(budgeted.command.len() < 256);
        for frame in reply_frames(
            ReplyData::ReviewVerify(budgeted.clone()),
            &budgeted.objective_id,
        ) {
            let encoded = encode(&frame, Framing::Client);
            assert!(
                encoded.is_ok(),
                "the reply fits a frame: {:?}",
                encoded.err()
            );
        }
    }

    #[test]
    fn details_and_snapshots_of_ordinary_objectives_are_representable() {
        let mut harness = Harness::new("budget-detail");
        let (id, oid) = harness.create_objective("Budget");
        let st = harness.service.objectives[&oid].clone();
        let detail = harness.service.detail_of(&st);
        assert!(harness.service.check_intent_representation(&detail).is_ok());
        assert!(
            harness
                .service
                .check_snapshot_representation(&harness.service.settings.clone(), Some(&st), None)
                .is_ok()
        );
        let mut verifying = detail.clone();
        let tail = "\u{1}".repeat(4096);
        verifying.verifications = vec![verification(tail, vec!["sh".into()]); DETAIL_LIST_MAX];
        let budgeted = harness.service.budget_detail(verifying);
        assert!(budgeted.verifications.iter().all(|v| v.truncated));
        assert!(
            budgeted
                .verifications
                .iter()
                .all(|v| !v.output_tail.is_empty()),
            "each keeps a share"
        );
        for frame in frames(&budgeted, &harness.service.epoch) {
            assert!(encode(&frame, Framing::Client).is_ok());
        }
        assert!(
            harness
                .call(Request::ObjectiveGet(ObjectiveSelector {
                    objective_id: id
                }))
                .is_ok()
        );
    }

    #[test]
    fn snapshot_budgets_cut_long_outcomes_but_keep_every_row() {
        let mut harness = Harness::new("budget-snapshot");
        for n in 0..6 {
            harness.create_objective(&format!("Objective {n}"));
        }
        let mut data =
            harness
                .service
                .snapshot_data(&harness.service.settings.clone(), None, None, 64);
        for summary in &mut data.objectives {
            summary.outcome = "o".repeat(MAX_STRING - 1);
        }
        let budgeted = Service::budget_snapshot(data.clone());
        assert_eq!(budgeted.objectives.len(), data.objectives.len());
        assert!(
            budgeted
                .objectives
                .iter()
                .all(|o| o.outcome.len() < MAX_STRING - 1)
        );
        assert!(
            budgeted
                .objectives
                .iter()
                .all(|o| o.outcome.starts_with('o'))
        );
        assert_eq!(budgeted.workspaces[0].branches, data.workspaces[0].branches);
        assert!(capacity(snapshot_frames(budgeted)).is_some());
    }
}
