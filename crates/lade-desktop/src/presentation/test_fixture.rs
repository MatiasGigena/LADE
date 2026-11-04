use super::state::model::{
    ActionName, ApprovalId, ArtifactDigest, Attention, AvailableAction, Badge, CandidateId,
    CandidateStatus, CandidateSummary, ChangeCounts, DeliveryMode, DeliveryState, Draft, DraftId,
    DraftState, Execution, ExecutionState, GitId, InputId, InputSummary, Intent as ObjectiveIntent,
    Lifecycle, ModelRef, ObjectiveDetail, ObjectiveId, ObjectiveSummary, OperationId, Provenance,
    Reference, ReferenceVersion, RepoPath, SessionId, SessionInfo, Settings, Side, TargetRef,
    Timestamp, VerificationId, VerificationState, VerificationSummary, Workspace, WorkspaceId,
};
use super::state::store::StoreUpdate;

pub const MAIN_OBJECTIVE: &str = "7d0c2a3e-5b1f-4c8e-9a62-0f4f1b7c2d11";
pub const SECOND_OBJECTIVE: &str = "a41f9c07-2e6d-4b3a-8f15-6c9e0d2b7a22";
pub const SAVED_DRAFT: &str = "d1000000-0000-4000-8000-000000000001";
const WORKSPACE: &str = "5e3b1d2c-0a9f-4e7d-b6c5-1f2e3d4c5b6a";
const SESSION: &str = "c3d4e5f6-0718-4293-a4b5-c6d7e8f90a1b";
const CANDIDATE_1: &str = "b0000001-1111-4111-8111-111111111111";
const CANDIDATE_2: &str = "b0000002-2222-4222-8222-222222222222";

fn model() -> ModelRef {
    ModelRef::new("test-provider", "test-model")
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

fn digest(side: &str, path: &str) -> ArtifactDigest {
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
        model: model(),
        thinking: None,
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
    let (files, added, removed) = (6, 58, 14);
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
                files,
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
            files,
            added,
            removed,
        },
        unsent_drafts: 5,
        terminal_open: Some(super::state::model::TerminalOpenAction {
            enabled: true,
            reason: None,
        }),
        history_deleted: false,
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
            super::state::model::ApprovalSummary {
                approval_id: ApprovalId::new("a0000003-0000-4000-8000-000000000003"),
                candidate_id: CandidateId::new("b0000003-3333-4333-8333-333333333333"),
                target_commit: target().commit,
                override_reason: None,
                state: super::state::model::ApprovalState::Valid,
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
        terminal_open: Some(super::state::model::TerminalOpenAction {
            enabled: !summary.lifecycle.is_archived(),
            reason: summary.lifecycle.is_archived().then(|| {
                "Terminals open only while the Objective is in progress or under review.".to_owned()
            }),
        }),
        history_deleted: false,
    }
}

fn reference(path: &str, version: ReferenceVersion, needle: &str, side: Side) -> Reference {
    Reference {
        artifact_digest: digest(if side == Side::New { "new" } else { "old" }, path),
        path: RepoPath::utf8(path),
        version,
        side,
        byte_start: 0,
        byte_end: needle.len() as u64,
        selected_text: needle.into(),
        line_hint: Some(1),
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
        default_model: Some(model()),
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
        path: RepoPath::utf8("/tmp/lade-sample"),
        name: "lade-sample".into(),
        default_branch: Some("main".into()),
        branches: vec!["main".into(), "release/0.2".into(), "lade/7d0c2a3e".into()],
    }
}

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
    updates.extend(agent_session_fixture());
    updates
}

fn agent_session_fixture() -> Vec<StoreUpdate> {
    use super::state::agent_session::{
        AgentSessionUpdate, Catalog, Inventory, ModelView, ProviderStatus, ProviderView,
        ResourceKind, ResourceState, ResourceView, SessionView, ThinkingLevel,
    };
    let provider =
        |id: &str, name: &str, status, reason: Option<&str>, auth: &[&str]| ProviderView {
            id: id.into(),
            name: name.into(),
            auth: auth.iter().map(|a| (*a).to_owned()).collect(),
            status,
            reason: reason.map(str::to_owned),
            variables: Vec::new(),
            models: 1,
        };
    vec![
        StoreUpdate::AgentSession(AgentSessionUpdate::Catalog(Catalog {
            sdk_version: "1.5.0".into(),
            providers: vec![
                provider(
                    "test-provider",
                    "Test provider",
                    ProviderStatus::Available,
                    None,
                    &["no credentials needed"],
                ),
                provider(
                    "anthropic",
                    "Anthropic",
                    ProviderStatus::MissingCredentials,
                    Some("Add an API key or subscription token for Anthropic to use it."),
                    &["API key", "subscription token", "OAuth login"],
                ),
                provider(
                    "openai-codex",
                    "OpenAI (ChatGPT Plus/Pro)",
                    ProviderStatus::Restricted,
                    Some("interactive OAuth login and refresh-token storage are restricted: LADE has no credential custody for them"),
                    &["OAuth login"],
                ),
            ],
            error: None,
        })),
        StoreUpdate::AgentSession(AgentSessionUpdate::Models {
            provider: "test-provider".into(),
            models: vec![ModelView {
                provider: "test-provider".into(),
                id: "test-model".into(),
                name: "Test model".into(),
                reasoning: false,
                thinking: vec![ThinkingLevel::Off],
            }],
        }),
        StoreUpdate::AgentSession(AgentSessionUpdate::Session {
            objective_id: id(MAIN_OBJECTIVE),
            session: SessionView {
                generation: 2,
                reason: "reloaded".into(),
                model: model(),
                thinking: ThinkingLevel::Off,
                association_revision: Some(2),
                pi_session_id: Some("01a12924-dd29-706e-adf1-5c643ac92a8a".into()),
                leaf_id: None,
            },
        }),
        StoreUpdate::AgentSession(AgentSessionUpdate::Inventory(Inventory {
            profile_dir: "~/.local/state/lade/profile".into(),
            entries: vec![
                ResourceView {
                    kind: ResourceKind::Context,
                    name: "AGENTS.md".into(),
                    state: ResourceState::Active,
                    sha256: None,
                    reason: None,
                },
                ResourceView {
                    kind: ResourceKind::Extension,
                    name: "ask.js".into(),
                    state: ResourceState::PendingApproval,
                    sha256: Some("4554c00dbb28cad89d67afb301d893073eefca19bee50292764c7b8c3dc8bf03".into()),
                    reason: Some("new or changed executable extension: approve this exact content, then reload the session".into()),
                },
            ],
            restrictions: vec![
                "Repository .pi/ resources are never loaded (project trust is \"never\").".into(),
            ],
        })),
    ]
}

pub fn fixture_updates() -> Vec<StoreUpdate> {
    fixture_updates_at(Timestamp(1_790_000_000_000))
}
