#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code fails loudly on broken fixtures"
)]

mod service_support;

use std::fmt::Write as _;
use std::fs;
use std::io::{Seek, SeekFrom, Write};

use lade_protocol::client::{
    ActionName, ApprovalState, Basis, ContentNoticeKind, DiffGetQuery, DiffLineKind, DraftRevision,
    DraftSaveCommand, ErrorCode, ExecutionState, FeedbackSubmitCommand, FileEncoding,
    FileReadQuery, Lifecycle, ObjectiveCleanupCommand, ObjectiveDeleteHistoryCommand,
    ObjectiveSelector, Reference, ReferenceVersion, ReplyData, Request, ReviewApproveCommand,
    ReviewIntegrateCommand, SearchQuery, SearchScope, Side,
};
use lade_protocol::types::{Counter, DeliveryMode, Digest, Id, PageLimit, ReadLimit, WirePath};
use service_support::*;
use sha2::{Digest as _, Sha256};

fn freeze(client: &mut Client, objective: &Id) -> Id {
    match client.call(Request::ReviewFreeze(ObjectiveSelector {
        objective_id: objective.clone(),
    })) {
        Ok(ReplyData::ReviewFreeze(candidate)) => candidate.candidate_id,
        other => panic!("review.freeze: {other:?}"),
    }
}

fn read_at(client: &mut Client, objective: &Id, path: &str, offset: u64) -> ReplyData {
    client
        .call(Request::FileRead(FileReadQuery {
            objective_id: objective.clone(),
            basis: Basis::Working,
            path: WirePath::from_bytes(path.as_bytes().to_vec()).unwrap(),
            side: Side::New,
            offset: Counter::new(offset),
            limit: ReadLimit::parse("262144").unwrap(),
        }))
        .unwrap()
}

fn retained_refs(env: &Env) -> String {
    git(
        &env.repo,
        &["for-each-ref", "--format=%(refname)", "refs/lade/"],
    )
}

#[test]
fn working_change_set_names_nested_repositories_and_filtered_content() {
    let env = Env::new("svc-notices");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "Notices").objective_id;
    start_execution(&mut client, &objective);
    wait_state(&mut client, &objective, ExecutionState::Idle);
    let wt = worktree(&env, &objective);
    fs::create_dir(wt.join("vendored")).unwrap();
    git(&wt.join("vendored"), &["init", "-q"]);
    fs::write(wt.join("vendored/lib.c"), "int x;\n").unwrap();
    fs::write(wt.join(".gitattributes"), "*.bin filter=lfs -text\n").unwrap();
    fs::write(wt.join("weights.bin"), "smudged real content\n").unwrap();
    fs::write(wt.join("plain.txt"), "plain\n").unwrap();

    let list = changes(&mut client, &objective, Basis::Working);
    let paths: Vec<&[u8]> = list.files.iter().map(|f| f.path.as_bytes()).collect();
    assert_eq!(
        paths,
        [&b".gitattributes"[..], b"plain.txt", b"weights.bin"]
    );
    let filtered: Vec<bool> = list
        .files
        .iter()
        .map(|f| f.filtered == Some(true))
        .collect();
    assert_eq!(filtered, [false, false, true]);
    let notices = list.notices.clone().unwrap_or_default();
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert_eq!(notices[0].kind, ContentNoticeKind::NestedRepository);
    assert_eq!(
        notices[0].path.as_ref().map(WirePath::as_bytes),
        Some(&b"vendored"[..])
    );
    assert_eq!(notices[1].kind, ContentNoticeKind::FilteredContent);
    assert!(
        notices[1].message.contains("1 changed file"),
        "{}",
        notices[1].message
    );
    assert!(notices[1].path.is_none());

    let candidate = freeze(&mut client, &objective);
    let frozen = changes(
        &mut client,
        &objective,
        Basis::Candidate {
            candidate_id: candidate,
        },
    );
    assert!(
        frozen
            .files
            .iter()
            .any(|f| f.path.as_bytes() == b"weights.bin" && f.filtered == Some(true))
    );
    let kinds: Vec<ContentNoticeKind> = frozen.notices.unwrap().iter().map(|n| n.kind).collect();
    assert_eq!(kinds, [ContentNoticeKind::FilteredContent]);
    drop(client);
    service.shutdown();
}

#[test]
fn content_search_returns_exact_byte_ranges_of_the_retained_bytes() {
    let env = Env::new("svc-search");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "Search").objective_id;
    start_execution(&mut client, &objective);
    wait_state(&mut client, &objective, ExecutionState::Idle);
    let text = "αβγ\r\nsecond line\r\nwith ÉCOLE Needle here\n";
    fs::write(worktree(&env, &objective).join("notes.txt"), text).unwrap();

    let found = match client
        .call(Request::Search(SearchQuery {
            workspace_id: ws.workspace_id.clone(),
            scope: SearchScope::Content,
            query: "needle".into(),
            limit: PageLimit::parse("10").unwrap(),
            objective_id: Some(objective.clone()),
            basis: Some(Basis::Working),
        }))
        .unwrap()
    {
        ReplyData::Search(found) => found,
        other => panic!("{other:?}"),
    };
    assert_eq!(found.results.len(), 1);
    let hit = &found.results[0];
    let needle = text.find("Needle").unwrap();
    let start = needle as u64;
    assert_eq!(hit.line.as_ref().map(Counter::value), Some(3));
    assert_eq!(hit.byte_start.as_ref().map(Counter::value), Some(start));
    assert_eq!(hit.byte_end.as_ref().map(Counter::value), Some(start + 6));
    let column = start - text.find("with").unwrap() as u64 + 1;
    assert_eq!(hit.column.as_ref().map(Counter::value), Some(column));

    let file = read_file(&mut client, &objective, Basis::Working, "notes.txt");
    let retained = file.text.unwrap().into_bytes();
    assert_eq!(&retained[needle..needle + 6], b"Needle");
    drop(client);
    service.shutdown();
}

#[test]
fn a_moved_target_creates_a_fresh_working_capture_and_old_side() {
    let env = Env::new("svc-capture-target");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "Target context").objective_id;
    start_execution(&mut client, &objective);
    wait_state(&mut client, &objective, ExecutionState::Idle);
    let before = changes(&mut client, &objective, Basis::Working);
    assert_eq!(
        changes(&mut client, &objective, Basis::Working).capture_id,
        before.capture_id
    );
    fs::write(env.repo.join("README"), "hello\ntarget update\n").unwrap();
    let updated = commit_all(&env.repo, "move the target");
    let after = changes(&mut client, &objective, Basis::Working);
    assert_eq!(after.target.commit.hex(), updated);
    let old_side = client
        .call(Request::FileRead(FileReadQuery {
            objective_id: objective.clone(),
            basis: Basis::Working,
            path: WirePath::from_bytes(b"README".to_vec()).unwrap(),
            side: Side::Old,
            offset: Counter::new(0),
            limit: ReadLimit::parse("1024").unwrap(),
        }))
        .unwrap();
    let ReplyData::FileRead(old_side) = old_side else {
        panic!("expected file content")
    };
    assert_eq!(old_side.text.as_deref(), Some("hello\ntarget update\n"));
    assert_ne!(before.capture_id, after.capture_id);
    assert_eq!(after.total_removed.value(), 1);
    let diff = client
        .call(Request::DiffGet(DiffGetQuery {
            objective_id: objective.clone(),
            basis: Basis::Working,
            path: WirePath::from_bytes(b"README".to_vec()).unwrap(),
        }))
        .unwrap();
    let ReplyData::DiffGet(diff) = diff else {
        panic!("expected diff")
    };
    assert!(
        diff.hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .any(|line| line.kind == DiffLineKind::Del && line.text == "target update")
    );
    let repeated = changes(&mut client, &objective, Basis::Working);
    assert_eq!(repeated.capture_id, after.capture_id);
    assert_eq!(repeated.total_removed, after.total_removed);
    drop(client);
    service.shutdown();
}

#[test]
fn diff_lines_distinguish_final_bare_cr_from_crlf() {
    let env = Env::new("svc-diff-terminators");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "Line endings").objective_id;
    start_execution(&mut client, &objective);
    wait_state(&mut client, &objective, ExecutionState::Idle);
    let wt = worktree(&env, &objective);
    for (path, text, no_newline) in [
        ("bare-cr.txt", "abc\r", true),
        ("crlf.txt", "abc\r\n", false),
        ("README", "abc\r", true),
    ] {
        fs::write(wt.join(path), text).unwrap();
        changes(&mut client, &objective, Basis::Working);
        let ReplyData::DiffGet(diff) = client
            .call(Request::DiffGet(DiffGetQuery {
                objective_id: objective.clone(),
                basis: Basis::Working,
                path: WirePath::from_bytes(path.as_bytes().to_vec()).unwrap(),
            }))
            .unwrap()
        else {
            panic!("expected diff")
        };
        let added = diff
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .find(|line| line.kind == DiffLineKind::Add)
            .unwrap();
        assert_eq!(added.text, "abc\r", "{path}");
        assert_eq!(added.no_newline, no_newline, "{path}");
    }
    drop(client);
    service.shutdown();
}

/// Reads `large.txt`, repeated `line`s making up `content`, as bounded pages from the start,
/// the next page and the tail, and refuses an offset inside a character. Returns the file's
/// artifact digest.
fn assert_read_in_chunks(client: &mut Client, objective: &Id, content: &str, line: &str) -> Digest {
    let first = match read_at(client, objective, "large.txt", 0) {
        ReplyData::FileRead(page) => page,
        other => panic!("{other:?}"),
    };
    assert_eq!(first.chunked, Some(true));
    assert_eq!(first.encoding, FileEncoding::Utf8);
    assert_eq!(first.total_bytes.value(), content.len() as u64);
    assert!(!first.eof);
    let length = first.length.value();
    assert!(length > 0 && length <= 262_144, "{length}");
    let text = first.text.unwrap();
    assert_eq!(text.len() as u64, length);
    assert_eq!(
        text.as_bytes(),
        &content.as_bytes()[..usize::try_from(length).unwrap()]
    );
    let digest = Sha256::digest(content.as_bytes())
        .iter()
        .fold(String::new(), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        });
    assert_eq!(first.artifact_digest.as_str(), digest);

    let next = match read_at(client, objective, "large.txt", length) {
        ReplyData::FileRead(page) => page,
        other => panic!("{other:?}"),
    };
    assert_eq!(next.offset.value(), length);
    assert_eq!(next.artifact_digest.as_str(), digest);
    let at = usize::try_from(next.offset.value()).unwrap();
    let len = usize::try_from(next.length.value()).unwrap();
    assert_eq!(
        next.text.unwrap().as_bytes(),
        &content.as_bytes()[at..at + len]
    );
    let tail_offset = content.len() as u64 - line.len() as u64;
    let last = match read_at(client, objective, "large.txt", tail_offset) {
        ReplyData::FileRead(page) => page,
        other => panic!("{other:?}"),
    };
    assert!(last.eof);
    assert_eq!(last.text.as_deref(), Some(line));
    let inside = content.find('Ä').unwrap() as u64 + 1;
    let refused = client
        .call(Request::FileRead(FileReadQuery {
            objective_id: objective.clone(),
            basis: Basis::Working,
            path: WirePath::from_bytes(b"large.txt".to_vec()).unwrap(),
            side: Side::New,
            offset: Counter::new(inside),
            limit: ReadLimit::parse("1024").unwrap(),
        }))
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidRequest);
    first.artifact_digest
}

#[test]
fn files_above_the_whole_file_bound_are_read_in_bounded_chunks() {
    let env = Env::new("svc-large");
    let line = "0123456789 abcdefghij ÄÖÜ ünïcödé text line for the large file test\n";
    let mut content = String::with_capacity((64 << 20) + 4096);
    while content.len() <= 64 << 20 {
        content.push_str(line);
    }
    fs::write(env.repo.join("large.txt"), &content).unwrap();
    commit_all(&env.repo, "large file");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "Large").objective_id;
    start_execution(&mut client, &objective);
    wait_state(&mut client, &objective, ExecutionState::Idle);
    let capture = changes(&mut client, &objective, Basis::Working)
        .capture_id
        .unwrap();

    let first_digest = assert_read_in_chunks(&mut client, &objective, &content, line);

    let session = detail(&mut client, &objective).session.unwrap();
    let save = |client: &mut Client, draft: &Id| {
        client
            .call(Request::DraftSave(DraftSaveCommand {
                draft_id: draft.clone(),
                objective_id: objective.clone(),
                expected_revision: Counter::new(0),
                reference: Reference {
                    artifact_digest: first_digest.clone(),
                    path: WirePath::from_bytes(b"large.txt".to_vec()).unwrap(),
                    version: ReferenceVersion::Working {
                        capture_id: capture.clone(),
                    },
                    side: Side::New,
                    byte_start: Counter::new(11),
                    byte_end: Counter::new(21),
                    selected_text: "abcdefghij".into(),
                    line_hint: Some(Counter::new(1)),
                },
                body: "note".into(),
            }))
            .unwrap();
    };
    let submit = |client: &mut Client, draft: &Id| {
        client.call(Request::FeedbackSubmit(FeedbackSubmitCommand {
            objective_id: objective.clone(),
            destination_session_id: session.session_id.clone(),
            association_revision: session.association_revision.clone(),
            drafts: vec![DraftRevision {
                draft_id: draft.clone(),
                revision: Counter::new(1),
            }],
            mode: DeliveryMode::FollowUp,
            note: None,
        }))
    };
    let fresh = new_id();
    save(&mut client, &fresh);
    submit(&mut client, &fresh).unwrap();
    let stale = new_id();
    save(&mut client, &stale);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .open(worktree(&env, &objective).join("large.txt"))
        .unwrap();
    file.seek(SeekFrom::Start(11)).unwrap();
    file.write_all(b"ABCDEFGHIJ").unwrap();
    drop(file);
    let rejected = submit(&mut client, &stale).unwrap_err();
    assert_eq!(
        rejected.code,
        ErrorCode::StaleReference,
        "{}",
        rejected.detail
    );
    drop(client);
    service.shutdown();
}

#[test]
fn untracked_collision_refuses_before_the_target_moves() {
    let env = Env::new("svc-untracked");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "Collide").objective_id;
    start_execution(&mut client, &objective);
    wait_state(&mut client, &objective, ExecutionState::Idle);
    fs::write(worktree(&env, &objective).join("feature.txt"), "feature\n").unwrap();
    let candidate = freeze(&mut client, &objective);
    let current = detail(&mut client, &objective);
    let approval = match client
        .call(Request::ReviewApprove(ReviewApproveCommand {
            objective_id: objective.clone(),
            candidate_id: candidate,
            target_commit: current.integration_target.commit.clone(),
            override_reason: Some("manual check".into()),
        }))
        .unwrap()
    {
        ReplyData::ReviewApprove(a) => a.approval_id,
        other => panic!("{other:?}"),
    };
    let head = git(&env.repo, &["rev-parse", "HEAD"]);
    let reflog = git(
        &env.repo,
        &["reflog", "show", "--format=%H", "refs/heads/main"],
    );
    fs::write(env.repo.join("feature.txt"), "my local file\n").unwrap();
    let integrate = |client: &mut Client| {
        client.call(Request::ReviewIntegrate(ReviewIntegrateCommand {
            objective_id: objective.clone(),
            approval_id: approval.clone(),
        }))
    };
    let refused = integrate(&mut client).unwrap_err();
    assert_eq!(refused.code, ErrorCode::Conflict);
    assert!(refused.detail.contains("feature.txt"), "{}", refused.detail);
    assert_eq!(git(&env.repo, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git(
            &env.repo,
            &["reflog", "show", "--format=%H", "refs/heads/main"]
        ),
        reflog,
        "the target ref never moved"
    );
    assert_eq!(
        fs::read_to_string(env.repo.join("feature.txt")).unwrap(),
        "my local file\n"
    );
    let after = detail(&mut client, &objective);
    assert_eq!(after.lifecycle, Lifecycle::Approved);
    assert_eq!(after.approval.unwrap().state, ApprovalState::Valid);

    fs::remove_file(env.repo.join("feature.txt")).unwrap();
    integrate(&mut client).unwrap();
    assert_eq!(
        fs::read_to_string(env.repo.join("feature.txt")).unwrap(),
        "feature\n"
    );
    drop(client);
    service.shutdown();
}

#[test]
fn history_is_deleted_only_on_explicit_confirmation_after_cleanup() {
    let env = Env::new("svc-history");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "History").objective_id;
    start_execution(&mut client, &objective);
    wait_state(&mut client, &objective, ExecutionState::Idle);
    fs::write(worktree(&env, &objective).join("work.txt"), "work\n").unwrap();
    assert!(
        changes(&mut client, &objective, Basis::Working)
            .capture_id
            .is_some()
    );
    freeze(&mut client, &objective);
    let branch = detail(&mut client, &objective).branch;
    let delete = |client: &mut Client, confirm: bool| {
        client.call(Request::ObjectiveDeleteHistory(
            ObjectiveDeleteHistoryCommand {
                objective_id: objective.clone(),
                confirm_history_loss: confirm,
            },
        ))
    };

    let early = delete(&mut client, true).unwrap_err();
    assert_eq!(early.code, ErrorCode::InvalidLifecycle);
    assert!(!action_enabled(
        &detail(&mut client, &objective),
        ActionName::ObjectiveDeleteHistory
    ));
    client
        .call(Request::ObjectiveDiscard(ObjectiveSelector {
            objective_id: objective.clone(),
        }))
        .unwrap();
    client
        .call(Request::ObjectiveCleanup(ObjectiveCleanupCommand {
            objective_id: objective.clone(),
            confirm_data_loss: true,
        }))
        .unwrap();
    let before = retained_refs(&env);
    assert!(
        before.contains("/capture-") && before.contains("/candidate-"),
        "{before}"
    );
    assert!(git(&env.repo, &["branch", "--list", &branch]).is_empty());
    let cleaned = detail(&mut client, &objective);
    assert!(action_enabled(&cleaned, ActionName::ObjectiveDeleteHistory));
    assert_eq!(cleaned.history_deleted, None);

    let unconfirmed = delete(&mut client, false).unwrap_err();
    assert_eq!(unconfirmed.code, ErrorCode::InvalidRequest);
    assert_eq!(retained_refs(&env), before);

    let deleted = match delete(&mut client, true).unwrap() {
        ReplyData::ObjectiveDeleteHistory(d) => *d,
        other => panic!("{other:?}"),
    };
    assert_eq!(deleted.history_deleted, Some(true));
    assert_eq!(deleted.lifecycle, Lifecycle::Discarded);
    assert!(!action_enabled(
        &deleted,
        ActionName::ObjectiveDeleteHistory
    ));
    assert_eq!(
        retained_refs(&env),
        "",
        "every retention ref of the objective is gone"
    );
    assert_eq!(
        delete(&mut client, true).unwrap_err().code,
        ErrorCode::InvalidLifecycle
    );

    drop(client);
    service.shutdown();
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let restarted = detail(&mut client, &objective);
    assert_eq!(restarted.history_deleted, Some(true));
    assert_eq!(restarted.title, "History");
    drop(client);
    service.shutdown();
}
