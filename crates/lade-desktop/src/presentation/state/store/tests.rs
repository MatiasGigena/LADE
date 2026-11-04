use super::*;
use crate::presentation::state::agent_session::AgentSessionUpdate;
use crate::presentation::state::model::{
    ActivityItem, ActivityItemId, ActivityRole, Basis, ExecutionState, Lifecycle, NoticeScope,
    Reference, RepoPath, Side, Timestamp,
};
use crate::presentation::terminal::{TerminalEnd, TerminalMode};

fn summary(id: &str, attention: Attention, lifecycle: Lifecycle, updated: i64) -> ObjectiveSummary {
    ObjectiveSummary {
        objective_id: ObjectiveId::new(id),
        workspace_id: WorkspaceId::new("w"),
        title: id.to_owned(),
        outcome: String::new(),
        lifecycle,
        execution: crate::presentation::state::model::Execution::new(ExecutionState::None, 0),
        attention,
        badges: Vec::new(),
        updated_at: Timestamp(updated),
        revision: 1,
    }
}

fn item(id: &str, seq: u64, text: &str) -> ActivityItem {
    ActivityItem {
        item_id: ActivityItemId::new(id),
        seq,
        at: Timestamp(0),
        role: ActivityRole::Assistant,
        title: String::new(),
        text: text.to_owned(),
        tool_name: None,
        is_error: false,
        truncated: false,
    }
}

#[test]
fn board_orders_groups_by_attention_and_archives_terminal_lifecycles() {
    let mut state = PresentationState::new(ConnectionState::Connected);
    state.apply(StoreUpdate::Snapshot {
        workspaces: Vec::new(),
        objectives: vec![
            summary("idle", Attention::Idle, Lifecycle::Defined, 5),
            summary("working", Attention::Working, Lifecycle::Active, 4),
            summary("ready-old", Attention::ReadyForReview, Lifecycle::Frozen, 1),
            summary(
                "ready-new",
                Attention::ReadyForReview,
                Lifecycle::Approved,
                9,
            ),
            summary("needs", Attention::NeedsAttention, Lifecycle::Active, 2),
            summary("done", Attention::Idle, Lifecycle::Integrated, 7),
            summary("gone", Attention::Idle, Lifecycle::Discarded, 8),
        ],
        settings: Settings::default(),
    });
    let board = state.board();
    let order: Vec<(Attention, Vec<&str>)> = board
        .groups
        .iter()
        .map(|(a, g)| (*a, g.iter().map(|o| o.title.as_str()).collect()))
        .collect();
    assert_eq!(
        order,
        vec![
            (Attention::NeedsAttention, vec!["needs"]),
            (Attention::ReadyForReview, vec!["ready-new", "ready-old"]),
            (Attention::Working, vec!["working"]),
            (Attention::Idle, vec!["idle"]),
        ]
    );
    let archive: Vec<&str> = board.archive.iter().map(|o| o.title.as_str()).collect();
    assert_eq!(archive, vec!["gone", "done"]);
}

#[test]
fn file_pages_advance_by_the_reply_length() {
    use crate::presentation::state::model::{
        ArtifactDigest, FileEncoding, FilePage, RepoPath, Side,
    };
    let mut state = PresentationState::default();
    let o = ObjectiveId::new("o");
    let path = RepoPath::utf8("a.txt");
    state.viewer.key = Some(ViewerKey {
        objective_id: o.clone(),
        basis: Basis::Working,
        path: path.clone(),
        mode: ViewerMode::FullFile,
        side: Side::New,
    });
    let page = |offset: u64, length: u64, text: &str, eof: bool| {
        StoreUpdate::FilePage(FilePage {
            objective_id: o.clone(),
            basis: Basis::Working,
            path: path.clone(),
            side: Side::New,
            artifact_digest: ArtifactDigest::new("d"),
            total_bytes: 10,
            offset,
            length,
            encoding: FileEncoding::Utf8,
            text: Some(text.into()),
            eof,
            version_label: "working".into(),
            chunked: false,
        })
    };
    state.apply(page(0, 6, "héllo", false));
    let ViewerContent::File(content) = &state.viewer.content else {
        panic!("file content");
    };
    assert_eq!(content.loaded_bytes, 6);
    state.apply(page(6, 4, "rest", true));
    let ViewerContent::File(content) = &state.viewer.content else {
        panic!("file content");
    };
    assert_eq!((content.loaded_bytes, content.eof), (10, true));
    assert_eq!(content.text, "héllorest");
}

#[test]
fn a_resynced_activity_page_replaces_items_by_id() {
    let mut state = PresentationState::default();
    let o = ObjectiveId::new("o");
    state.apply(StoreUpdate::ActivityPage {
        objective_id: o.clone(),
        items: vec![item("a", 1, "hi"), item("b", 2, "par")],
        has_more: false,
    });
    state.apply(StoreUpdate::ActivityPage {
        objective_id: o.clone(),
        items: vec![item("b", 2, "partial text"), item("c", 3, "new")],
        has_more: true,
    });
    let log = &state.activity[&o];
    let texts: Vec<&str> = log.items.iter().map(|i| i.text.as_str()).collect();
    assert_eq!(texts, ["hi", "partial text", "new"]);
    assert!(!log.has_more, "a newer page does not reopen older history");
    state.apply(StoreUpdate::ActivityPage {
        objective_id: o.clone(),
        items: vec![item("z", 0, "first")],
        has_more: false,
    });
    assert_eq!(state.activity[&o].items.len(), 4);
    assert_eq!(state.activity[&o].items[0].text, "first");
}

#[test]
fn activity_appends_incrementally_and_applies_deltas_in_place() {
    let mut state = PresentationState::default();
    let o = ObjectiveId::new("o");
    state.apply(StoreUpdate::ActivityPage {
        objective_id: o.clone(),
        items: vec![item("a", 1, "hi")],
        has_more: true,
    });
    let changes = state.apply(StoreUpdate::ActivityAppended {
        objective_id: o.clone(),
        items: vec![item("b", 2, "par")],
    });
    assert_eq!(
        changes,
        vec![Change::Activity(
            o.clone(),
            ActivityChange::Appended { from_index: 1 }
        )]
    );
    let changes = state.apply(StoreUpdate::ActivityDelta {
        objective_id: o.clone(),
        item_id: ActivityItemId::new("b"),
        append_text: "tial".into(),
    });
    assert_eq!(
        changes,
        vec![Change::Activity(
            o.clone(),
            ActivityChange::Delta { index: 1 }
        )]
    );
    assert_eq!(state.activity[&o].items[1].text, "partial");
    assert!(
        state
            .apply(StoreUpdate::ActivityDelta {
                objective_id: o.clone(),
                item_id: ActivityItemId::new("zz"),
                append_text: "x".into(),
            })
            .is_empty()
    );
}

#[test]
fn deltas_coalesce_per_item() {
    let o = ObjectiveId::new("o");
    let delta = |id: &str, t: &str| StoreUpdate::ActivityDelta {
        objective_id: o.clone(),
        item_id: ActivityItemId::new(id),
        append_text: t.into(),
    };
    let merged = coalesce(vec![
        delta("a", "1"),
        delta("a", "2"),
        delta("b", "3"),
        delta("a", "4"),
    ]);
    assert_eq!(
        merged,
        vec![delta("a", "12"), delta("b", "3"), delta("a", "4")]
    );
}

#[test]
fn stale_changes_reply_for_previous_basis_is_ignored() {
    let mut state = PresentationState::default();
    let o = ObjectiveId::new("o");
    state.objective_ui_mut(&o).basis = Basis::Candidate {
        candidate_id: crate::presentation::state::model::CandidateId::new("c2"),
    };
    let list = ChangeList {
        objective_id: o.clone(),
        basis: Basis::Working,
        basis_label: "Working inspection".into(),
        target: crate::presentation::state::model::TargetRef {
            branch: "main".into(),
            commit: crate::presentation::state::model::GitId::sha1("abc"),
        },
        capture_id: None,
        candidate_id: None,
        files: Vec::new(),
        truncated: false,
        total_added: 0,
        total_removed: 0,
        notices: Vec::new(),
    };
    assert!(state.apply(StoreUpdate::Changes(list)).is_empty());
    assert!(!state.changes.contains_key(&o));
}

#[test]
fn notices_received_keeps_counting_past_the_retained_cap() {
    let mut state = PresentationState::default();
    for n in 0..MAX_NOTICES + 5 {
        state.apply(StoreUpdate::Notice(ServiceNotice {
            level: crate::presentation::state::model::NoticeLevel::Warning,
            code: "c".into(),
            message: format!("notice {n}"),
            scope: NoticeScope::General,
        }));
    }
    assert_eq!(state.notices.len(), MAX_NOTICES);
    assert_eq!(state.notices_received, MAX_NOTICES + 5);
}

#[test]
fn worktree_change_flags_instead_of_replacing() {
    let mut state = PresentationState::default();
    let o = ObjectiveId::new("o");
    state.apply(StoreUpdate::WorktreeChanged {
        objective_id: o.clone(),
    });
    assert!(state.working_updated.contains(&o));
}

#[test]
fn store_notifies_listeners_once_per_change() {
    let store = Store::new(PresentationState::default());
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    store.subscribe(move |changes| sink.borrow_mut().extend_from_slice(changes));
    store.apply_all(vec![
        StoreUpdate::Connection(ConnectionState::Connected),
        StoreUpdate::Connection(ConnectionState::Reconnecting { attempt: 1 }),
    ]);
    assert_eq!(*seen.borrow(), vec![Change::Connection]);
    assert_eq!(
        store.state().connection,
        ConnectionState::Reconnecting { attempt: 1 }
    );
}

#[test]
fn extension_dialogs_are_dropped_with_the_link_and_reannounced_after_it() {
    use crate::presentation::state::agent_session::{UiDialog, UiPromptView};
    let mut state = PresentationState::new(ConnectionState::Connected);
    let dialog = |id: &str| {
        StoreUpdate::AgentSession(AgentSessionUpdate::DialogOpened(UiDialog {
            objective_id: ObjectiveId::new("o"),
            ui_id: id.to_owned(),
            generation: 1,
            prompt: UiPromptView::Confirm {
                title: "t".into(),
                message: "m".into(),
            },
        }))
    };
    state.apply(dialog("closed-while-away"));
    state.apply(dialog("still-open"));
    let changes = state.apply(StoreUpdate::Connection(ConnectionState::Reconnecting {
        attempt: 1,
    }));
    assert!(changes.contains(&Change::Dialogs));
    assert!(state.agent_session.dialogs.is_empty());
    state.apply(StoreUpdate::Connection(ConnectionState::Connected));
    assert_eq!(state.apply(dialog("still-open")), vec![Change::Dialogs]);
    let open: Vec<&str> = state
        .agent_session
        .dialogs
        .iter()
        .map(|d| d.ui_id.as_str())
        .collect();
    assert_eq!(open, ["still-open"]);
}

fn refusal(message: &str) -> Refusal {
    Refusal {
        message: message.into(),
        details: format!("invalid_lifecycle: {message}"),
    }
}

#[test]
fn refusals_are_kept_per_surface_until_that_surface_succeeds_or_the_objective_changes() {
    let mut state = PresentationState::new(ConnectionState::Connected);
    let o = ObjectiveId::new("o");
    let mut current = summary("o", Attention::Idle, Lifecycle::Active, 1);
    state.apply(StoreUpdate::ObjectiveChanged(current.clone()));
    let refused = |surface| StoreUpdate::Refused {
        objective_id: o.clone(),
        surface,
        refusal: refusal("Couldn't send message. Start the agent first."),
    };
    let changes = state.apply(refused(ActionSurface::Send));
    assert!(changes.contains(&Change::Detail(o.clone())));
    state.apply(refused(ActionSurface::Review));
    assert_eq!(
        state.last_refusal(&o, ActionSurface::Send),
        Some(&refusal("Couldn't send message. Start the agent first."))
    );
    assert_eq!(state.last_refusal(&o, ActionSurface::Agent), None);
    state.apply(StoreUpdate::ObjectiveChanged(current.clone()));
    assert!(state.last_refusal(&o, ActionSurface::Send).is_some());
    state.apply(StoreUpdate::RefusalCleared {
        objective_id: o.clone(),
        surface: ActionSurface::Send,
    });
    assert_eq!(state.last_refusal(&o, ActionSurface::Send), None);
    assert!(state.last_refusal(&o, ActionSurface::Review).is_some());
    current.revision = 2;
    state.apply(StoreUpdate::ObjectiveChanged(current));
    assert_eq!(state.last_refusal(&o, ActionSurface::Review), None);
}

#[test]
fn command_notes_are_kept_apart_from_refusals() {
    let mut state = PresentationState::default();
    let o = ObjectiveId::new("o");
    let note = ServiceNotice {
        level: crate::presentation::state::model::NoticeLevel::Info,
        code: "query_operation".into(),
        message: "Checking outcome of “Integrate”…".into(),
        scope: NoticeScope::General,
    };
    state.apply(StoreUpdate::CommandStatus {
        objective_id: o.clone(),
        status: Some(note.clone()),
    });
    let changes = state.apply(StoreUpdate::Refused {
        objective_id: o.clone(),
        surface: ActionSurface::Review,
        refusal: refusal("Couldn't integrate. The target moved."),
    });
    assert_eq!(changes, [Change::Detail(o.clone())]);
    assert_eq!(
        state.command_note(&o),
        Some(&note),
        "a refusal is not a note"
    );
    state.apply(StoreUpdate::CommandStatus {
        objective_id: o.clone(),
        status: None,
    });
    assert_eq!(state.command_note(&o), None);
    assert!(state.last_refusal(&o, ActionSurface::Review).is_some());
    state.apply(StoreUpdate::Refused {
        objective_id: o.clone(),
        surface: ActionSurface::Drafts,
        refusal: refusal("Couldn't delete draft."),
    });
    let changes = state.apply(StoreUpdate::RefusalCleared {
        objective_id: o.clone(),
        surface: ActionSurface::Drafts,
    });
    assert!(changes.contains(&Change::Drafts(o)));
}

#[test]
fn draft_refusals_land_on_the_draft_and_its_objective() {
    use crate::presentation::state::drafts::{DraftEditor, SaveStatus};
    use crate::presentation::state::model::{
        ArtifactDigest, CaptureId, ReferenceVersion, SessionId,
    };
    let mut state = PresentationState::new(ConnectionState::Connected);
    let o = ObjectiveId::new("o");
    let reference = Reference {
        artifact_digest: ArtifactDigest::new("digest"),
        path: RepoPath::utf8("a.txt"),
        version: ReferenceVersion::Working {
            capture_id: CaptureId::new("cap"),
        },
        side: Side::New,
        byte_start: 0,
        byte_end: 1,
        selected_text: "a".into(),
        line_hint: Some(1),
    };
    let id = DraftId::new("d");
    let editor = DraftEditor::new_local(id.clone(), o.clone(), SessionId::new("s"), reference);
    state.drafts.insert(o.clone(), vec![editor]);
    state.apply(StoreUpdate::DraftSaveRefused {
        draft_id: id.clone(),
        refusal: Refusal {
            message: "The draft was not written.".into(),
            details: "durability_failure: the draft was not written".into(),
        },
    });
    let status = state.draft_mut(&id).unwrap().status(true);
    assert_eq!(status.label(), "Save failed. The draft was not written.");
    assert!(
        matches!(status, SaveStatus::Failed { details, .. } if details.starts_with("durability_failure"))
    );
    assert!(
        state.command_status.is_empty(),
        "shown beside the draft only"
    );
    state.apply(StoreUpdate::DraftDeleteRefused {
        draft_id: id,
        refusal: refusal("Couldn't delete draft. The draft was already submitted."),
    });
    assert!(state.last_refusal(&o, ActionSurface::Drafts).is_some());
    assert!(state.notices.is_empty());
}

#[test]
fn a_refused_open_leaves_no_failed_terminal_behind() {
    let mut state = PresentationState::new(ConnectionState::Connected);
    let o = ObjectiveId::new("o");
    state.push_terminal(TerminalSession::opening(o.clone(), 80, 24));
    let changes = state.apply(StoreUpdate::TerminalRefused {
        objective_id: o.clone(),
        refusal: refusal("Couldn't open terminal. Start the agent once."),
    });
    assert!(changes.contains(&Change::Terminal(o.clone())));
    assert!(state.terminal_for(&o).is_none(), "no terminal ever opened");
    assert_eq!(
        state
            .last_refusal(&o, ActionSurface::Terminal)
            .map(|r| r.message.as_str()),
        Some("Couldn't open terminal. Start the agent once.")
    );
    state.apply(StoreUpdate::TerminalProblem {
        objective_id: o.clone(),
        message: "Open terminal was not sent: not connected.".into(),
    });
    assert!(state.terminal_for(&o).is_none());
    state.push_terminal(TerminalSession::opening(o.clone(), 80, 24));
    state.apply(StoreUpdate::TerminalOpened {
        objective_id: o.clone(),
        terminal_id: "t".into(),
        mode: TerminalMode::ReadWrite,
        cols: 80,
        rows: 24,
    });
    assert_eq!(state.last_refusal(&o, ActionSurface::Terminal), None);
    state.apply(StoreUpdate::TerminalClosed {
        terminal_id: "t".into(),
        end: TerminalEnd::Closed,
    });
    state.push_terminal(TerminalSession::opening(o.clone(), 80, 24));
    state.apply(StoreUpdate::TerminalRefused {
        objective_id: o.clone(),
        refusal: refusal("Couldn't open terminal. Too many terminals are open."),
    });
    let session = state.terminal_for(&o).unwrap();
    assert_eq!(session.status, TerminalStatus::Ended(TerminalEnd::Closed));
    assert_eq!(
        state
            .last_refusal(&o, ActionSurface::Terminal)
            .map(|r| r.message.as_str()),
        Some("Couldn't open terminal. Too many terminals are open.")
    );
    assert_eq!(state.terminals.len(), 1);
    state.push_terminal(TerminalSession::opening(o.clone(), 80, 24));
    state.apply(StoreUpdate::Connection(ConnectionState::Reconnecting {
        attempt: 1,
    }));
    assert_eq!(state.terminals.len(), 1);
    assert_eq!(
        state.terminal_for(&o).map(|t| t.status.clone()),
        Some(TerminalStatus::Ended(TerminalEnd::Closed))
    );
}

#[test]
fn never_saved_draft_unlisted_after_reconnect_is_saved_again() {
    use crate::presentation::state::drafts::DraftEditor;
    use crate::presentation::state::model::{
        ArtifactDigest, CaptureId, DraftId, Reference, ReferenceVersion, RepoPath, SessionId, Side,
    };
    let mut state = PresentationState::new(ConnectionState::Connected);
    let o = ObjectiveId::new("o");
    let reference = Reference {
        artifact_digest: ArtifactDigest::new("digest"),
        path: RepoPath::utf8("a.txt"),
        version: ReferenceVersion::Working {
            capture_id: CaptureId::new("cap"),
        },
        side: Side::New,
        byte_start: 0,
        byte_end: 1,
        selected_text: "a".into(),
        line_hint: Some(1),
    };
    let id = DraftId::new("d");
    let mut editor = DraftEditor::new_local(id.clone(), o.clone(), SessionId::new("s"), reference);
    editor.edit("typed", 0);
    assert!(editor.poll(300, true).is_some());
    state.drafts.insert(o.clone(), vec![editor]);
    state.apply(StoreUpdate::Connection(ConnectionState::Reconnecting {
        attempt: 1,
    }));
    state.apply(StoreUpdate::Connection(ConnectionState::Connected));
    let draft = state.draft_mut(&id).unwrap();
    assert!(
        draft.poll(10_000, true).is_none(),
        "outcome unknown until drafts are listed"
    );
    state.apply(StoreUpdate::Drafts {
        objective_id: o.clone(),
        drafts: Vec::new(),
    });
    let draft = state.draft_mut(&id).expect("local text kept");
    assert_eq!(draft.body, "typed");
    let request = draft.poll(10_000, true).expect("saved again");
    assert_eq!(request.expected_revision, 0);
}
