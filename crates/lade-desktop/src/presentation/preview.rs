//! Labelled development preview (`lade-desktop --preview`).
//!
//! Everything here is fixture data. The header always reads "Preview data" while it is shown, the
//! responder answers queries from the fixture and refuses every command with a visible notice, so
//! nothing can be mistaken for service authority or a real execution.

use std::collections::HashMap;
use std::rc::Weak;

use super::drafts::DraftEditor;
use super::intents::{Intent, IntentSink};
use super::model::{
    ActionName, ActivityItem, ActivityItemId, ActivityRole, ApprovalId, ArtifactDigest, Attention,
    AvailableAction, Badge, Basis, CandidateId, CandidateStatus, CandidateSummary, CaptureId,
    ChangeCounts, ChangeList, ChangeStatus, ChangedFile, ConnectionState, DeliveryMode,
    DeliveryState, DiffHunk, DiffLine, DiffLineKind, Draft, DraftId, DraftState, Execution,
    ExecutionState, FileDiff, FileEncoding, FilePage, GitId, InputId, InputSummary,
    Intent as ObjectiveIntent, Lifecycle, ModelRef, NoticeLevel, ObjectiveDetail, ObjectiveId,
    ObjectiveSummary, OperationId, Provenance, Reference, ReferenceVersion, RepoPath, SearchResult,
    SearchResultKind, SearchResults, SearchScope, ServiceNotice, SessionId, SessionInfo, Settings,
    Side, TargetRef, Timestamp, VerificationId, VerificationState, VerificationSummary, Workspace,
    WorkspaceId,
};
use super::store::{PresentationState, Store, StoreUpdate};

pub const MAIN_OBJECTIVE: &str = "7d0c2a3e-5b1f-4c8e-9a62-0f4f1b7c2d11";
pub const SECOND_OBJECTIVE: &str = "a41f9c07-2e6d-4b3a-8f15-6c9e0d2b7a22";
pub const SAVED_DRAFT: &str = "d1000000-0000-4000-8000-000000000001";
const WORKSPACE: &str = "5e3b1d2c-0a9f-4e7d-b6c5-1f2e3d4c5b6a";
const SESSION: &str = "c3d4e5f6-0718-4293-a4b5-c6d7e8f90a1b";
const CAPTURE: &str = "f00dcafe-1234-4abc-9def-001122334455";
const CANDIDATE_1: &str = "b0000001-1111-4111-8111-111111111111";
const CANDIDATE_2: &str = "b0000002-2222-4222-8222-222222222222";
const PAGE_BYTES: usize = 4096;

/// The status line every preview surface shows.
pub const PREVIEW_LABEL: &str = "Preview data — fixture, no service connected";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewVariant {
    Standard,
    /// Simulates a lost service link so disconnected states can be reviewed.
    Disconnected,
}

fn id(s: &str) -> ObjectiveId {
    ObjectiveId::new(s)
}

fn commit(hex: &str) -> GitId {
    GitId::sha1(hex)
}

fn target() -> TargetRef {
    TargetRef {
        branch: "main".into(),
        commit: commit("4f2c9e1a7b3d5f60718293a4b5c6d7e8f9012345"),
    }
}

fn action(action: ActionName, reason: Option<&str>) -> AvailableAction {
    AvailableAction {
        action,
        enabled: reason.is_none(),
        reason: reason.map(str::to_owned),
    }
}

struct FixtureFile {
    path: &'static str,
    status: ChangeStatus,
    old: Option<String>,
    new: Option<String>,
    binary: bool,
}

const REQUEST_OLD: &str = r#"import axios from 'axios';
import { setTimeout } from 'timers/promises';

export interface RequestOptions {
  retries?: number;
  timeoutMs?: number;
  headers?: Record<string, string>;
}

export async function fetchWithRetry(
  url: string,
  options: RequestOptions = {},
): Promise<Response> {
  const retries = options.retries ?? 3;
  for (let attempt = 0; attempt <= retries; attempt++) {
    try {
      const res = await fetch(url, {
        ...options,
      });
      return res;
    } catch (error) {
      if (attempt === retries) {
        throw error;
      }
      await setTimeout(2 ** attempt * 100);
    }
  }
  throw new Error('unreachable');
}
"#;

const REQUEST_NEW: &str = r#"import axios from 'axios';
import { setTimeout } from 'timers/promises';
import { NetworkError, TimeoutError } from './errors';

export interface RequestOptions {
  retries?: number;
  timeoutMs?: number;
  headers?: Record<string, string>;
}

/** Retries transient failures; a timeout is reported separately from a confirmed failure. */
export async function fetchWithRetry(
  url: string,
  options: RequestOptions = {},
): Promise<Response> {
  const retries = Math.max(0, options.retries ?? 3);
  const timeoutMs = options.timeoutMs ?? 10_000;
  for (let attempt = 0; attempt <= retries; attempt++) {
    const controller = new AbortController();
    const timeout = setTimeout(timeoutMs).then(() => controller.abort());
    try {
      const res = await fetch(url, {
        ...options,
        signal: controller.signal,
      });
      return res;
    } catch (error) {
      if (attempt === retries) {
        if (error instanceof Error && error.name === 'AbortError') {
          throw new TimeoutError(`Request timed out after ${timeoutMs} ms`, { cause: error });
        }
        throw new NetworkError('Request failed', { cause: error });
      }
      await setTimeout(2 ** attempt * 100);
    } finally {
      void timeout;
    }
  }
  throw new Error('unreachable');
}
"#;

const RETRY_OLD: &str = "export function backoff(attempt: number): number {\n  return 2 ** attempt * 100;\n}\n\nexport function shouldRetry(status: number): boolean {\n  return status >= 500;\n}\n";
const RETRY_NEW: &str = "const MAX_BACKOFF_MS = 5_000;\n\nexport function backoff(attempt: number): number {\n  // Jitter avoids synchronized retries from many workers.\n  const base = Math.min(2 ** attempt * 100, MAX_BACKOFF_MS);\n  return base / 2 + Math.random() * (base / 2);\n}\n\nexport function shouldRetry(status: number): boolean {\n  return status === 408 || status === 429 || status >= 500;\n}\n";
const TEST_OLD: &str = "import { fetchWithRetry } from '../../src/network/request';\n\ntest('retries server errors', async () => {\n  expect(await fetchWithRetry('http://fixture/500')).toBeDefined();\n});\n";
const TEST_NEW: &str = "import { fetchWithRetry } from '../../src/network/request';\nimport { TimeoutError } from '../../src/network/errors';\n\ntest('retries server errors', async () => {\n  expect(await fetchWithRetry('http://fixture/500')).toBeDefined();\n});\n\ntest('reports a timeout distinctly — “délai dépassé” is not a failure', async () => {\n  await expect(fetchWithRetry('http://fixture/slow', { timeoutMs: 5 })).rejects.toBeInstanceOf(TimeoutError);\n});\n";
const DOC_OLD: &str = "# Reliability\r\n\r\nNetwork calls retry three times.\r\n";
const DOC_NEW: &str = "# Reliability\r\n\r\nNetwork calls retry three times.\r\n\r\nA timeout raises `TimeoutError`; a confirmed failure raises `NetworkError`.\r\nBoth keep the original error as `cause`.\r\n";

fn large_fixture() -> String {
    let mut out = String::from("[\n");
    for i in 0..400 {
        out.push_str(&format!(
            "  {{ \"id\": {i}, \"status\": {}, \"latency_ms\": {}, \"region\": \"eu-north-{}\" }},\n",
            if i % 7 == 0 { 503 } else { 200 },
            40 + (i * 37) % 900,
            i % 3
        ));
    }
    out.push_str(
        "  { \"id\": 400, \"status\": 200, \"latency_ms\": 12, \"region\": \"eu-north-0\" }\n]\n",
    );
    out
}

fn large_fixture_old() -> String {
    large_fixture().replace("\"latency_ms\": 12,", "\"latency_ms\": 15,")
}

fn fixture_files() -> Vec<FixtureFile> {
    vec![
        FixtureFile {
            path: "src/network/request.ts",
            status: ChangeStatus::Modified,
            old: Some(REQUEST_OLD.into()),
            new: Some(REQUEST_NEW.into()),
            binary: false,
        },
        FixtureFile {
            path: "src/network/retry.ts",
            status: ChangeStatus::Modified,
            old: Some(RETRY_OLD.into()),
            new: Some(RETRY_NEW.into()),
            binary: false,
        },
        FixtureFile {
            path: "tests/network/retry.test.ts",
            status: ChangeStatus::Modified,
            old: Some(TEST_OLD.into()),
            new: Some(TEST_NEW.into()),
            binary: false,
        },
        FixtureFile {
            path: "docs/reliability.md",
            status: ChangeStatus::Modified,
            old: Some(DOC_OLD.into()),
            new: Some(DOC_NEW.into()),
            binary: false,
        },
        FixtureFile {
            path: "fixtures/network/latency-samples.json",
            status: ChangeStatus::Modified,
            old: Some(large_fixture_old()),
            new: Some(large_fixture()),
            binary: false,
        },
        FixtureFile {
            path: "docs/assets/retry-states.png",
            status: ChangeStatus::Added,
            old: None,
            new: None,
            binary: true,
        },
    ]
}

/// Line-level LCS diff with three lines of context and exact byte offsets.
pub fn make_hunks(old: &str, new: &str) -> Vec<DiffHunk> {
    fn split(text: &str) -> Vec<(&str, u64)> {
        let mut out = Vec::new();
        let mut start = 0u64;
        for line in text.split_inclusive('\n') {
            out.push((line.strip_suffix('\n').unwrap_or(line), start));
            start += line.len() as u64;
        }
        out
    }
    let a = split(old);
    let b = split(new);
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i].0 == b[j].0 {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut ops: Vec<DiffLine> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && a[i].0 == b[j].0 {
            ops.push(DiffLine {
                kind: DiffLineKind::Context,
                old_no: Some(i as u64 + 1),
                new_no: Some(j as u64 + 1),
                text: a[i].0.to_owned(),
                old_byte_start: Some(a[i].1),
                new_byte_start: Some(b[j].1),
            });
            i += 1;
            j += 1;
        } else if j < m && (i >= n || lcs[i][j + 1] > lcs[i + 1][j]) {
            ops.push(DiffLine {
                kind: DiffLineKind::Add,
                old_no: None,
                new_no: Some(j as u64 + 1),
                text: b[j].0.to_owned(),
                old_byte_start: None,
                new_byte_start: Some(b[j].1),
            });
            j += 1;
        } else {
            ops.push(DiffLine {
                kind: DiffLineKind::Del,
                old_no: Some(i as u64 + 1),
                new_no: None,
                text: a[i].0.to_owned(),
                old_byte_start: Some(a[i].1),
                new_byte_start: None,
            });
            i += 1;
        }
    }
    const CONTEXT: usize = 3;
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, l)| l.kind != DiffLineKind::Context)
        .map(|(k, _)| k)
        .collect();
    let mut hunks = Vec::new();
    let mut k = 0;
    while k < changed.len() {
        let begin = changed[k].saturating_sub(CONTEXT);
        let mut end = changed[k];
        while k + 1 < changed.len() && changed[k + 1] <= end + 2 * CONTEXT + 1 {
            k += 1;
            end = changed[k];
        }
        let end = (end + CONTEXT + 1).min(ops.len());
        let lines: Vec<DiffLine> = ops[begin..end].to_vec();
        let old_start = lines.iter().find_map(|l| l.old_no).unwrap_or(0);
        let new_start = lines.iter().find_map(|l| l.new_no).unwrap_or(0);
        hunks.push(DiffHunk {
            old_start,
            old_lines: lines.iter().filter(|l| l.old_no.is_some()).count() as u64,
            new_start,
            new_lines: lines.iter().filter(|l| l.new_no.is_some()).count() as u64,
            lines,
        });
        k += 1;
    }
    hunks
}

fn counts(old: &str, new: &str) -> (u64, u64) {
    make_hunks(old, new)
        .iter()
        .flat_map(|h| &h.lines)
        .fold((0, 0), |(a, r), l| match l.kind {
            DiffLineKind::Add => (a + 1, r),
            DiffLineKind::Del => (a, r + 1),
            DiffLineKind::Context => (a, r),
        })
}

fn digest(side: &str, path: &str) -> ArtifactDigest {
    // Fixture identities only; not content hashes.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in side.bytes().chain(path.bytes()) {
        h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
    }
    ArtifactDigest::new(format!("{h:016x}{h:016x}{h:016x}{h:016x}"))
}

fn summary(
    objective: &str,
    title: &str,
    outcome: &str,
    (lifecycle, execution, attention): (Lifecycle, ExecutionState, Attention),
    badges: &[Badge],
    updated: Timestamp,
) -> ObjectiveSummary {
    ObjectiveSummary {
        objective_id: id(objective),
        workspace_id: WorkspaceId::new(WORKSPACE),
        title: title.into(),
        outcome: outcome.into(),
        lifecycle,
        execution: Execution {
            state: execution,
            generation: u64::from(execution != ExecutionState::None),
            reason: (execution == ExecutionState::Cancelled)
                .then(|| "interrupted by a service restart".to_owned()),
        },
        attention,
        badges: badges.to_vec(),
        updated_at: updated,
        revision: 4,
    }
}

fn minutes(now: Timestamp, m: i64) -> Timestamp {
    Timestamp(now.0 - m * 60_000)
}

fn summaries(now: Timestamp) -> Vec<ObjectiveSummary> {
    vec![
        summary(
            MAIN_OBJECTIVE,
            "Make retries safe",
            "Harden network calls and distinguish a timeout from a confirmed failure, with clearer error types and updated tests.",
            (
                Lifecycle::Frozen,
                ExecutionState::Idle,
                Attention::NeedsAttention,
            ),
            &[Badge::VerificationFailed, Badge::UnsentDrafts],
            minutes(now, 12),
        ),
        summary(
            SECOND_OBJECTIVE,
            "Explain interrupted work",
            "Provide clear recovery steps when an agent run is interrupted by a service restart.",
            (
                Lifecycle::Active,
                ExecutionState::Cancelled,
                Attention::NeedsAttention,
            ),
            &[Badge::Interrupted, Badge::DeliveryUncertain],
            minutes(now, 41),
        ),
        summary(
            "c9b8a7d6-0000-4000-8000-000000000003",
            "Speed up workspace search",
            "Improve indexing and reduce latency for large repositories without loading every file into memory.",
            (
                Lifecycle::Approved,
                ExecutionState::Idle,
                Attention::ReadyForReview,
            ),
            &[],
            minutes(now, 95),
        ),
        summary(
            "c9b8a7d6-0000-4000-8000-000000000004",
            "Improve test reliability",
            "Reduce flaky tests in CI by isolating clock and network dependencies.",
            (
                Lifecycle::Active,
                ExecutionState::Running,
                Attention::Working,
            ),
            &[],
            minutes(now, 3),
        ),
        summary(
            "c9b8a7d6-0000-4000-8000-000000000005",
            "Simplify project onboarding",
            "Create a guided setup flow.",
            (Lifecycle::Defined, ExecutionState::None, Attention::Idle),
            &[],
            minutes(now, 60 * 26),
        ),
        summary(
            "c9b8a7d6-0000-4000-8000-000000000006",
            "Document the release checklist",
            "Write down the manual steps for a signed bundle.",
            (Lifecycle::Integrated, ExecutionState::None, Attention::Idle),
            &[],
            minutes(now, 60 * 72),
        ),
        summary(
            "c9b8a7d6-0000-4000-8000-000000000007",
            "Try a websocket transport",
            "Spike, abandoned in favour of the Unix socket.",
            (Lifecycle::Discarded, ExecutionState::None, Attention::Idle),
            &[],
            minutes(now, 60 * 24 * 6),
        ),
    ]
}

fn session(generation: u64) -> SessionInfo {
    SessionInfo {
        session_id: SessionId::new(SESSION),
        association_revision: 2,
        execution_generation: generation,
        model: ModelRef::new("lade-fixture", "scripted-preview"),
    }
}

fn verification(
    vid: &str,
    candidate: &str,
    argv: &[&str],
    (state, exit): (VerificationState, Option<u64>),
    (started, ms): (Timestamp, i64),
    output: &str,
    truncated: bool,
) -> VerificationSummary {
    VerificationSummary {
        verification_id: VerificationId::new(vid),
        objective_id: id(MAIN_OBJECTIVE),
        candidate_id: CandidateId::new(candidate),
        command: argv.iter().map(|a| (*a).to_owned()).collect(),
        state,
        exit_code: exit,
        started_at: started,
        ended_at: (state != VerificationState::Running).then_some(Timestamp(started.0 + ms)),
        output_tail: output.into(),
        truncated,
        provenance: Provenance::ServiceObserved,
    }
}

fn main_detail(now: Timestamp, summaries: &[ObjectiveSummary]) -> ObjectiveDetail {
    let files = fixture_files();
    let (added, removed) = files.iter().fold((0, 0), |(a, r), f| {
        let (fa, fr) = counts(
            f.old.as_deref().unwrap_or_default(),
            f.new.as_deref().unwrap_or_default(),
        );
        (a + fa, r + fr)
    });
    ObjectiveDetail {
        summary: summaries[0].clone(),
        intent: ObjectiveIntent {
            outcome: summaries[0].outcome.clone(),
            constraints: "Keep the public fetchWithRetry signature. No new runtime dependencies. Do not retry non-idempotent requests.".into(),
            acceptance: "A timeout raises TimeoutError, a confirmed failure raises NetworkError, both keep the original cause; tests cover both paths and pass.".into(),
        },
        intent_revision: 2,
        integration_target: target(),
        start_commit: commit("9a8b7c6d5e4f30211234567890abcdef12345678"),
        branch: "lade/7d0c2a3e".into(),
        session: Some(session(3)),
        candidates: vec![
            CandidateSummary {
                candidate_id: CandidateId::new(CANDIDATE_2),
                ordinal: 2,
                commit: commit("e1f2a3b4c5d6e7f80912a3b4c5d6e7f809123456"),
                base_target_commit: target().commit,
                status: CandidateStatus::Current,
                files: files.len() as u64,
                added,
                removed,
                created_at: minutes(now, 14),
            },
            CandidateSummary {
                candidate_id: CandidateId::new(CANDIDATE_1),
                ordinal: 1,
                commit: commit("0aa1bb2cc3dd4ee5ff60718293a4b5c6d7e8f901"),
                base_target_commit: target().commit,
                status: CandidateStatus::Historical,
                files: 3,
                added: 31,
                removed: 6,
                created_at: minutes(now, 58),
            },
        ],
        approval: None,
        verifications: vec![
            verification(
                "e0000001-0000-4000-8000-000000000001",
                CANDIDATE_2,
                &["git", "diff", "--check"],
                (VerificationState::Passed, Some(0)),
                (minutes(now, 13), 412),
                "",
                false,
            ),
            verification(
                "e0000002-0000-4000-8000-000000000002",
                CANDIDATE_2,
                &["npm", "test", "--", "--runInBand"],
                (VerificationState::Failed, Some(1)),
                (minutes(now, 13), 48_300),
                "  ● reports a timeout distinctly — “délai dépassé” is not a failure\n\n    expect(received).rejects.toBeInstanceOf(expected)\n\n    Expected constructor: TimeoutError\n    Received constructor: NetworkError\n\n      at Object.<anonymous> (tests/network/retry.test.ts:9:3)\n\nTest Suites: 1 failed, 3 passed, 4 total\nTests:       1 failed, 17 passed, 18 total",
                true,
            ),
            verification(
                "e0000003-0000-4000-8000-000000000003",
                CANDIDATE_1,
                &["npm", "test"],
                (VerificationState::Interrupted, None),
                (minutes(now, 57), 9_000),
                "Interrupted: the candidate was returned for revision while the check ran.",
                false,
            ),
        ],
        change_counts: ChangeCounts {
            files: files.len() as u64,
            added,
            removed,
        },
        unsent_drafts: 5,
        available_actions: vec![
            action(ActionName::ReviewFreeze, Some("Already frozen as Candidate 2")),
            action(ActionName::ReviewReturn, None),
            action(ActionName::ReviewVerify, None),
            action(ActionName::ReviewApprove, None),
            action(
                ActionName::ReviewIntegrate,
                Some("Requires a valid approval of Candidate 2 against main@4f2c9e1"),
            ),
            action(ActionName::ReviewRefresh, Some("main has not moved since Candidate 2 was frozen")),
            action(ActionName::ObjectiveComplete, Some("The Change Set is not empty")),
            action(ActionName::ObjectiveDiscard, None),
            action(
                ActionName::ObjectiveCleanup,
                Some("Available after integration, completion or discard"),
            ),
            action(ActionName::ExecutionCancel, Some("The agent is idle")),
            action(
                ActionName::MessageSend,
                Some("Return for revision before instructing the agent"),
            ),
            action(
                ActionName::FeedbackSubmit,
                Some("Return for revision before submitting feedback"),
            ),
        ],
    }
}

fn simple_detail(summary: &ObjectiveSummary, now: Timestamp) -> ObjectiveDetail {
    let running = summary.execution.state == ExecutionState::Running;
    let cancelled = summary.execution.state == ExecutionState::Cancelled;
    let defined = summary.lifecycle == Lifecycle::Defined;
    let archived = summary.lifecycle.is_archived();
    let mut actions = vec![action(
        ActionName::ObjectiveDiscard,
        archived.then_some("Already archived"),
    )];
    if defined {
        actions.push(action(ActionName::ExecutionStart, None));
        actions.push(action(ActionName::ObjectiveComplete, None));
    }
    if running {
        actions.push(action(ActionName::ExecutionCancel, None));
        actions.push(action(ActionName::MessageSend, None));
        actions.push(action(
            ActionName::ReviewFreeze,
            Some("Stop the agent before freezing"),
        ));
    }
    if cancelled {
        actions.push(action(ActionName::ExecutionResume, None));
        actions.push(action(ActionName::ReviewFreeze, None));
        actions.push(action(
            ActionName::MessageSend,
            Some("Resume the agent first — execution was interrupted"),
        ));
    }
    if summary.lifecycle == Lifecycle::Approved {
        actions.push(action(ActionName::ReviewIntegrate, None));
        actions.push(action(ActionName::ReviewReturn, None));
    }
    if archived {
        actions.push(action(ActionName::ObjectiveCleanup, None));
    }
    ObjectiveDetail {
        summary: summary.clone(),
        intent: ObjectiveIntent {
            outcome: summary.outcome.clone(),
            constraints: "No new dependencies.".into(),
            acceptance: "Existing tests pass; the change is described in the docs.".into(),
        },
        intent_revision: 1,
        integration_target: target(),
        start_commit: target().commit,
        branch: format!("lade/{}", &summary.objective_id.as_str()[..8]),
        session: (!defined).then(|| session(1)),
        candidates: if summary.lifecycle == Lifecycle::Approved {
            vec![CandidateSummary {
                candidate_id: CandidateId::new("b0000003-3333-4333-8333-333333333333"),
                ordinal: 1,
                commit: commit("3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f"),
                base_target_commit: target().commit,
                status: CandidateStatus::Current,
                files: 2,
                added: 40,
                removed: 11,
                created_at: minutes(now, 100),
            }]
        } else {
            Vec::new()
        },
        approval: (summary.lifecycle == Lifecycle::Approved).then(|| {
            super::model::ApprovalSummary {
                approval_id: ApprovalId::new("a0000003-0000-4000-8000-000000000003"),
                candidate_id: CandidateId::new("b0000003-3333-4333-8333-333333333333"),
                target_commit: target().commit,
                override_reason: None,
                state: super::model::ApprovalState::Valid,
            }
        }),
        verifications: Vec::new(),
        change_counts: ChangeCounts {
            files: if defined { 0 } else { 2 },
            added: if defined { 0 } else { 40 },
            removed: if defined { 0 } else { 11 },
        },
        unsent_drafts: 0,
        available_actions: actions,
    }
}

fn change_list(objective: &str, basis: Basis) -> ChangeList {
    let files = fixture_files();
    let changed: Vec<ChangedFile> = files
        .iter()
        .map(|f| {
            let (added, removed) = counts(
                f.old.as_deref().unwrap_or_default(),
                f.new.as_deref().unwrap_or_default(),
            );
            ChangedFile {
                path: RepoPath::utf8(f.path),
                old_path: None,
                status: f.status,
                added,
                removed,
                binary: f.binary,
            }
        })
        .collect();
    let (basis_label, capture_id, candidate_id) = match &basis {
        Basis::Working => (
            "Working inspection — capture f00dcafe".to_owned(),
            Some(CaptureId::new(CAPTURE)),
            None,
        ),
        Basis::Candidate { candidate_id } => (
            "Candidate 2 — frozen".to_owned(),
            None,
            Some(candidate_id.clone()),
        ),
        Basis::Since {
            to_candidate_id, ..
        } => (
            "Since previous review — Candidate 1 → Candidate 2".to_owned(),
            None,
            Some(to_candidate_id.clone()),
        ),
    };
    ChangeList {
        objective_id: id(objective),
        basis,
        basis_label,
        target: target(),
        capture_id,
        candidate_id,
        truncated: false,
        total_added: changed.iter().map(|f| f.added).sum(),
        total_removed: changed.iter().map(|f| f.removed).sum(),
        files: changed,
    }
}

fn activity(objective: &str, now: Timestamp) -> Vec<ActivityItem> {
    let item = |seq: u64,
                role: ActivityRole,
                title: &str,
                text: &str,
                tool: Option<&str>,
                error: bool,
                truncated: bool| ActivityItem {
        item_id: ActivityItemId::new(format!("{objective}-{seq}")),
        seq,
        at: minutes(now, 60 - seq as i64 * 4),
        role,
        title: title.into(),
        text: text.into(),
        tool_name: tool.map(str::to_owned),
        is_error: error,
        truncated,
    };
    vec![
        item(
            1,
            ActivityRole::User,
            "Prompt",
            "Harden network calls in src/network. A timeout must be distinguishable from a confirmed failure; keep the fetchWithRetry signature and add tests for both paths.",
            None,
            false,
            false,
        ),
        item(
            2,
            ActivityRole::Thinking,
            "Thinking",
            "The retry loop catches every error the same way. An AbortController per attempt lets a timeout surface as AbortError, which can be mapped to a dedicated TimeoutError while other failures become NetworkError.",
            None,
            false,
            false,
        ),
        item(
            3,
            ActivityRole::Tool,
            "read src/network/request.ts",
            "export async function fetchWithRetry(\n  url: string,\n  options: RequestOptions = {},\n): Promise<Response> {",
            Some("read"),
            false,
            false,
        ),
        item(
            4,
            ActivityRole::Tool,
            "edit src/network/request.ts",
            "Applied 3 replacements (+28 −6).",
            Some("edit"),
            false,
            false,
        ),
        item(
            5,
            ActivityRole::Tool,
            "bash npm test",
            "FAIL tests/network/retry.test.ts\n  ● reports a timeout distinctly\n    Received constructor: NetworkError",
            Some("bash"),
            true,
            false,
        ),
        item(
            6,
            ActivityRole::Assistant,
            "Agent",
            "I added TimeoutError and NetworkError in src/network/errors.ts and map AbortError to TimeoutError on the final attempt. One test still fails: the timeout is detected only when the abort happens before fetch resolves. I believe this is fixed — please verify with the test suite rather than relying on this message.",
            None,
            false,
            false,
        ),
        item(
            7,
            ActivityRole::Tool,
            "read fixtures/network/latency-samples.json",
            "[\n  { \"id\": 0, \"status\": 503, … (output truncated at 192 KiB; 1.4 MiB total)",
            Some("read"),
            false,
            true,
        ),
        item(
            8,
            ActivityRole::System,
            "Execution interrupted",
            "The service restarted; execution generation 2 was marked interrupted. Nothing resumes automatically — resume explicitly to continue.",
            None,
            true,
            false,
        ),
        item(
            9,
            ActivityRole::User,
            "Steer",
            "Keep the AbortController per attempt, but clear the timer in finally so it never fires after a response.",
            None,
            false,
            false,
        ),
        item(
            10,
            ActivityRole::Assistant,
            "Agent",
            "Understood. The timer is now cleared in finally; I froze nothing — that remains your decision.",
            None,
            false,
            false,
        ),
    ]
}

fn reference(
    path: &str,
    version: ReferenceVersion,
    text: &str,
    needle: &str,
    side: Side,
) -> Reference {
    let start = text.find(needle).unwrap_or(0);
    let line = text[..start].matches('\n').count() as u64 + 1;
    Reference {
        artifact_digest: digest(if side == Side::New { "new" } else { "old" }, path),
        path: RepoPath::utf8(path),
        version,
        side,
        byte_start: start as u64,
        byte_end: (start + needle.len()) as u64,
        selected_text: needle.into(),
        line_hint: Some(line),
    }
}

fn draft(n: u8, reference: Reference, body: &str, revision: u64, stale: bool) -> Draft {
    Draft {
        draft_id: DraftId::new(format!("d1000000-0000-4000-8000-00000000000{n}")),
        objective_id: id(MAIN_OBJECTIVE),
        destination_session_id: SessionId::new(SESSION),
        reference,
        body: body.into(),
        revision,
        state: DraftState::Unsent,
        stale,
        input_id: None,
    }
}

fn drafts() -> Vec<Draft> {
    let c2 = || ReferenceVersion::Candidate {
        candidate_id: CandidateId::new(CANDIDATE_2),
    };
    vec![
        draft(
            1,
            reference(
                "src/network/request.ts",
                c2(),
                REQUEST_NEW,
                "throw new NetworkError('Request failed', { cause: error });",
                Side::New,
            ),
            "Only wrap errors that are actually network failures; a 4xx response should surface unchanged so callers can handle validation errors.",
            3,
            false,
        ),
        draft(
            2,
            reference(
                "src/network/retry.ts",
                c2(),
                RETRY_NEW,
                "return status === 408 || status === 429 || status >= 500;",
                Side::New,
            ),
            "Retrying 429 should respect Retry-After instead of the jittered backoff.",
            1,
            false,
        ),
        draft(
            3,
            reference(
                "tests/network/retry.test.ts",
                c2(),
                TEST_NEW,
                "{ timeoutMs: 5 }",
                Side::New,
            ),
            "5 ms is likely to be flaky on a loaded CI runner; inject a fake timer instead.",
            2,
            false,
        ),
        draft(
            4,
            reference(
                "docs/reliability.md",
                c2(),
                DOC_NEW,
                "Both keep the original error as `cause`.",
                Side::New,
            ),
            "Mention that the cause chain is preserved for logging.",
            1,
            false,
        ),
        draft(
            5,
            reference(
                "src/network/request.ts",
                ReferenceVersion::Candidate {
                    candidate_id: CandidateId::new(CANDIDATE_1),
                },
                REQUEST_OLD,
                "await setTimeout(2 ** attempt * 100);",
                Side::Old,
            ),
            "Backoff should be capped; this grows without bound for large retry counts.",
            2,
            true,
        ),
    ]
}

fn inputs(now: Timestamp) -> Vec<InputSummary> {
    let input =
        |n: u8, mode: DeliveryMode, text: &str, at: i64, delivery: DeliveryState| InputSummary {
            input_id: InputId::new(format!("{n}e{n}c4a10-0000-4000-8000-00000000000{n}")),
            objective_id: id(MAIN_OBJECTIVE),
            blocked_reason: (delivery == DeliveryState::Blocked)
                .then(|| "execution generation 2 ended; resume explicitly".to_owned()),
            operation_id: OperationId::new(format!("20000000-0000-4000-8000-00000000000{n}")),
            mode,
            text_preview: text.into(),
            accepted_at: minutes(now, at),
            delivery,
        };
    vec![
        input(
            1,
            DeliveryMode::Prompt,
            "Harden network calls in src/network…",
            60,
            DeliveryState::Received,
        ),
        input(
            2,
            DeliveryMode::Steer,
            "Feedback (2 items) on Candidate 1: cap the backoff; keep the signature",
            52,
            DeliveryState::Received,
        ),
        input(
            3,
            DeliveryMode::FollowUp,
            "Keep the AbortController per attempt, but clear the timer…",
            24,
            DeliveryState::Uncertain,
        ),
        input(
            4,
            DeliveryMode::Steer,
            "Feedback (1 item) on working capture 9c1e: rename shouldRetry",
            20,
            DeliveryState::Blocked,
        ),
    ]
}

fn settings() -> Settings {
    Settings {
        default_model: Some(ModelRef::new("lade-fixture", "scripted-preview")),
        available_models: vec![ModelRef::new("lade-fixture", "scripted-preview")],
        verification_commands: vec![
            vec!["git".into(), "diff".into(), "--check".into()],
            vec![
                "npm".into(),
                "test".into(),
                "--".into(),
                "--runInBand".into(),
            ],
        ],
    }
}

fn workspace() -> Workspace {
    Workspace {
        workspace_id: WorkspaceId::new(WORKSPACE),
        path: RepoPath::utf8("/home/matias/src/lade-sample"),
        name: "lade-sample".into(),
        default_branch: Some("main".into()),
        branches: vec!["main".into(), "release/0.2".into(), "lade/7d0c2a3e".into()],
    }
}

/// Fixture as a sequence of ordinary store updates (what WP-G will produce from the service).
pub fn fixture_updates_at(now: Timestamp) -> Vec<StoreUpdate> {
    let objectives = summaries(now);
    let mut updates = vec![StoreUpdate::Snapshot {
        workspaces: vec![workspace()],
        objectives: objectives.clone(),
        settings: settings(),
    }];
    updates.push(StoreUpdate::ObjectiveDetail(Box::new(main_detail(
        now,
        &objectives,
    ))));
    for summary in &objectives[1..] {
        updates.push(StoreUpdate::ObjectiveDetail(Box::new(simple_detail(
            summary, now,
        ))));
    }
    updates.push(StoreUpdate::Drafts {
        objective_id: id(MAIN_OBJECTIVE),
        drafts: drafts(),
    });
    updates.push(StoreUpdate::Inputs {
        objective_id: id(MAIN_OBJECTIVE),
        inputs: inputs(now),
    });
    updates
}

pub fn fixture_updates() -> Vec<StoreUpdate> {
    fixture_updates_at(Timestamp(1_790_000_000_000))
}

/// Builds the preview state: fixture projections plus transient draft states for review.
pub fn preview_state(
    now: Timestamp,
    monotonic_ms: u64,
    variant: PreviewVariant,
) -> PresentationState {
    let mut state = PresentationState::new(ConnectionState::Connected);
    for update in fixture_updates_at(now) {
        state.apply(update);
    }
    state.preview = true;
    let main = id(MAIN_OBJECTIVE);
    state.ui.selected = Some(main.clone());
    let ui = state.objective_ui_mut(&main);
    ui.basis = Basis::Candidate {
        candidate_id: CandidateId::new(CANDIDATE_2),
    };
    ui.file = Some(RepoPath::utf8("src/network/request.ts"));
    state.changes.insert(
        main.clone(),
        super::model::Load::Loaded(change_list(
            MAIN_OBJECTIVE,
            Basis::Candidate {
                candidate_id: CandidateId::new(CANDIDATE_2),
            },
        )),
    );
    state.activity.insert(
        main.clone(),
        super::store::ActivityLog {
            items: activity(MAIN_OBJECTIVE, now),
            has_more: true,
            loaded: true,
        },
    );
    // Transient save states: unsaved local edit, a save in flight, a failed save.
    let editors: &mut Vec<DraftEditor> = state.drafts.entry(main).or_default();
    let t = monotonic_ms;
    if let Some(e) = editors.get_mut(1) {
        e.edit("Retrying 429 should respect Retry-After instead of the jittered backoff — and log the wait.", t);
    }
    if let Some(e) = editors.get_mut(2) {
        e.edit("5 ms is likely to be flaky on a loaded CI runner; inject a fake timer instead of real time.", t);
        e.poll(t + 400, true);
    }
    if let Some(e) = editors.get_mut(3) {
        e.edit(
            "Mention that the cause chain is preserved for structured logging.",
            t,
        );
        e.poll(t + 400, true);
        e.fail("storage full (ENOSPC); your text is kept here");
    }
    if variant == PreviewVariant::Disconnected {
        state.connection = ConnectionState::Disconnected {
            reason: "simulated loss of the service link".into(),
        };
        for e in state.drafts.values_mut().flatten() {
            e.connection_lost();
        }
    }
    state
}

/// Answers queries from the fixture; refuses commands with a visible notice.
pub struct PreviewResponder {
    store: Weak<Store>,
    now: Timestamp,
    files: HashMap<String, FixtureFile>,
}

impl PreviewResponder {
    pub fn new(store: Weak<Store>, now: Timestamp) -> Self {
        let files = fixture_files()
            .into_iter()
            .map(|f| (f.path.to_owned(), f))
            .collect();
        Self { store, now, files }
    }

    fn notice(message: String) -> StoreUpdate {
        StoreUpdate::Notice(ServiceNotice {
            level: NoticeLevel::Info,
            code: "preview".into(),
            message,
        })
    }

    fn respond(&self, intent: Intent) -> Vec<StoreUpdate> {
        let objectives = summaries(self.now);
        match intent {
            Intent::LoadObjective { objective_id } => {
                let detail = if objective_id.as_str() == MAIN_OBJECTIVE {
                    Some(main_detail(self.now, &objectives))
                } else {
                    objectives
                        .iter()
                        .find(|o| o.objective_id == objective_id)
                        .map(|s| simple_detail(s, self.now))
                };
                detail
                    .map(|d| StoreUpdate::ObjectiveDetail(Box::new(d)))
                    .into_iter()
                    .collect()
            }
            Intent::LoadChanges {
                objective_id,
                basis,
            } => {
                let defined = objectives
                    .iter()
                    .any(|o| o.objective_id == objective_id && o.lifecycle == Lifecycle::Defined);
                let mut list = change_list(objective_id.as_str(), basis);
                if defined {
                    list.files.clear();
                    list.total_added = 0;
                    list.total_removed = 0;
                    list.basis_label = "Working inspection — no worktree yet".into();
                }
                vec![StoreUpdate::Changes(list)]
            }
            Intent::LoadDiff {
                objective_id,
                basis,
                path,
            } => {
                let Some(file) = self.files.get(&path.display) else {
                    return Vec::new();
                };
                let hunks = if file.binary {
                    Vec::new()
                } else {
                    make_hunks(
                        file.old.as_deref().unwrap_or_default(),
                        file.new.as_deref().unwrap_or_default(),
                    )
                };
                vec![StoreUpdate::Diff(FileDiff {
                    objective_id,
                    basis,
                    path: path.clone(),
                    old_digest: file.old.as_ref().map(|_| digest("old", &path.display)),
                    new_digest: file
                        .new
                        .as_ref()
                        .map(|_| digest("new", &path.display))
                        .or_else(|| file.binary.then(|| digest("new", &path.display))),
                    binary: file.binary,
                    hunks,
                    truncated: false,
                })]
            }
            Intent::OpenFile {
                objective_id,
                basis,
                path,
                side,
                offset,
            } => {
                let Some(file) = self.files.get(&path.display) else {
                    return Vec::new();
                };
                let content = match side {
                    Side::Old => file.old.clone(),
                    Side::New => file.new.clone(),
                };
                let version_label = match &basis {
                    Basis::Working => "Working capture f00dcafe".to_owned(),
                    _ => "Candidate 2 — frozen".to_owned(),
                };
                if file.binary {
                    return vec![StoreUpdate::FilePage(FilePage {
                        objective_id,
                        basis,
                        path: path.clone(),
                        side,
                        artifact_digest: digest("new", &path.display),
                        total_bytes: 48_213,
                        offset: 0,
                        length: 48_213,
                        encoding: FileEncoding::Binary,
                        text: None,
                        eof: true,
                        version_label,
                    })];
                }
                let text = content.unwrap_or_default();
                let start = (offset as usize).min(text.len());
                let mut end = (start + PAGE_BYTES).min(text.len());
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                vec![StoreUpdate::FilePage(FilePage {
                    objective_id,
                    basis,
                    path: path.clone(),
                    side,
                    artifact_digest: digest(
                        if side == Side::New { "new" } else { "old" },
                        &path.display,
                    ),
                    total_bytes: text.len() as u64,
                    offset: start as u64,
                    length: (end - start) as u64,
                    encoding: FileEncoding::Utf8,
                    text: Some(text[start..end].to_owned()),
                    eof: end == text.len(),
                    version_label,
                })]
            }
            Intent::LoadActivity { objective_id, .. } => vec![StoreUpdate::ActivityPage {
                items: activity(objective_id.as_str(), self.now),
                objective_id,
                has_more: false,
            }],
            Intent::LoadDrafts { objective_id } => vec![StoreUpdate::Drafts {
                drafts: if objective_id.as_str() == MAIN_OBJECTIVE {
                    drafts()
                } else {
                    Vec::new()
                },
                objective_id,
            }],
            Intent::LoadInputs { objective_id } => vec![StoreUpdate::Inputs {
                inputs: if objective_id.as_str() == MAIN_OBJECTIVE {
                    inputs(self.now)
                } else {
                    Vec::new()
                },
                objective_id,
            }],
            Intent::Search {
                scope,
                query,
                objective_id,
                basis,
                ..
            } => vec![StoreUpdate::Search(self.search(
                scope,
                &query,
                objective_id,
                basis,
            ))],
            Intent::SaveDraft { draft_id, .. } => vec![StoreUpdate::DraftSaveFailed {
                draft_id,
                message: "Preview data — no service stores drafts".into(),
            }],
            other => vec![Self::notice(format!(
                "Preview data — “{}” was not sent: no service is connected",
                other.label()
            ))],
        }
    }

    fn search(
        &self,
        scope: SearchScope,
        query: &str,
        objective_id: Option<ObjectiveId>,
        basis: Option<Basis>,
    ) -> SearchResults {
        let needle = query.to_lowercase();
        let mut results = Vec::new();
        let scope_label;
        match scope {
            SearchScope::Objectives => {
                scope_label = "Objectives in lade-sample".to_owned();
                for o in summaries(self.now) {
                    if o.title.to_lowercase().contains(&needle)
                        || o.outcome.to_lowercase().contains(&needle)
                    {
                        results.push(SearchResult {
                            kind: SearchResultKind::Objective,
                            objective_id: Some(o.objective_id.clone()),
                            path: None,
                            line: None,
                            preview: o.title.clone(),
                            byte_start: None,
                        });
                    }
                }
            }
            SearchScope::Files | SearchScope::Content => {
                let version = match &basis {
                    Some(Basis::Candidate { .. }) => "Candidate 2 (frozen)",
                    Some(Basis::Since { .. }) => "Candidate 2 (frozen)",
                    _ => "working capture f00dcafe",
                };
                scope_label = format!(
                    "{} in {version} of Make retries safe",
                    if scope == SearchScope::Files {
                        "Files"
                    } else {
                        "Content"
                    }
                );
                let mut paths: Vec<&FixtureFile> = self.files.values().collect();
                paths.sort_by_key(|f| f.path);
                for file in paths {
                    if scope == SearchScope::Files {
                        if file.path.to_lowercase().contains(&needle) {
                            results.push(SearchResult {
                                kind: SearchResultKind::File,
                                objective_id: objective_id.clone(),
                                path: Some(RepoPath::utf8(file.path)),
                                line: None,
                                preview: file.path.into(),
                                byte_start: None,
                            });
                        }
                        continue;
                    }
                    let Some(text) = &file.new else { continue };
                    let mut start = 0u64;
                    for (n, line) in text.split_inclusive('\n').enumerate() {
                        if line.to_lowercase().contains(&needle) {
                            results.push(SearchResult {
                                kind: SearchResultKind::Content,
                                objective_id: objective_id.clone(),
                                path: Some(RepoPath::utf8(file.path)),
                                line: Some(n as u64 + 1),
                                preview: line.trim().to_owned(),
                                byte_start: Some(start),
                            });
                        }
                        start += line.len() as u64;
                    }
                }
            }
        }
        let truncated = results.len() > 50;
        results.truncate(50);
        SearchResults {
            scope,
            query: query.to_owned(),
            scope_label: format!("Preview data — {scope_label}"),
            basis,
            objective_id,
            results,
            truncated,
        }
    }
}

impl IntentSink for PreviewResponder {
    fn submit(&self, intent: Intent) {
        let Some(store) = self.store.upgrade() else {
            return;
        };
        let updates = self.respond(intent);
        store.apply_all(updates);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_hunks_carry_exact_byte_offsets() {
        for file in fixture_files().into_iter().filter(|f| !f.binary) {
            let old = file.old.unwrap_or_default();
            let new = file.new.unwrap_or_default();
            for hunk in make_hunks(&old, &new) {
                for line in hunk.lines {
                    if let Some(start) = line.new_byte_start {
                        assert!(
                            new[start as usize..].starts_with(&line.text),
                            "{}",
                            file.path
                        );
                    }
                    if let Some(start) = line.old_byte_start {
                        assert!(
                            old[start as usize..].starts_with(&line.text),
                            "{}",
                            file.path
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn preview_state_is_labelled_and_has_every_draft_state() {
        let state = preview_state(
            Timestamp(1_790_000_000_000),
            10_000,
            PreviewVariant::Standard,
        );
        assert!(state.preview);
        let statuses: Vec<String> = state
            .drafts
            .values()
            .flatten()
            .map(|d| d.status(true).label())
            .collect();
        assert!(statuses.iter().any(|s| s.starts_with("Saved r")));
        assert!(statuses.iter().any(|s| s == "Unsaved"));
        assert!(statuses.iter().any(|s| s == "Saving…"));
        assert!(statuses.iter().any(|s| s.starts_with("Save failed")));
        assert!(state.drafts.values().flatten().any(|d| d.stale));
    }
}
