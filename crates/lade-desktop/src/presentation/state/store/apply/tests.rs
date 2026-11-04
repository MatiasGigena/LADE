use super::*;
use crate::presentation::state::agent_session::AgentSessionUpdate;
use crate::presentation::state::model::{
    ActivityItem, ActivityItemId, ActivityRole, ArtifactDigest, Attention, CandidateId,
    ChangeStatus, ChangedFile, DiffHunk, FileEncoding, GitId, Lifecycle, NoticeLevel, Reference,
    Side, TargetRef, Timestamp, WorkspaceId,
};
use crate::presentation::state::store::ViewerKey;
use crate::presentation::state::store::tests::summary;

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

fn change_list(o: &ObjectiveId, basis: Basis, files: &[&str]) -> ChangeList {
    ChangeList {
        objective_id: o.clone(),
        basis,
        basis_label: "Working inspection".into(),
        target: TargetRef {
            branch: "main".into(),
            commit: GitId::sha1("abc"),
        },
        capture_id: None,
        candidate_id: None,
        files: files
            .iter()
            .map(|path| ChangedFile {
                path: RepoPath::utf8(path),
                old_path: None,
                status: ChangeStatus::Modified,
                added: 1,
                removed: 0,
                binary: false,
                filtered: false,
            })
            .collect(),
        truncated: false,
        total_added: 0,
        total_removed: 0,
        notices: Vec::new(),
    }
}

fn workspace(id: &str) -> Workspace {
    Workspace {
        workspace_id: WorkspaceId::new(id),
        path: RepoPath::utf8(id),
        name: id.to_owned(),
        default_branch: None,
        branches: Vec::new(),
    }
}

fn viewer_key(o: &ObjectiveId, path: &str, mode: ViewerMode) -> ViewerKey {
    ViewerKey {
        objective_id: o.clone(),
        basis: Basis::Working,
        path: RepoPath::utf8(path),
        mode,
        side: Side::New,
    }
}

fn file_page(o: &ObjectiveId, digest: &str, offset: u64, text: &str, eof: bool) -> StoreUpdate {
    StoreUpdate::FilePage(FilePage {
        objective_id: o.clone(),
        basis: Basis::Working,
        path: RepoPath::utf8("a.txt"),
        side: Side::New,
        artifact_digest: ArtifactDigest::new(digest),
        total_bytes: 10,
        offset,
        length: text.len() as u64,
        encoding: FileEncoding::Utf8,
        text: Some(text.into()),
        eof,
        version_label: "working".into(),
        chunked: false,
    })
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
fn stale_changes_reply_for_previous_basis_is_ignored() {
    let mut state = PresentationState::default();
    let o = ObjectiveId::new("o");
    state.objective_ui_mut(&o).basis = Basis::Candidate {
        candidate_id: crate::presentation::state::model::CandidateId::new("c2"),
    };
    let list = change_list(&o, Basis::Working, &[]);
    assert!(state.apply(StoreUpdate::Changes(list)).is_empty());
    assert!(!state.changes.contains_key(&o));
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

#[test]
fn refusals_also_repaint_the_surface_that_shows_them() {
    let o = ObjectiveId::new("o");
    assert_eq!(
        refusal_changes(o.clone(), ActionSurface::Terminal),
        [Change::Detail(o.clone()), Change::Terminal(o.clone())]
    );
    for surface in [ActionSurface::Drafts, ActionSurface::Feedback] {
        assert_eq!(
            refusal_changes(o.clone(), surface),
            [Change::Detail(o.clone()), Change::Drafts(o.clone())]
        );
    }
    assert_eq!(
        refusal_changes(o.clone(), ActionSurface::Review),
        [Change::Detail(o)]
    );
}

#[test]
fn a_snapshot_keeps_a_listed_active_workspace_and_drops_a_vanished_selection() {
    let mut state = PresentationState::new(ConnectionState::Connected);
    let snapshot =
        |workspaces: Vec<Workspace>, objectives: Vec<ObjectiveSummary>| StoreUpdate::Snapshot {
            workspaces,
            objectives,
            settings: Settings::default(),
        };
    state.apply(snapshot(vec![workspace("a"), workspace("b")], Vec::new()));
    assert_eq!(state.active_workspace, Some(WorkspaceId::new("a")));
    state.apply(StoreUpdate::WorkspaceOpened(workspace("b")));
    assert_eq!(state.active_workspace, Some(WorkspaceId::new("b")));
    assert_eq!(state.workspaces.len(), 2, "an opened workspace is upserted");
    state.apply(snapshot(
        vec![workspace("a"), workspace("b")],
        vec![summary("o", Attention::Idle, Lifecycle::Active, 1)],
    ));
    assert_eq!(state.active_workspace, Some(WorkspaceId::new("b")));
    state.ui.selected = Some(ObjectiveId::new("o"));
    let changes = state.apply(snapshot(vec![workspace("c")], Vec::new()));
    assert_eq!(state.active_workspace, Some(WorkspaceId::new("c")));
    assert_eq!(state.ui.selected, None);
    assert!(changes.contains(&Change::Selection));
}

#[test]
fn a_change_list_forgets_a_selected_file_it_no_longer_contains() {
    let mut state = PresentationState::default();
    let o = ObjectiveId::new("o");
    state.objective_ui_mut(&o).file = Some(RepoPath::utf8("kept.rs"));
    state.working_updated.insert(o.clone());
    state.apply(StoreUpdate::Changes(change_list(
        &o,
        Basis::Working,
        &["kept.rs"],
    )));
    assert_eq!(
        state.objective_ui_mut(&o).file,
        Some(RepoPath::utf8("kept.rs"))
    );
    assert!(
        !state.working_updated.contains(&o),
        "a fresh list clears the flag"
    );
    state.apply(StoreUpdate::Changes(change_list(
        &o,
        Basis::Working,
        &["other.rs"],
    )));
    assert_eq!(state.objective_ui_mut(&o).file, None);
    let failed = |basis| StoreUpdate::ChangesFailed {
        objective_id: o.clone(),
        basis,
        message: "gone".into(),
    };
    assert!(
        state
            .apply(failed(Basis::Candidate {
                candidate_id: CandidateId::new("c")
            }))
            .is_empty()
    );
    assert!(matches!(state.changes.get(&o), Some(Load::Loaded(_))));
    assert_eq!(
        state.apply(failed(Basis::Working)),
        [Change::Changes(o.clone())]
    );
    assert_eq!(state.changes.get(&o), Some(&Load::Failed("gone".into())));
}

#[test]
fn viewer_replies_only_land_on_the_file_and_mode_still_shown() {
    let mut state = PresentationState::default();
    let o = ObjectiveId::new("o");
    let diff = |path: &str| {
        StoreUpdate::Diff(FileDiff {
            objective_id: o.clone(),
            basis: Basis::Working,
            path: RepoPath::utf8(path),
            old_digest: None,
            new_digest: None,
            binary: false,
            hunks: Vec::<DiffHunk>::new(),
            truncated: false,
        })
    };
    assert!(state.apply(diff("a.txt")).is_empty(), "no file shown");
    state.viewer.key = Some(viewer_key(&o, "a.txt", ViewerMode::FullFile));
    assert!(state.apply(diff("a.txt")).is_empty(), "full-file mode");
    state.viewer.key = Some(viewer_key(&o, "a.txt", ViewerMode::Diff));
    assert!(state.apply(diff("b.txt")).is_empty(), "another file");
    assert_eq!(state.apply(diff("a.txt")), [Change::Viewer]);
    assert!(matches!(state.viewer.content, ViewerContent::Diff(_)));
    let failed = |path: &str| StoreUpdate::ViewerFailed {
        objective_id: o.clone(),
        path: RepoPath::utf8(path),
        message: "unreadable".into(),
    };
    assert!(state.apply(failed("b.txt")).is_empty());
    assert_eq!(state.apply(failed("a.txt")), [Change::Viewer]);
    assert_eq!(
        state.viewer.content,
        ViewerContent::Failed("unreadable".into())
    );
}

#[test]
fn file_pages_out_of_sequence_are_dropped_and_a_new_digest_restarts_from_zero() {
    let mut state = PresentationState::default();
    let o = ObjectiveId::new("o");
    state.viewer.key = Some(viewer_key(&o, "a.txt", ViewerMode::FullFile));
    assert!(
        state
            .apply(file_page(&o, "d1", 4, "late", false))
            .is_empty()
    );
    state.apply(file_page(&o, "d1", 0, "abcd", false));
    assert!(
        state.apply(file_page(&o, "d1", 2, "cd", false)).is_empty(),
        "an overlapping page of the same version is ignored"
    );
    assert!(
        state
            .apply(file_page(&o, "d2", 4, "efgh", false))
            .is_empty()
    );
    assert_eq!(
        state.apply(file_page(&o, "d2", 0, "new", true)),
        [Change::Viewer]
    );
    let ViewerContent::File(content) = &state.viewer.content else {
        panic!("file content");
    };
    assert_eq!(
        (content.text.as_str(), content.loaded_bytes, content.eof),
        ("new", 3, true)
    );
}

#[test]
fn selecting_another_objective_clears_the_viewer_once() {
    let mut state = PresentationState::default();
    let o = ObjectiveId::new("o");
    state.viewer.key = Some(viewer_key(&o, "a.txt", ViewerMode::Diff));
    state.viewer.content = ViewerContent::Loading;
    assert_eq!(
        state.apply(StoreUpdate::Select(o.clone())),
        [Change::Selection, Change::Viewer]
    );
    assert_eq!(state.viewer.key, None);
    assert_eq!(state.viewer.content, ViewerContent::Empty);
    assert!(state.apply(StoreUpdate::Select(o)).is_empty());
}

#[test]
fn an_unchanged_command_status_reports_nothing() {
    let mut state = PresentationState::default();
    let o = ObjectiveId::new("o");
    let status = || StoreUpdate::CommandStatus {
        objective_id: o.clone(),
        status: Some(ServiceNotice {
            level: NoticeLevel::Info,
            code: "query_operation".into(),
            message: "Checking…".into(),
            scope: NoticeScope::General,
        }),
    };
    assert_eq!(state.apply(status()), [Change::Detail(o.clone())]);
    assert!(state.apply(status()).is_empty());
    let cleared = || StoreUpdate::CommandStatus {
        objective_id: o.clone(),
        status: None,
    };
    assert_eq!(state.apply(cleared()), [Change::Detail(o.clone())]);
    assert!(state.apply(cleared()).is_empty());
}

#[test]
fn a_delete_refusal_for_an_unknown_draft_becomes_a_coded_warning() {
    let mut state = PresentationState::default();
    let changes = state.apply(StoreUpdate::DraftDeleteRefused {
        draft_id: DraftId::new("gone"),
        refusal: refusal("Couldn't delete draft."),
    });
    assert_eq!(changes, [Change::Notices]);
    let notice = state.notices.last().expect("warning");
    assert_eq!(notice.level, NoticeLevel::Warning);
    assert_eq!(notice.code, "invalid_lifecycle");
    assert_eq!(notice.message, "Couldn't delete draft.");
}

#[test]
fn updates_for_unknown_drafts_terminals_and_details_change_nothing() {
    let mut state = PresentationState::default();
    assert!(
        state
            .apply(StoreUpdate::DraftSaveFailed {
                draft_id: DraftId::new("gone"),
                message: "x".into(),
            })
            .is_empty()
    );
    assert!(
        state
            .apply(StoreUpdate::TerminalOutput {
                terminal_id: "t".into(),
                seq: 1,
                data: "x".into(),
                dropped: 0,
            })
            .is_empty()
    );
    assert!(state.remove_draft(&DraftId::new("gone")).is_empty());
    assert!(state.inputs.is_empty() && state.terminals.is_empty());
}

#[test]
fn settings_repaint_the_workspaces_and_the_selected_detail() {
    let mut state = PresentationState::default();
    assert_eq!(
        state.apply(StoreUpdate::Settings(Settings::default())),
        [Change::Workspaces]
    );
    let o = ObjectiveId::new("o");
    state.ui.selected = Some(o.clone());
    assert_eq!(
        state.apply(StoreUpdate::Settings(Settings::default())),
        [Change::Workspaces, Change::Detail(o)]
    );
}
