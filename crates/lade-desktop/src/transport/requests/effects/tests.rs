use lade_protocol::client::{self as wire, ReplyData};
use lade_protocol::types::{Counter, Digest, Id};

use crate::presentation::state::agent_session::AgentSessionUpdate;
use crate::presentation::state::model::{ModelRef, Refusal};
use crate::transport::convert;
use crate::transport::requests::tests::*;
use crate::transport::requests::*;

#[test]
fn query_replies_carry_their_request_context() {
    let path = RepoPath::utf8("hello.txt");
    let context = Context::Diff(oid(), Basis::Working, path.clone());
    let effects = reply_effects(
        &context,
        &ReplyData::DiffGet(wire::FileDiff {
            binary: false,
            hunks: vec![wire::DiffHunk {
                old_start: Counter::new(0),
                old_lines: Counter::new(0),
                new_start: Counter::new(1),
                new_lines: Counter::new(1),
                lines: vec![wire::DiffLine {
                    no_newline: true,
                    kind: wire::DiffLineKind::Add,
                    text: "hi".into(),
                    old_no: None,
                    new_no: Some(Counter::new(1)),
                    old_byte_start: None,
                    new_byte_start: Some(Counter::new(0)),
                }],
            }],
            truncated: false,
            old_digest: None,
            new_digest: Some(Digest::from_bytes([7; 32])),
        }),
    );
    let [StoreUpdate::Diff(diff)] = effects.updates.as_slice() else {
        panic!("{effects:?}")
    };
    assert_eq!(diff.path, path);
    assert_eq!(diff.basis, Basis::Working);
    assert_eq!(diff.hunks[0].lines[0].kind, view::DiffLineKind::Add);
    assert_eq!(diff.hunks[0].lines[0].new_byte_start, Some(0));
    assert!(diff.hunks[0].lines[0].no_newline);

    let page = reply_effects(
        &Context::File(oid(), Basis::Working, path.clone(), Side::New),
        &ReplyData::FileRead(wire::FileContent {
            artifact_digest: Digest::from_bytes([1; 32]),
            total_bytes: Counter::new(3),
            offset: Counter::new(0),
            length: Counter::new(3),
            encoding: wire::FileEncoding::Utf8,
            eof: true,
            version_label: "Working capture".into(),
            text: Some("hi\n".into()),
            chunked: None,
        }),
    );
    assert!(matches!(
        page.updates.as_slice(),
        [StoreUpdate::FilePage(p)] if p.side == Side::New && p.eof && p.path == path
    ));
}

#[test]
fn objective_create_selects_and_loads_the_new_objective() {
    let effects = reply_effects(
        &Context::command("Create Objective", None, None),
        &ReplyData::ObjectiveCreate(Box::new(wire_detail(O))),
    );
    assert!(
        effects
            .updates
            .iter()
            .any(|u| matches!(u, StoreUpdate::Select(o) if o == &oid()))
    );
    assert!(effects.follow_ups.contains(&Intent::LoadChanges {
        objective_id: oid(),
        basis: Basis::Working
    }));
}

/// The context `request` builds for `intent`.

#[test]
fn settings_offer_known_models_with_the_default_selected() {
    let settings = convert::settings(&wire::Settings {
        verification_commands: vec![vec!["git".into(), "diff".into(), "--check".into()]],
        default_model: Some(lade_protocol::types::Model {
            provider: "lade-fixture".into(),
            id: "scripted".into(),
        }),
    });
    assert_eq!(
        settings.default_model,
        Some(ModelRef::new("lade-fixture", "scripted"))
    );
}

/// A catalogue with one restricted provider and the fixture provider's scripted model.
fn fixture_catalog() -> ReplyData {
    ReplyData::ModelsList(wire::ModelCatalog {
        sdk_version: "1.5.0".into(),
        providers: vec![wire::ProviderInfo {
            provider: "openai-codex".into(),
            name: "OpenAI Codex".into(),
            auth: vec![wire::ProviderAuth::OauthLogin],
            status: wire::ProviderStatus::Restricted,
            models: Counter::new(8),
            reason: Some("interactive OAuth login is restricted".into()),
            variables: None,
        }],
        models: vec![wire::ModelInfo {
            provider: "lade-fixture".into(),
            id: "scripted".into(),
            name: "Scripted".into(),
            reasoning: false,
            thinking_levels: vec![lade_protocol::types::ThinkingLevel::Off],
        }],
        truncated: false,
        error: None,
    })
}

#[test]
fn agent_session_replies_and_events_reach_the_store() {
    let models = reply_effects(
        &Context::Models(Some("lade-fixture".into())),
        &fixture_catalog(),
    );
    assert!(matches!(
        models.updates.as_slice(),
        [
            StoreUpdate::AgentSession(AgentSessionUpdate::Catalog(c)),
            StoreUpdate::AgentSession(AgentSessionUpdate::Models { provider, models }),
        ] if c.providers[0].status == crate::presentation::state::agent_session::ProviderStatus::Restricted
            && c.providers[0].reason.is_some()
            && provider == "lade-fixture"
            && models.len() == 1
    ));
    let state = wire::SessionState {
        objective_id: Id::parse(O).unwrap(),
        generation: Counter::new(2),
        reason: lade_protocol::types::SessionChangeReason::Fork,
        model: lade_protocol::types::Model {
            provider: "lade-fixture".into(),
            id: "scripted".into(),
        },
        thinking: lade_protocol::types::ThinkingLevel::Off,
        session_id: None,
        association_revision: Some(Counter::new(2)),
        pi_session_id: Some("pi".into()),
        leaf_id: None,
    };
    let forked = reply_effects(
        &Context::command("Fork session", Some(&oid()), Some(ActionSurface::Session)),
        &ReplyData::SessionFork(state.clone()),
    );
    assert!(forked.updates.iter().any(|u| matches!(
        u,
        StoreUpdate::AgentSession(AgentSessionUpdate::Session { session, .. }) if session.generation == 2
    )));
    assert!(
        forked
            .follow_ups
            .iter()
            .any(|i| matches!(i, Intent::LoadSessionTree { .. }))
    );
    let changed = crate::transport::core::event_updates(wire::Event::SessionChanged(state));
    assert!(matches!(
        changed.as_slice(),
        [StoreUpdate::AgentSession(
            AgentSessionUpdate::Session { .. }
        )]
    ));
    let opened = crate::transport::core::event_updates(wire::Event::UiRequest(wire::UiRequest {
        objective_id: Id::parse(O).unwrap(),
        ui_id: Id::parse(D).unwrap(),
        generation: Counter::new(2),
        prompt: lade_protocol::types::UiPrompt::Confirm {
            title: "Proceed?".into(),
            message: "m".into(),
        },
    }));
    assert!(matches!(
        opened.as_slice(),
        [StoreUpdate::AgentSession(AgentSessionUpdate::DialogOpened(d))] if d.generation == 2
    ));
    let snapshot = reply_effects(
        &Context::Snapshot,
        &ReplyData::Snapshot(wire::SnapshotData {
            service_epoch: Id::parse(O).unwrap(),
            sequence: Counter::new(0),
            workspaces: Vec::new(),
            objectives: Vec::new(),
            settings: wire::Settings {
                verification_commands: Vec::new(),
                default_model: None,
            },
        }),
    );
    assert!(
        snapshot
            .follow_ups
            .contains(&Intent::LoadModels { provider: None })
    );
    assert!(snapshot.follow_ups.contains(&Intent::LoadResources));
}

#[test]
fn terminal_replies_and_refusals_land_in_the_terminal_pane() {
    use crate::presentation::terminal::{TerminalEnd, TerminalMode as ViewMode};
    let context = Context::Terminal(oid(), "Open terminal");
    let effects = reply_effects(
        &context,
        &ReplyData::TerminalOpen(wire::TerminalInfo {
            terminal_id: Id::parse(T.to_owned()).unwrap(),
            objective_id: Id::parse(O.to_owned()).unwrap(),
            mode: wire::TerminalMode::ReadOnly,
            cols: Counter::new(80),
            rows: Counter::new(24),
        }),
    );
    assert_eq!(
        effects.updates,
        vec![StoreUpdate::TerminalOpened {
            objective_id: oid(),
            terminal_id: T.into(),
            mode: ViewMode::ReadOnly,
            cols: 80,
            rows: 24,
        }]
    );
    let refused = rejection_effects(
        &context,
        &wire::Rejection::new(
            wire::ErrorCode::InvalidLifecycle,
            wire::SafeAction::None,
            "Start the agent once to create this Objective's worktree.",
        ),
    );
    assert_eq!(
        refused.updates,
        vec![StoreUpdate::TerminalRefused {
            objective_id: oid(),
            refusal: Refusal {
                code: "invalid_lifecycle",
                message: "Couldn't open terminal. Start the agent once to create this Objective's worktree.".into(),
                details: "invalid_lifecycle: Start the agent once to create this Objective's worktree.".into(),
            },
        }]
    );
    let stopped = reply_effects(&Context::Stop, &ReplyData::ServiceStop(wire::Empty {}));
    assert_eq!(stopped.updates, vec![StoreUpdate::ServiceStopped]);
    let closed =
        crate::transport::core::event_updates(wire::Event::TerminalClosed(wire::TerminalClosed {
            objective_id: Id::parse(O.to_owned()).unwrap(),
            terminal_id: Id::parse(T.to_owned()).unwrap(),
            reason: wire::TerminalCloseReason::ReviewFreeze,
            exit_code: None,
            signal: None,
        }));
    assert_eq!(
        closed,
        vec![StoreUpdate::TerminalClosed {
            terminal_id: T.into(),
            end: TerminalEnd::ReviewFreeze,
        }]
    );
}

fn command(label: &'static str, saved_draft: Option<DraftId>, batch: Vec<DraftId>) -> Context {
    Context::Command {
        label,
        objective_id: Some(oid()),
        saved_draft,
        batch,
        surface: Some(ActionSurface::Drafts),
    }
}

#[test]
fn a_successful_command_clears_its_status_and_its_surfaces_refusal() {
    let effects = reply_effects(
        &command("Save draft", None, Vec::new()),
        &ReplyData::DraftSave(wire_draft(1)),
    );
    assert_eq!(
        effects.updates[..2],
        [
            StoreUpdate::CommandStatus {
                objective_id: oid(),
                status: None,
            },
            StoreUpdate::RefusalCleared {
                objective_id: oid(),
                surface: ActionSurface::Drafts,
            },
        ]
    );
    assert!(matches!(effects.updates[2], StoreUpdate::DraftChanged(_)));
}

#[test]
fn recovered_outcomes_leave_draft_and_input_records_to_current_state() {
    let save = command("Save draft", None, Vec::new());
    let draft = ReplyData::DraftSave(wire_draft(1));
    let saved = recovered_effects(&save, &draft).expect("a saved draft is historical");
    assert!(
        !saved
            .updates
            .iter()
            .any(|u| matches!(u, StoreUpdate::DraftChanged(_)))
    );
    assert_eq!(
        saved.follow_ups,
        [Intent::LoadDrafts {
            objective_id: oid()
        }]
    );
    let send = command("Send message", None, Vec::new());
    let accepted = ReplyData::MessageSend(input_summary(&test_id(5)));
    let recovered = recovered_effects(&send, &accepted).expect("an input is historical");
    assert!(
        !recovered
            .updates
            .iter()
            .any(|u| matches!(u, StoreUpdate::InputChanged { .. }))
    );
    assert_eq!(
        recovered.follow_ups,
        [Intent::LoadInputs {
            objective_id: oid()
        }]
    );
}

#[test]
fn recovered_deletions_and_objective_details_apply_like_current_replies() {
    let delete = Context::DraftDelete("Delete draft", DraftId::new(D));
    let deleted = ReplyData::DraftDelete(wire::Empty {});
    assert_eq!(recovered_effects(&delete, &deleted), None);
    assert!(
        reply_effects(&delete, &deleted)
            .updates
            .contains(&StoreUpdate::DraftDeleted {
                draft_id: DraftId::new(D)
            })
    );
    let start = command("Start agent", None, Vec::new());
    let started = ReplyData::ExecutionStart(Box::new(wire_detail(O)));
    assert_eq!(recovered_effects(&start, &started), None);
}

#[test]
fn a_reply_that_does_not_match_its_context_changes_nothing() {
    let page = ReplyData::DraftsList(wire::DraftList {
        drafts: vec![wire_draft(1)],
        next_after: None,
    });
    assert_eq!(
        reply_effects(&Context::Inputs(oid()), &page),
        Effects::default()
    );
}

#[test]
fn accepted_feedback_reloads_the_drafts_it_consumed() {
    let submit = command("Submit feedback", None, vec![DraftId::new(D)]);
    let effects = reply_effects(
        &submit,
        &ReplyData::FeedbackSubmit(input_summary(&test_id(5))),
    );
    assert!(
        effects
            .updates
            .iter()
            .any(|u| matches!(u, StoreUpdate::InputChanged { .. }))
    );
    assert_eq!(
        effects.follow_ups,
        [Intent::LoadDrafts {
            objective_id: oid()
        }],
        "the Objective arrives with the objective.detail event that follows"
    );
}

#[test]
fn an_oversized_terminal_size_is_clamped_and_a_stop_is_reported() {
    let info = wire::TerminalInfo {
        terminal_id: Id::parse(T).unwrap(),
        objective_id: Id::parse(O).unwrap(),
        mode: wire::TerminalMode::ReadOnly,
        cols: Counter::new(70_000),
        rows: Counter::new(40),
    };
    let effects = reply_effects(
        &Context::Terminal(oid(), "Open terminal"),
        &ReplyData::TerminalOpen(info),
    );
    assert!(matches!(
        effects.updates.as_slice(),
        [StoreUpdate::TerminalOpened {
            cols: u16::MAX,
            rows: 40,
            mode: crate::presentation::terminal::TerminalMode::ReadOnly,
            ..
        }]
    ));
    let effects = reply_effects(&Context::Stop, &ReplyData::ServiceStop(wire::Empty {}));
    assert_eq!(effects.updates, [StoreUpdate::ServiceStopped]);
}
