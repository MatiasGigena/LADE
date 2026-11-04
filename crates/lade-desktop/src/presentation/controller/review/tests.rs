use super::*;
use crate::presentation::controller::intents::RecordingSink;
use crate::presentation::controller::tests::setup;
use crate::presentation::state::model::{
    ActivityItem, ActivityItemId, ActivityRole, ArtifactDigest, CandidateId, CaptureId, ChangeList,
    ChangedFile, FileDiff, FileEncoding, FilePage, GitId, SearchResults, TargetRef, Timestamp,
};
use crate::presentation::state::store::StoreUpdate;
use crate::presentation::test_fixture;
use std::rc::Rc;

fn main_selected() -> (Rc<Controller>, Rc<RecordingSink>, ObjectiveId) {
    let (controller, sink) = setup();
    let id = ObjectiveId::new(test_fixture::MAIN_OBJECTIVE);
    controller.select_objective(&id);
    sink.intents.borrow_mut().clear();
    (controller, sink, id)
}

fn changes(id: &ObjectiveId, files: &[(&str, ChangeStatus)]) -> StoreUpdate {
    StoreUpdate::Changes(ChangeList {
        objective_id: id.clone(),
        basis: Basis::Working,
        basis_label: "Working inspection".into(),
        target: TargetRef {
            branch: "main".into(),
            commit: GitId::sha1("abc"),
        },
        capture_id: Some(CaptureId::new("capture")),
        candidate_id: None,
        files: files
            .iter()
            .map(|(path, status)| ChangedFile {
                path: RepoPath::utf8(path),
                old_path: None,
                status: *status,
                added: 1,
                removed: 1,
                binary: false,
                filtered: false,
            })
            .collect(),
        truncated: false,
        total_added: 1,
        total_removed: 1,
        notices: Vec::new(),
    })
}

fn selection(side: Side) -> MappedSelection {
    MappedSelection {
        side,
        start: 3,
        end: 7,
        selected_text: "text".into(),
        line_hint: Some(2),
    }
}

fn diff(id: &ObjectiveId, path: &str) -> StoreUpdate {
    StoreUpdate::Diff(FileDiff {
        objective_id: id.clone(),
        basis: Basis::Working,
        path: RepoPath::utf8(path),
        old_digest: Some(ArtifactDigest::new("old")),
        new_digest: Some(ArtifactDigest::new("new")),
        binary: false,
        hunks: Vec::new(),
        truncated: false,
    })
}

#[test]
fn content_search_results_become_the_viewer_selection_range() {
    let (controller, _) = setup();
    let id = ObjectiveId::new(test_fixture::MAIN_OBJECTIVE);
    controller.select_objective(&id);
    let path = RepoPath::utf8("src/network/retry.ts");
    controller.open_search_result(&SearchResult {
        kind: SearchResultKind::Content,
        objective_id: Some(id.clone()),
        path: Some(path.clone()),
        line: Some(2),
        preview: "retry".into(),
        byte_start: Some(10),
        byte_end: Some(15),
    });
    let key = controller
        .store
        .state()
        .viewer
        .key
        .clone()
        .expect("file opened");
    assert_eq!((key.path.clone(), key.mode), (path, ViewerMode::FullFile));
    assert_eq!(
        controller.take_reveal(&key, 12),
        None,
        "range not loaded yet"
    );
    assert_eq!(controller.take_reveal(&key, 15), Some((10, 15)));
    assert_eq!(controller.take_reveal(&key, 15), None, "taken once");
    controller.open_search_result(&SearchResult {
        kind: SearchResultKind::Content,
        objective_id: Some(id),
        path: Some(RepoPath::utf8("src/network/retry.ts")),
        line: Some(2),
        preview: "retry".into(),
        byte_start: None,
        byte_end: None,
    });
    assert_eq!(controller.store.state().viewer.reveal, None);
}

#[test]
fn a_deleted_file_opens_on_its_old_side_and_each_mode_requests_its_projection() {
    let (controller, sink, id) = main_selected();
    controller.store.apply(changes(
        &id,
        &[
            ("gone.rs", ChangeStatus::Deleted),
            ("kept.rs", ChangeStatus::Modified),
        ],
    ));
    controller.select_file(&RepoPath::utf8("gone.rs"));
    controller.select_file(&RepoPath::utf8("gone.rs"));
    assert_eq!(
        sink.intents.borrow().as_slice(),
        [Intent::LoadDiff {
            objective_id: id.clone(),
            basis: Basis::Working,
            path: RepoPath::utf8("gone.rs"),
        }],
        "reselecting the open file does not reload it"
    );
    controller.toggle_viewer_mode();
    assert_eq!(
        sink.intents.borrow().last(),
        Some(&Intent::OpenFile {
            objective_id: id.clone(),
            basis: Basis::Working,
            path: RepoPath::utf8("gone.rs"),
            side: Side::Old,
            offset: 0,
        })
    );
    let state = controller.store.state();
    let key = state.viewer.key.as_ref().unwrap();
    assert_eq!((key.mode, key.side), (ViewerMode::FullFile, Side::Old));
    assert_eq!(state.viewer.content, ViewerContent::Loading);
}

#[test]
fn more_of_a_file_is_requested_only_until_its_end() {
    let (controller, sink, id) = main_selected();
    controller.set_viewer_mode(ViewerMode::FullFile);
    controller.select_file(&RepoPath::utf8("a.txt"));
    sink.intents.borrow_mut().clear();
    controller.load_more();
    assert!(sink.intents.borrow().is_empty(), "nothing loaded yet");
    let page = |offset, text: &str, eof| {
        StoreUpdate::FilePage(FilePage {
            objective_id: id.clone(),
            basis: Basis::Working,
            path: RepoPath::utf8("a.txt"),
            side: Side::New,
            artifact_digest: ArtifactDigest::new("d"),
            total_bytes: 8,
            offset,
            length: 4,
            encoding: FileEncoding::Utf8,
            text: Some(text.into()),
            eof,
            version_label: "working".into(),
            chunked: true,
        })
    };
    controller.store.apply(page(0, "abcd", false));
    controller.load_more();
    assert!(matches!(
        sink.intents.borrow().as_slice(),
        [Intent::OpenFile { offset: 4, .. }]
    ));
    controller.store.apply(page(4, "efgh", true));
    sink.intents.borrow_mut().clear();
    controller.load_more();
    assert!(sink.intents.borrow().is_empty(), "the end was reached");
}

#[test]
fn a_diff_selection_becomes_a_reference_to_the_working_capture() {
    let (controller, _, id) = main_selected();
    controller
        .store
        .apply(changes(&id, &[("a.txt", ChangeStatus::Modified)]));
    controller.select_file(&RepoPath::utf8("a.txt"));
    controller.set_viewer_selection(Some(Ok(selection(Side::New))));
    assert_eq!(
        controller.store.state().selected_ui().unwrap().selection,
        Some(Err(
            "The inspected content has no retained identity yet — refresh".into()
        )),
        "the diff has not arrived"
    );
    controller.store.apply(diff(&id, "a.txt"));
    for (side, digest) in [(Side::New, "new"), (Side::Old, "old")] {
        controller.set_viewer_selection(Some(Ok(selection(side))));
        let state = controller.store.state();
        let Some(Ok(reference)) = &state.selected_ui().unwrap().selection else {
            panic!("a reference");
        };
        assert_eq!(reference.artifact_digest, ArtifactDigest::new(digest));
        assert_eq!(
            reference.version,
            ReferenceVersion::Working {
                capture_id: CaptureId::new("capture")
            }
        );
        assert_eq!(
            (reference.byte_start, reference.byte_end, reference.side),
            (3, 7, side)
        );
    }
    controller.set_viewer_selection(Some(Err(SelectionError::SpansHunks)));
    assert_eq!(
        controller.store.state().selected_ui().unwrap().selection,
        Some(Err(SelectionError::SpansHunks.message().into()))
    );
    controller.set_viewer_selection(None);
    assert_eq!(
        controller.store.state().selected_ui().unwrap().selection,
        None
    );
}

#[test]
fn a_since_basis_references_the_candidate_on_the_selected_side() {
    let (controller, _, id) = main_selected();
    let since = Basis::Since {
        from_candidate_id: CandidateId::new("from"),
        to_candidate_id: CandidateId::new("to"),
    };
    controller.select_basis(since.clone());
    controller.select_file(&RepoPath::utf8("a.txt"));
    let StoreUpdate::Diff(mut reply) = diff(&id, "a.txt") else {
        unreachable!()
    };
    reply.basis = since;
    controller.store.apply(StoreUpdate::Diff(reply));
    for (side, candidate) in [(Side::New, "to"), (Side::Old, "from")] {
        controller.set_viewer_selection(Some(Ok(selection(side))));
        let state = controller.store.state();
        let Some(Ok(reference)) = &state.selected_ui().unwrap().selection else {
            panic!("a reference");
        };
        assert_eq!(
            reference.version,
            ReferenceVersion::Candidate {
                candidate_id: CandidateId::new(candidate)
            }
        );
    }
}

#[test]
fn changing_the_basis_reloads_the_change_set_and_open_file_and_drops_the_selection() {
    let (controller, sink, id) = main_selected();
    controller.select_file(&RepoPath::utf8("a.txt"));
    controller.store.update(|s| {
        s.objective_ui_mut(&id).selection = Some(Err("old".into()));
        ((), Vec::new())
    });
    sink.intents.borrow_mut().clear();
    controller.select_basis(Basis::Working);
    assert!(sink.intents.borrow().is_empty(), "same basis");
    let candidate = Basis::Candidate {
        candidate_id: CandidateId::new("c"),
    };
    controller.select_basis(candidate.clone());
    assert_eq!(
        sink.intents.borrow().as_slice(),
        [
            Intent::LoadChanges {
                objective_id: id.clone(),
                basis: candidate.clone(),
            },
            Intent::LoadDiff {
                objective_id: id.clone(),
                basis: candidate,
                path: RepoPath::utf8("a.txt"),
            },
        ]
    );
    let state = controller.store.state();
    assert_eq!(state.changes.get(&id), Some(&Load::Loading));
    assert_eq!(state.selected_ui().unwrap().selection, None);
}

#[test]
fn searches_need_a_workspace_and_a_query_and_scope_files_to_the_selection() {
    let (controller, sink, id) = main_selected();
    controller.search(SearchScope::Content, "   ");
    assert!(sink.intents.borrow().is_empty());
    controller.search(SearchScope::Files, " retry ");
    controller.search(SearchScope::Objectives, "retry");
    let intents = sink.intents.borrow();
    let [
        Intent::Search {
            objective_id: Some(files_in),
            basis: Some(Basis::Working),
            query,
            ..
        },
        Intent::Search {
            objective_id: None,
            basis: None,
            ..
        },
    ] = intents.as_slice()
    else {
        panic!("two searches: {intents:?}");
    };
    assert_eq!((files_in, query.as_str()), (&id, "retry"));
    assert_eq!(controller.store.state().search, Load::Loading);
    drop(intents);
    controller.store.update(|s| {
        s.active_workspace = None;
        ((), Vec::new())
    });
    sink.intents.borrow_mut().clear();
    controller.search(SearchScope::Objectives, "retry");
    assert!(sink.intents.borrow().is_empty(), "no workspace to search");
}

#[test]
fn a_content_result_from_another_basis_switches_to_that_basis() {
    let (controller, sink, id) = main_selected();
    let candidate = Basis::Candidate {
        candidate_id: CandidateId::new("c"),
    };
    controller.store.apply(StoreUpdate::Search(SearchResults {
        scope: SearchScope::Content,
        query: "retry".into(),
        scope_label: "Candidate".into(),
        basis: Some(candidate.clone()),
        objective_id: Some(id.clone()),
        results: Vec::new(),
        truncated: false,
    }));
    controller.open_search_result(&SearchResult {
        kind: SearchResultKind::Objective,
        objective_id: Some(id.clone()),
        path: None,
        line: None,
        preview: String::new(),
        byte_start: None,
        byte_end: None,
    });
    assert_eq!(
        controller.store.state().selected_ui().unwrap().basis,
        Basis::Working
    );
    controller.open_search_result(&SearchResult {
        kind: SearchResultKind::File,
        objective_id: Some(id),
        path: Some(RepoPath::utf8("a.txt")),
        line: None,
        preview: String::new(),
        byte_start: None,
        byte_end: None,
    });
    let state = controller.store.state();
    assert_eq!(state.selected_ui().unwrap().basis, candidate);
    assert_eq!(state.viewer.key.as_ref().unwrap().mode, ViewerMode::Diff);
    assert!(
        sink.intents
            .borrow()
            .iter()
            .any(|i| matches!(i, Intent::LoadDiff { basis, .. } if basis == &candidate))
    );
}

#[test]
fn a_reveal_for_another_file_is_discarded() {
    let (controller, _, id) = main_selected();
    let key = |path: &str| ViewerKey {
        objective_id: id.clone(),
        basis: Basis::Working,
        path: RepoPath::utf8(path),
        mode: ViewerMode::FullFile,
        side: Side::New,
    };
    controller.store.update(|s| {
        s.viewer.reveal = Some(Reveal {
            key: key("a.txt"),
            start: 0,
            end: 1,
        });
        ((), Vec::new())
    });
    assert_eq!(controller.take_reveal(&key("b.txt"), 100), None);
    assert_eq!(controller.store.state().viewer.reveal, None);
}

#[test]
fn older_activity_is_requested_before_the_oldest_item_held() {
    let (controller, sink, id) = main_selected();
    controller.load_older_activity();
    assert_eq!(
        sink.intents.borrow().last(),
        Some(&Intent::LoadActivity {
            objective_id: id.clone(),
            before: None,
        })
    );
    controller.store.apply(StoreUpdate::ActivityPage {
        objective_id: id.clone(),
        items: vec![ActivityItem {
            item_id: ActivityItemId::new("i"),
            seq: 41,
            at: Timestamp(0),
            role: ActivityRole::Assistant,
            title: String::new(),
            text: String::new(),
            tool_name: None,
            is_error: false,
            truncated: false,
        }],
        has_more: true,
    });
    controller.load_older_activity();
    assert_eq!(
        sink.intents.borrow().last(),
        Some(&Intent::LoadActivity {
            objective_id: id,
            before: Some(41),
        })
    );
}
