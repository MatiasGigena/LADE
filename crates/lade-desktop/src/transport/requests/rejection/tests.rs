use std::path::PathBuf;

use lade_protocol::client::{ErrorCode, Rejection, ReplyData, SafeAction};
use lade_protocol::types::Id;

use crate::presentation::state::model::{ModelRef, NoticeScope, Refusal};
use crate::transport::convert;
use crate::transport::requests::rejection::refusal;
use crate::transport::requests::tests::*;
use crate::transport::requests::*;

#[test]
fn command_success_clears_and_rejection_reaches_only_its_objective_and_surface() {
    let context = context_of(&Intent::Freeze {
        objective_id: oid(),
    });
    let rejection = Rejection::new(
        ErrorCode::InvalidLifecycle,
        SafeAction::Refresh,
        "Wait until the agent has stopped.",
    );
    let effects = rejection_effects(&context, &rejection);
    let message = "Couldn't freeze for review. Wait until the agent has stopped.";
    assert!(effects.updates.contains(&StoreUpdate::Refused {
        objective_id: oid(),
        surface: ActionSurface::Review,
        refusal: Refusal {
            code: "invalid_lifecycle",
            message: message.into(),
            details: "invalid_lifecycle: Wait until the agent has stopped.".into(),
        },
    }));
    // Shown on its surface only: no copy in the Objective's command note, no global notice.
    assert!(effects.updates.contains(&StoreUpdate::CommandStatus {
        objective_id: oid(),
        status: None,
    }));
    assert!(
        !effects
            .updates
            .iter()
            .any(|u| matches!(u, StoreUpdate::Notice(_)))
    );
    assert!(effects.follow_ups.contains(&Intent::LoadObjective {
        objective_id: oid()
    }));
    let ok = reply_effects(
        &context,
        &ReplyData::ReviewFreeze(wire_detail(O).candidates[0].clone()),
    );
    assert!(ok.updates.contains(&StoreUpdate::CommandStatus {
        objective_id: oid(),
        status: None
    }));
    assert!(ok.updates.contains(&StoreUpdate::RefusalCleared {
        objective_id: oid(),
        surface: ActionSurface::Review,
    }));
}

#[test]
fn refusals_are_plain_sentences_with_the_code_kept_apart() {
    // A precondition: no code, no "Refresh" advice (refreshing would not help).
    let terminal = refusal(
        "open terminal",
        &Rejection::new(
            ErrorCode::InvalidLifecycle,
            SafeAction::Refresh,
            "Start the agent once to create this Objective's worktree.",
        ),
    );
    assert_eq!(
        terminal.message,
        "Couldn't open terminal. Start the agent once to create this Objective's worktree."
    );
    assert_eq!(
        terminal.details,
        "invalid_lifecycle: Start the agent once to create this Objective's worktree."
    );
    // Stale state: refreshing helps, and says so once.
    let stale = refusal(
        "approve",
        &Rejection::new(ErrorCode::StaleTarget, SafeAction::Refresh, ""),
    );
    assert_eq!(
        stale.message,
        "Couldn't approve. The Integration Target moved. Refresh and try again."
    );
    assert_eq!(stale.details, "stale_target");
    let worded = refusal(
        "edit intent",
        &Rejection::new(
            ErrorCode::StaleAssociation,
            SafeAction::Refresh,
            "The Agent Session changed. Refresh and try again.",
        ),
    );
    assert_eq!(
        worded.message,
        "Couldn't edit intent. The Agent Session changed. Refresh and try again."
    );
    // No detail falls back to the code's wording; a lowercase service detail becomes a sentence.
    let bare = refusal(
        "start agent",
        &Rejection::new(
            ErrorCode::ExecutionUnavailable,
            SafeAction::ExplicitContinue,
            "",
        ),
    );
    assert_eq!(
        bare.message,
        "Couldn't start agent. The agent isn't running."
    );
    let lower = refusal(
        "send terminal input",
        &Rejection::new(
            ErrorCode::NotFound,
            SafeAction::Refresh,
            "terminal not found",
        ),
    );
    assert_eq!(
        lower.message,
        "Couldn't send terminal input. Terminal not found."
    );
    assert_eq!(
        rejection_message("check the outcome of “Approve”", &stale_rejection()),
        "Couldn't check the outcome of “Approve”. The Integration Target moved. Refresh and try again."
    );
    // Every code reads without its spelling.
    for code in [
        ErrorCode::Unauthorized,
        ErrorCode::DurabilityFailure,
        ErrorCode::LimitReached,
        ErrorCode::Conflict,
        ErrorCode::VersionMismatch,
    ] {
        let message = rejection_message("approve", &Rejection::new(code, SafeAction::None, ""));
        assert!(!message.contains(convert::error_code(code)), "{message}");
        assert!(!message.contains('_'), "{message}");
    }
}

fn stale_rejection() -> Rejection {
    Rejection::new(ErrorCode::StaleTarget, SafeAction::Refresh, "")
}

#[test]
fn commands_without_an_objective_still_use_the_notice() {
    let effects = rejection_effects(
        &context_of(&Intent::UpdateSettings {
            default_model: ModelRef::new("lade-fixture", "scripted"),
        }),
        &Rejection::new(ErrorCode::InvalidRequest, SafeAction::None, "unknown model"),
    );
    assert!(matches!(
        effects.updates.as_slice(),
        [StoreUpdate::Notice(n)] if n.message == "Couldn't save default model. Unknown model."
    ));
}

#[test]
fn a_failed_workspace_open_is_scoped_and_explains_the_folder() {
    let effects = rejection_effects(
        &context_of(&Intent::OpenWorkspace {
            path: PathBuf::from("/tmp/repo/.git"),
        }),
        &Rejection::new(
            ErrorCode::InvalidRequest,
            SafeAction::None,
            "not a repository",
        ),
    );
    let [StoreUpdate::Notice(notice)] = effects.updates.as_slice() else {
        panic!("one notice: {:?}", effects.updates);
    };
    assert_eq!(notice.scope, NoticeScope::WorkspaceOpen);
    assert_eq!(notice.code, "invalid_request");
    assert!(
        notice
            .message
            .starts_with("Couldn't open workspace. Not a repository.")
    );
    assert!(notice.message.ends_with("It needs at least one commit."));
    let other = rejection_effects(
        &context_of(&Intent::LoadResources),
        &Rejection::new(ErrorCode::InvalidRequest, SafeAction::None, "bad"),
    );
    assert!(matches!(
        other.updates.as_slice(),
        [StoreUpdate::Notice(n)] if n.scope == NoticeScope::General
    ));
}

#[test]
fn stale_reference_names_the_stale_drafts() {
    let context = Context::Command {
        label: "Submit feedback",
        objective_id: Some(oid()),
        saved_draft: None,
        batch: vec![DraftId::new(D), DraftId::new(C)],
        surface: Some(ActionSurface::Feedback),
    };
    let rejection = Rejection::new(
        ErrorCode::StaleReference,
        SafeAction::Refresh,
        "a feedback draft refers to code that changed",
    )
    .with_stale_drafts(vec![Id::parse(D).unwrap()]);
    let effects = rejection_effects(&context, &rejection);
    let message = effects
        .updates
        .iter()
        .find_map(|u| match u {
            StoreUpdate::Refused {
                surface: ActionSurface::Feedback,
                refusal,
                ..
            } => Some(refusal.message.clone()),
            _ => None,
        })
        .unwrap();
    assert!(message.contains("Stale draft: 3b1c2d3e."), "{message}");
    assert!(
        !message.contains(D),
        "draft ids stay in the details: {message}"
    );
    assert!(effects.follow_ups.contains(&Intent::LoadDrafts {
        objective_id: oid()
    }));
}

#[test]
fn refused_draft_save_keeps_text_and_reports_beside_the_draft() {
    let context = Context::Command {
        label: "Save draft",
        objective_id: Some(oid()),
        saved_draft: Some(DraftId::new(D)),
        batch: Vec::new(),
        surface: Some(ActionSurface::Drafts),
    };
    let effects = rejection_effects(
        &context,
        &Rejection::new(
            ErrorCode::DurabilityFailure,
            SafeAction::QueryOperation,
            "The draft was not written (injected test fault).",
        ),
    );
    // Shown once, beside the draft: no Objective status, no notice.
    assert_eq!(
        effects.updates,
        vec![StoreUpdate::DraftSaveRefused {
            draft_id: DraftId::new(D),
            refusal: Refusal {
                code: "durability_failure",
                message: "The draft was not written (injected test fault).".into(),
                details: "durability_failure: The draft was not written (injected test fault)."
                    .into(),
            },
        }]
    );
    let deleted = rejection_effects(
        &context_of(&Intent::DeleteDraft {
            draft_id: DraftId::new(D),
            expected_revision: 2,
        }),
        &Rejection::new(
            ErrorCode::Conflict,
            SafeAction::Refresh,
            "The draft was changed elsewhere. Refresh and try again.",
        ),
    );
    assert_eq!(
        deleted.updates,
        vec![StoreUpdate::DraftDeleteRefused {
            draft_id: DraftId::new(D),
            refusal: Refusal {
                code: "conflict",
                message:
                    "Couldn't delete draft. The draft was changed elsewhere. Refresh and try again."
                        .into(),
                details: "conflict: The draft was changed elsewhere. Refresh and try again.".into(),
            },
        }]
    );
}

fn rejection(code: ErrorCode, detail: &str) -> Rejection {
    Rejection::new(code, SafeAction::None, detail)
}

#[test]
fn a_rejection_without_detail_falls_back_to_the_codes_reason() {
    let refused = refusal("approve", &rejection(ErrorCode::InvalidLifecycle, ""));
    assert!(
        refused.message.starts_with("Couldn't approve. "),
        "{}",
        refused.message
    );
    assert!(refused.message.len() > "Couldn't approve. ".len());
    assert_eq!(refused.details, "invalid_lifecycle");
    let detailed = refusal(
        "approve",
        &rejection(ErrorCode::InvalidLifecycle, "the candidate moved"),
    );
    assert_eq!(detailed.details, "invalid_lifecycle: the candidate moved");
}

#[test]
fn view_queries_fail_on_the_view_that_asked() {
    let refused = rejection(ErrorCode::InvalidRequest, "gone");
    let effects = rejection_effects(&Context::Objective(oid()), &refused);
    assert!(matches!(
        effects.updates.as_slice(),
        [StoreUpdate::ObjectiveLoadFailed { objective_id, .. }] if objective_id == &oid()
    ));
    let effects = rejection_effects(&Context::Changes(oid(), Basis::Working), &refused);
    assert!(matches!(
        effects.updates.as_slice(),
        [StoreUpdate::ChangesFailed {
            basis: Basis::Working,
            ..
        }]
    ));
    let path = RepoPath::utf8("a.txt");
    for context in [
        Context::Diff(oid(), Basis::Working, path.clone()),
        Context::File(oid(), Basis::Working, path.clone(), Side::New),
    ] {
        let effects = rejection_effects(&context, &refused);
        assert!(matches!(
            effects.updates.as_slice(),
            [StoreUpdate::ViewerFailed { path: p, .. }] if p == &path
        ));
    }
    let search = Context::Search {
        scope: view::SearchScope::Content,
        query: "q".into(),
        objective_id: None,
        basis: None,
    };
    assert!(matches!(
        rejection_effects(&search, &refused).updates.as_slice(),
        [StoreUpdate::SearchFailed { .. }]
    ));
    assert!(matches!(
        rejection_effects(&Context::Terminal(oid(), "Open terminal"), &refused)
            .updates
            .as_slice(),
        [StoreUpdate::TerminalRefused { .. }]
    ));
    for context in [&Context::Objective(oid()), &search] {
        assert!(rejection_effects(context, &refused).follow_ups.is_empty());
    }
}

#[test]
fn a_refused_command_reloads_its_objective_and_the_drafts_it_touched() {
    let refused = rejection(ErrorCode::InvalidLifecycle, "not now");
    let command = |batch: Vec<DraftId>| Context::Command {
        label: "Submit feedback",
        objective_id: Some(oid()),
        saved_draft: None,
        batch,
        surface: Some(ActionSurface::Feedback),
    };
    let effects = rejection_effects(&command(Vec::new()), &refused);
    assert_eq!(
        effects.follow_ups,
        [Intent::LoadObjective {
            objective_id: oid()
        }]
    );
    let effects = rejection_effects(&command(vec![DraftId::new(D)]), &refused);
    assert_eq!(
        effects.follow_ups,
        [
            Intent::LoadObjective {
                objective_id: oid()
            },
            Intent::LoadDrafts {
                objective_id: oid()
            },
        ]
    );
    assert!(matches!(
        effects.updates.as_slice(),
        [
            StoreUpdate::CommandStatus { status: None, .. },
            StoreUpdate::Refused {
                surface: ActionSurface::Feedback,
                ..
            },
        ]
    ));
}

#[test]
fn several_stale_drafts_are_named_together() {
    let mut stale = rejection(ErrorCode::StaleReference, "");
    let ids = [test_id(0xd1), test_id(0xd2), test_id(0xd3)];
    stale.stale_drafts = ids[..2].to_vec();
    let context = Context::Command {
        label: "Submit feedback",
        objective_id: Some(oid()),
        saved_draft: None,
        batch: ids.iter().map(|i| DraftId::new(i.as_str())).collect(),
        surface: Some(ActionSurface::Feedback),
    };
    let effects = rejection_effects(&context, &stale);
    let Some(StoreUpdate::Refused { refusal, .. }) = effects.updates.get(1) else {
        panic!("a refusal: {:?}", effects.updates);
    };
    let short = |i: &Id| DraftId::new(i.as_str()).short().to_owned();
    assert!(
        refusal.message.ends_with(&format!(
            " Stale drafts: {}, {}. Re-anchor or remove them and submit again.",
            short(&ids[0]),
            short(&ids[1])
        )),
        "{}",
        refusal.message
    );
}
