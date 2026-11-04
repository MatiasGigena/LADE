use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use lade_core::Revision;
use lade_core::domain::execution::{
    Execution, ExecutionGeneration, ExecutionState, ModelRef, SessionAssociation,
};
use lade_core::domain::feedback::{
    AcceptedInput, BlockedReason, DeliveryState, Draft, DraftRecord, DraftState, FeedbackItem,
    InputMode,
};
use lade_core::domain::ids::{
    ApprovalId, CandidateId, CaptureId, DraftId, GitAlgorithm, GitId, InputId, ObjectiveId,
    OperationId, SessionId, Timestamp, Uuid, VerificationId, WorkspaceId,
};
use lade_core::domain::inspection::{ArtifactDigest, Reference};
use lade_core::domain::objectives::{
    BranchName, Intent, Lifecycle, NewObjective, Objective, ObjectiveRecord, Title,
};
use lade_core::domain::review::{
    Approval, ApprovalState, Candidate, CandidateStatus, ChangeCounts, InvalidationReason,
    Provenance, Review, Verification, VerificationOutcome,
};
use lade_protocol::client::{self as wire, ErrorCode, Outcome, Rejection, RequestName, SafeAction};
use lade_protocol::types::Id;
use rusqlite::{Connection, params};
use serde_json::json;

use super::*;
use crate::storage::{memory, reference};

const SHA1: &str = "0123456789abcdef0123456789abcdef01234567";
const SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn id(n: u8) -> Uuid {
    Uuid::from_random_bytes([n; 16])
}

fn sha1(fill: char) -> GitId {
    GitId::new(GitAlgorithm::Sha1, &fill.to_string().repeat(40)).expect("sha1")
}

fn workspace(conn: &Connection, n: u8) -> WorkspaceId {
    let id = WorkspaceId::new(id(n));
    insert_workspace(
        conn,
        &WorkspaceRow {
            id,
            path: PathBuf::from(format!("/repo/{n}")),
            name: format!("w{n}"),
            default_branch: Some("main".into()),
            opened_at: i64::from(n),
        },
    )
    .expect("workspace");
    id
}

fn defined(workspace_id: WorkspaceId, n: u8, created: i64) -> Objective {
    Objective::define(
        NewObjective {
            id: ObjectiveId::new(id(n)),
            workspace_id,
            title: Title::new(&format!("objective {n}")).expect("title"),
            intent: Intent::new("outcome", "constraints", "acceptance").expect("intent"),
            integration_target: BranchName::new("main").expect("branch"),
            start_commit: sha1('a'),
        },
        Timestamp::from_unix_ms(created),
    )
}

fn row(objective: Objective) -> ObjectiveRow {
    ObjectiveRow {
        objective,
        git_branch: None,
        worktree: None,
        integration_conflict: false,
        history_deleted: false,
    }
}

/// A recorded Objective (in a fresh Workspace) that other rows can reference.
fn objective(conn: &Connection, n: u8) -> ObjectiveId {
    let objective = defined(workspace(conn, n), n, i64::from(n));
    let id = objective.id();
    save_objective(conn, &row(objective)).expect("objective");
    id
}

fn association(n: u8) -> SessionAssociation {
    SessionAssociation {
        session_id: SessionId::new(id(n)),
        association_revision: Revision::new(2),
        model: ModelRef::new("provider", "model").expect("model"),
    }
}

/// Asserts that `result` is the corrupt-value error naming `what`.
fn assert_corrupt<T: std::fmt::Debug>(result: Sql<T>, what: &str) {
    match result {
        Err(rusqlite::Error::InvalidColumnType(_, name, _)) => assert_eq!(name, what),
        other => panic!("expected corrupt {what}, got {other:?}"),
    }
}

fn reference_to(capture: Uuid) -> Reference {
    reference::decode(&json!({
        "artifact_digest": SHA256,
        "path": {"b64": "c3JjL2EucnM=", "display": "src/a.rs"},
        "version": {"kind": "working", "capture_id": capture.to_string()},
        "side": "new",
        "byte_start": "0",
        "byte_end": "3",
        "selected_text": "abc",
    }))
    .expect("reference")
}

#[test]
fn git_ids_are_told_apart_by_length_and_bad_hex_is_corrupt() {
    assert_eq!(git(SHA1).expect("sha1").algorithm(), GitAlgorithm::Sha1);
    assert_eq!(
        git(SHA256).expect("sha256").algorithm(),
        GitAlgorithm::Sha256
    );
    assert_corrupt(git("abc"), "git id");
    assert_corrupt(git(&"z".repeat(40)), "git id");
}

#[test]
fn numeric_columns_reject_negatives_and_saturate_on_write() {
    assert_eq!(count(7, "seq").expect("count"), 7);
    assert_corrupt(count(-1, "seq"), "seq");
    assert_eq!(rev(3).expect("revision"), Revision::new(3));
    assert_corrupt(rev(-1), "revision");
    assert_eq!(u(u64::MAX), i64::MAX);
    assert_eq!(u(5), 5);
    assert_corrupt(uuid("not a uuid"), "uuid");
}

#[test]
fn settings_are_absent_until_saved_then_replaced_and_corrupt_json_is_reported() {
    let conn = memory();
    assert!(load_settings(&conn).expect("load").is_none());
    let mut settings = wire::Settings {
        verification_commands: vec![vec!["cargo".into(), "test".into()]],
        default_model: None,
    };
    save_settings(&conn, &settings).expect("save");
    assert_eq!(load_settings(&conn).expect("load"), Some(settings.clone()));
    settings.verification_commands.clear();
    save_settings(&conn, &settings).expect("replace");
    assert_eq!(load_settings(&conn).expect("load"), Some(settings));
    conn.execute("UPDATE settings SET json = '{' WHERE id = 1", [])
        .expect("corrupt");
    assert_corrupt(load_settings(&conn), "settings");
}

#[test]
fn workspaces_keep_non_utf8_paths_and_reject_a_second_row_for_a_path() {
    let conn = memory();
    let path = PathBuf::from(OsString::from_vec(b"/repo/\xff".to_vec()));
    let first = WorkspaceRow {
        id: WorkspaceId::new(id(1)),
        path: path.clone(),
        name: "w".into(),
        default_branch: None,
        opened_at: 20,
    };
    insert_workspace(&conn, &first).expect("insert");
    workspace(&conn, 2);
    let found = workspace_by_path(&conn, &path)
        .expect("query")
        .expect("found");
    assert_eq!(
        (found.id, found.path, found.opened_at),
        (first.id, path, 20)
    );
    assert!(
        workspace_by_path(&conn, PathBuf::from("/elsewhere").as_path())
            .expect("query")
            .is_none()
    );
    let order: Vec<_> = load_workspaces(&conn)
        .expect("load")
        .into_iter()
        .map(|w| w.opened_at)
        .collect();
    assert_eq!(order, [2, 20], "oldest first");
    let duplicate = WorkspaceRow {
        id: WorkspaceId::new(id(3)),
        ..first
    };
    assert!(insert_workspace(&conn, &duplicate).is_err());
}

#[test]
fn objectives_round_trip_their_execution_and_service_columns() {
    let conn = memory();
    let ws = workspace(&conn, 1);
    let mut record: ObjectiveRecord = defined(ws, 1, 5).into_record();
    record.lifecycle = Lifecycle::Active;
    record.execution = Execution::restore(
        ExecutionState::Failed {
            reason: "worker exited".into(),
        },
        ExecutionGeneration::new(3),
        Some(association(9)),
    )
    .expect("execution");
    let worktree = PathBuf::from(OsString::from_vec(b"/wt/\xfe".to_vec()));
    let saved = ObjectiveRow {
        objective: Objective::restore(record).expect("objective"),
        git_branch: Some("lade/x".into()),
        worktree: Some(worktree.clone()),
        integration_conflict: true,
        history_deleted: true,
    };
    save_objective(&conn, &saved).expect("save");
    save_objective(&conn, &row(defined(ws, 2, 1))).expect("save second");

    let loaded = load_objectives(&conn).expect("load");
    assert_eq!(loaded.len(), 2);
    assert_eq!(
        loaded[0].objective.id(),
        ObjectiveId::new(id(2)),
        "oldest first"
    );
    let back = &loaded[1];
    assert_eq!(back.objective, saved.objective);
    assert_eq!(back.git_branch.as_deref(), Some("lade/x"));
    assert_eq!(back.worktree.as_ref(), Some(&worktree));
    assert!(back.integration_conflict && back.history_deleted);
}

#[test]
fn saving_an_objective_again_updates_it_in_place() {
    let conn = memory();
    let ws = workspace(&conn, 1);
    let mut record = defined(ws, 1, 5).into_record();
    save_objective(&conn, &row(Objective::restore(record.clone()).expect("o"))).expect("save");
    record.title = Title::new("renamed").expect("title");
    record.lifecycle = Lifecycle::Discarded;
    record.cleaned_up = true;
    save_objective(&conn, &row(Objective::restore(record.clone()).expect("o"))).expect("update");
    let loaded = load_objectives(&conn).expect("load");
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].objective.record(), &record);
}

#[test]
fn objectives_with_corrupt_columns_are_reported_by_field() {
    let conn = memory();
    let id = objective(&conn, 1);
    let corrupt_with = |sql: &str, what: &str| {
        let tx = conn.unchecked_transaction().expect("tx");
        tx.execute(sql, params![id.to_string()]).expect("corrupt");
        assert_corrupt(load_objectives(&tx), what);
        tx.rollback().expect("rollback");
    };
    corrupt_with(
        "UPDATE objectives SET lifecycle = 'lost' WHERE id = ?1",
        "lifecycle",
    );
    corrupt_with(
        "UPDATE objectives SET exec_state = 'lost' WHERE id = ?1",
        "execution state",
    );
    corrupt_with(
        "UPDATE objectives SET start_algorithm = 'md5' WHERE id = ?1",
        "start commit",
    );
    corrupt_with("UPDATE objectives SET title = '' WHERE id = ?1", "title");
    corrupt_with(
        "UPDATE objectives SET branch = 'other' WHERE id = ?1",
        "objective",
    );
    corrupt_with(
        "UPDATE objectives SET exec_state = 'running' WHERE id = ?1",
        "execution",
    );
    assert_eq!(load_objectives(&conn).expect("intact").len(), 1);
}

#[test]
fn sessions_record_their_pi_file_and_advance_only_the_generation() {
    let conn = memory();
    let objective = objective(&conn, 1);
    let session = association(4);
    save_session(&conn, objective, &session, 1, 10).expect("save");
    assert_eq!(
        pi_session_file(&conn, session.session_id).expect("file"),
        None
    );
    record_pi_session(&conn, session.session_id, "pi-1", "/s/1.jsonl").expect("record");
    save_session(&conn, objective, &session, 4, 99).expect("advance");
    let (generation, created, file): (i64, i64, String) = conn
        .query_row(
            "SELECT last_generation, created_at, session_file FROM sessions",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("row");
    assert_eq!((generation, created), (4, 10));
    assert_eq!(file, "/s/1.jsonl");
    assert_eq!(
        pi_session_file(&conn, SessionId::new(id(77))).expect("unknown"),
        None
    );
}

fn activity(item: &str, seq: u64) -> ActivityRow {
    ActivityRow {
        item_id: item.into(),
        seq,
        at: i64::try_from(seq).expect("at"),
        role: "assistant".into(),
        title: "t".into(),
        text: format!("text {seq}"),
        tool_name: None,
        is_error: false,
        truncated: false,
    }
}

#[test]
fn activity_pages_run_backwards_from_a_cursor_and_report_older_items() {
    let conn = memory();
    let objective = objective(&conn, 1);
    for seq in 1..=5 {
        upsert_activity(&conn, objective, &activity(&format!("i{seq}"), seq)).expect("upsert");
    }
    let seqs = |page: &[ActivityRow]| page.iter().map(|a| a.seq).collect::<Vec<_>>();
    let (page, more) = activity_page(&conn, objective, None, 2).expect("latest");
    assert_eq!((seqs(&page), more), (vec![4, 5], true));
    let (page, more) = activity_page(&conn, objective, Some(4), 2).expect("older");
    assert_eq!((seqs(&page), more), (vec![2, 3], true));
    let (page, more) = activity_page(&conn, objective, Some(2), 2).expect("oldest");
    assert_eq!((seqs(&page), more), (vec![1], false));
    let (page, more) = activity_page(&conn, objective, None, 0).expect("empty");
    assert!(page.is_empty() && more);
}

#[test]
fn activity_upserts_replace_the_text_but_keep_the_sequence() {
    let conn = memory();
    let objective = objective(&conn, 1);
    let other = self::objective(&conn, 2);
    upsert_activity(&conn, objective, &activity("a", 1)).expect("insert");
    let mut updated = activity("a", 1);
    updated.text = "final".into();
    updated.tool_name = Some("bash".into());
    updated.is_error = true;
    updated.truncated = true;
    upsert_activity(&conn, objective, &updated).expect("update");
    let (owner, item) = load_activity_item(&conn, "a")
        .expect("load")
        .expect("found");
    assert_eq!(owner, objective);
    assert_eq!(item.text, "final");
    assert_eq!(item.tool_name.as_deref(), Some("bash"));
    assert!(item.is_error && item.truncated);
    assert!(
        load_activity_item(&conn, "missing")
            .expect("load")
            .is_none()
    );
    assert!(
        upsert_activity(&conn, objective, &activity("b", 1)).is_err(),
        "a sequence number is unique per Objective"
    );
    upsert_activity(&conn, other, &activity("c", 9)).expect("other objective");
    let mut max = max_activity_seq(&conn).expect("max");
    max.sort_by_key(|(_, seq)| *seq);
    assert_eq!(max, [(objective, 1), (other, 9)]);
}

fn capture(n: u8, objective: ObjectiveId, created_at: i64) -> CaptureRow {
    CaptureRow {
        id: CaptureId::new(id(n)),
        objective_id: objective,
        tree: git(SHA256).expect("tree"),
        target_commit: sha1('b'),
        counts: ChangeCounts {
            files: 2,
            added: 10,
            removed: 3,
        },
        retention_ref: format!("refs/lade/capture/{n}"),
        created_at,
    }
}

#[test]
fn latest_captures_pick_the_newest_per_objective_and_break_ties_by_insertion() {
    let conn = memory();
    let first = objective(&conn, 1);
    let second = objective(&conn, 2);
    for row in [
        capture(10, first, 5),
        capture(11, first, 9),
        capture(12, first, 9),
        capture(13, first, 1),
        capture(20, second, 3),
    ] {
        insert_capture(&conn, &row).expect("insert");
    }
    let mut latest: Vec<_> = latest_captures(&conn)
        .expect("latest")
        .into_iter()
        .map(|c| c.id)
        .collect();
    latest.sort_by_key(ToString::to_string);
    let mut expected = vec![CaptureId::new(id(12)), CaptureId::new(id(20))];
    expected.sort_by_key(ToString::to_string);
    assert_eq!(latest, expected);
    assert_eq!(
        capture_retention_refs(&conn, first).expect("refs"),
        ["10", "11", "12", "13"].map(|n| format!("refs/lade/capture/{n}"))
    );
    let loaded = load_capture(&conn, CaptureId::new(id(20)))
        .expect("load")
        .expect("found");
    assert_eq!(loaded.tree.algorithm(), GitAlgorithm::Sha256);
    assert_eq!((loaded.counts.files, loaded.counts.added), (2, 10));
    assert!(
        load_capture(&conn, CaptureId::new(id(99)))
            .expect("load")
            .is_none()
    );
    assert!(
        insert_capture(&conn, &capture(30, ObjectiveId::new(id(99)), 0)).is_err(),
        "a capture needs a recorded Objective"
    );
}

fn journal(n: u8, objective: ObjectiveId) -> JournalRow {
    JournalRow {
        id: id(n),
        objective_id: objective,
        approval_id: ApprovalId::new(id(n + 1)),
        candidate_id: CandidateId::new(id(n + 2)),
        target_branch: "main".into(),
        expected_target: sha1('c'),
        candidate_tree: sha1('d'),
        state: JournalState::Intent,
        result_commit: None,
        message: "Squash".into(),
        commit_time: 42,
    }
}

#[test]
fn only_intent_journal_entries_stay_open_oldest_first() {
    let conn = memory();
    let objective = objective(&conn, 1);
    insert_journal(&conn, &journal(10, objective), 2).expect("second");
    insert_journal(&conn, &journal(20, objective), 1).expect("first");
    insert_journal(&conn, &journal(30, objective), 3).expect("third");
    update_journal(&conn, id(30), JournalState::Done, Some(&sha1('e')), None, 4).expect("done");
    let open = open_journal(&conn).expect("open");
    assert_eq!(
        open.iter().map(|j| j.id).collect::<Vec<_>>(),
        [id(20), id(10)]
    );
    assert_eq!(open[0].message, "Squash");
    assert_eq!(open[0].commit_time, 42);
    assert_eq!(open[0].state, JournalState::Intent);
    conn.execute(
        "UPDATE integration_journal SET result_commit = 'xyz' WHERE id = ?1",
        params![id(20).to_string()],
    )
    .expect("corrupt");
    assert_corrupt(open_journal(&conn), "git id");
}

#[test]
fn journal_states_round_trip_through_their_codes() {
    for state in [
        JournalState::Intent,
        JournalState::Done,
        JournalState::Failed,
        JournalState::ReconciledDone,
        JournalState::ReconciledAborted,
    ] {
        assert_eq!(JournalState::parse(state.as_str()).expect("parse"), state);
    }
    assert_corrupt(JournalState::parse("pending"), "journal state");
    for state in [
        OperationState::Pending,
        OperationState::Uncertain,
        OperationState::Completed,
    ] {
        assert_eq!(OperationState::parse(state.as_str()).expect("parse"), state);
    }
    assert_corrupt(OperationState::parse("done"), "operation state");
}

#[test]
fn operations_keep_their_digest_and_leave_pending_once_completed() {
    let conn = memory();
    let first = Id::from_random_bytes([1; 16]);
    let second = Id::from_random_bytes([2; 16]);
    let key = |id| OperationKey {
        id,
        digest: "digest",
        name: RequestName::SettingsUpdate,
    };
    assert!(load_operation(&conn, &first).expect("load").is_none());
    record_operation(&conn, key(&first), OperationState::Pending, None, 1).expect("pending");
    record_operation(&conn, key(&second), OperationState::Pending, None, 1).expect("pending");
    let outcome = Outcome::Rejected {
        name: RequestName::SettingsUpdate,
        rejection: Rejection::new(ErrorCode::Interrupted, SafeAction::Refresh, "restarted"),
    };
    let done = OperationKey {
        digest: "other digest",
        ..key(&first)
    };
    record_operation(&conn, done, OperationState::Completed, Some(&outcome), 2).expect("done");
    let row = load_operation(&conn, &first).expect("load").expect("found");
    assert_eq!(row.digest, "digest", "the first digest is kept");
    assert_eq!(row.state, OperationState::Completed);
    assert_eq!(row.outcome, Some(outcome));
    let pending = pending_operations(&conn).expect("pending");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, second);
    assert_eq!(pending[0].name, Some(RequestName::SettingsUpdate));
    conn.execute("UPDATE operations SET name = 'gone.request'", [])
        .expect("rename");
    assert_eq!(pending_operations(&conn).expect("pending")[0].name, None);
    conn.execute("UPDATE operations SET outcome = '[]'", [])
        .expect("corrupt");
    assert_corrupt(load_operation(&conn, &first), "outcome");
}

fn candidate(n: u8, objective: ObjectiveId, ordinal: u32, status: CandidateStatus) -> Candidate {
    Candidate {
        id: CandidateId::new(id(n)),
        objective_id: objective,
        ordinal,
        commit: sha1('1'),
        tree: sha1('2'),
        base_target_commit: sha1('3'),
        manifest_digest: ArtifactDigest::parse(SHA256).expect("digest"),
        changes: ChangeCounts {
            files: 1,
            added: 2,
            removed: 3,
        },
        created_at: Timestamp::from_unix_ms(i64::from(ordinal)),
        status,
    }
}

fn verification(n: u8, candidate: CandidateId, outcome: VerificationOutcome) -> Verification {
    Verification {
        id: VerificationId::new(id(n)),
        candidate_id: candidate,
        argv: vec!["cargo".into(), "test".into()],
        cwd_label: ".".into(),
        started_at: Timestamp::from_unix_ms(1),
        ended_at: None,
        exit_code: None,
        outcome,
        output_artifact: None,
        truncated: false,
        provenance: Provenance::ServiceObserved,
    }
}

fn approval(n: u8, candidate: CandidateId, state: ApprovalState) -> Approval {
    Approval {
        id: ApprovalId::new(id(n)),
        candidate_id: candidate,
        target_commit: sha1('4'),
        override_reason: Some("trusted".into()),
        state,
        approved_at: Timestamp::from_unix_ms(7),
    }
}

#[test]
fn reviews_round_trip_and_later_saves_update_only_mutable_state() {
    let conn = memory();
    let objective = objective(&conn, 1);
    let old = candidate(10, objective, 1, CandidateStatus::Invalidated);
    let current = candidate(11, objective, 2, CandidateStatus::Current);
    let review = Review::restore(
        objective,
        vec![old.clone(), current.clone()],
        vec![
            verification(20, old.id, VerificationOutcome::Passed),
            verification(21, current.id, VerificationOutcome::Running),
        ],
        vec![
            approval(
                30,
                old.id,
                ApprovalState::Invalidated {
                    reason: InvalidationReason::TargetMoved,
                },
            ),
            approval(31, current.id, ApprovalState::Valid),
        ],
    )
    .expect("review");
    save_review(&conn, &review).expect("save");
    assert_eq!(load_review(&conn, objective).expect("load"), review);

    let mut finished = verification(21, current.id, VerificationOutcome::Failed);
    finished.ended_at = Some(Timestamp::from_unix_ms(9));
    finished.exit_code = Some(1);
    finished.output_artifact = Some(ArtifactDigest::parse(SHA256).expect("digest"));
    finished.truncated = true;
    let mut moved = current.clone();
    moved.status = CandidateStatus::Historical;
    moved.ordinal = 5;
    let mut consumed = approval(31, current.id, ApprovalState::Consumed);
    consumed.override_reason = None;
    let updated = Review::restore(
        objective,
        vec![old.clone(), moved],
        vec![
            verification(20, old.id, VerificationOutcome::Passed),
            finished.clone(),
        ],
        vec![review.approvals()[0].clone(), consumed],
    )
    .expect("updated");
    save_review(&conn, &updated).expect("update");
    let loaded = load_review(&conn, objective).expect("reload");
    assert_eq!(loaded.candidates()[1].status, CandidateStatus::Historical);
    assert_eq!(
        loaded.candidates()[1].ordinal,
        2,
        "the ordinal is immutable"
    );
    assert_eq!(loaded.verifications()[1], finished);
    assert_eq!(loaded.approvals()[1].state, ApprovalState::Consumed);
    assert_eq!(
        loaded.approvals()[1].override_reason.as_deref(),
        Some("trusted"),
        "the override reason is immutable"
    );
    assert_eq!(
        load_review(&conn, self::objective(&conn, 2)).expect("empty"),
        Review::new(ObjectiveId::new(id(2)))
    );
}

#[test]
fn reviews_with_corrupt_rows_are_reported_by_field() {
    let conn = memory();
    let objective = objective(&conn, 1);
    let current = candidate(10, objective, 1, CandidateStatus::Current);
    let review = Review::restore(
        objective,
        vec![current.clone()],
        vec![verification(20, current.id, VerificationOutcome::Running)],
        vec![approval(30, current.id, ApprovalState::Valid)],
    )
    .expect("review");
    save_review(&conn, &review).expect("save");
    for (sql, what) in [
        ("UPDATE candidates SET status = 'lost'", "candidate status"),
        ("UPDATE candidates SET manifest_digest = 'x'", "manifest"),
        (
            "UPDATE verifications SET outcome = 'lost'",
            "verification outcome",
        ),
        ("UPDATE verifications SET argv = 'x'", "argv"),
        ("UPDATE approvals SET state = 'lost'", "approval state"),
        (
            "UPDATE approvals SET state = 'invalidated', invalidation_reason = NULL",
            "invalidation reason",
        ),
    ] {
        let tx = conn.unchecked_transaction().expect("tx");
        tx.execute(sql, []).expect("corrupt");
        assert_corrupt(load_review(&tx, objective), what);
        tx.rollback().expect("rollback");
    }
}

#[test]
fn verification_tails_are_stored_per_verification() {
    let conn = memory();
    let objective = objective(&conn, 1);
    let current = candidate(10, objective, 1, CandidateStatus::Current);
    let review = Review::restore(
        objective,
        vec![current.clone()],
        vec![verification(20, current.id, VerificationOutcome::Passed)],
        Vec::new(),
    )
    .expect("review");
    save_review(&conn, &review).expect("save");
    assert_eq!(
        load_verification_tails(&conn).expect("tails"),
        [(VerificationId::new(id(20)), String::new())]
    );
    set_verification_tail(&conn, VerificationId::new(id(20)), "ok\n").expect("tail");
    assert_eq!(
        load_verification_tails(&conn).expect("tails"),
        [(VerificationId::new(id(20)), "ok\n".to_owned())]
    );
}

fn draft(n: u8, objective: ObjectiveId, revision: u64, state: DraftState) -> Draft {
    Draft::restore(DraftRecord {
        id: DraftId::new(id(n)),
        objective_id: objective,
        destination_session_id: SessionId::new(id(50)),
        reference: reference_to(id(60)),
        body: format!("body {revision}"),
        revision: Revision::new(revision),
        state,
        created_at: Timestamp::from_unix_ms(i64::from(n)),
        updated_at: Timestamp::from_unix_ms(i64::try_from(revision).expect("rev")),
    })
    .expect("draft")
}

#[test]
fn drafts_record_each_revision_once_and_deleted_drafts_are_hidden() {
    let conn = memory();
    let objective = objective(&conn, 1);
    save_draft(&conn, &draft(10, objective, 1, DraftState::Unsent), true).expect("create");
    save_draft(&conn, &draft(11, objective, 1, DraftState::Unsent), true).expect("second");
    let submitted = DraftState::Submitted {
        input_id: InputId::new(id(70)),
    };
    let revised = draft(10, objective, 2, submitted);
    save_draft(&conn, &revised, false).expect("update without revision");
    assert!(
        save_draft(&conn, &draft(11, objective, 1, DraftState::Unsent), true).is_err(),
        "a revision is recorded once"
    );
    let revisions: i64 = conn
        .query_row("SELECT COUNT(*) FROM draft_revisions", [], |r| r.get(0))
        .expect("count");
    assert_eq!(revisions, 2);
    assert_eq!(
        load_drafts(&conn).expect("load"),
        [revised, draft(11, objective, 1, DraftState::Unsent)]
    );
    delete_draft(&conn, DraftId::new(id(10)), 9).expect("delete");
    assert_eq!(load_drafts(&conn).expect("load").len(), 1);
    conn.execute("UPDATE drafts SET state = 'submitted', input_id = NULL", [])
        .expect("corrupt");
    assert_corrupt(load_drafts(&conn), "draft state");
    conn.execute("UPDATE drafts SET state = 'unsent', reference = '{}'", [])
        .expect("corrupt");
    assert_corrupt(load_drafts(&conn), "reference");
}

fn input(n: u8, objective: ObjectiveId, delivery: DeliveryState) -> AcceptedInput {
    AcceptedInput {
        id: InputId::new(id(n)),
        operation_id: OperationId::new(id(n + 100)),
        objective_id: objective,
        session_id: SessionId::new(id(50)),
        association_revision: Revision::new(3),
        mode: InputMode::FollowUp,
        items: vec![FeedbackItem {
            draft_id: DraftId::new(id(10)),
            draft_revision: Revision::new(2),
            reference: reference_to(id(60)),
            body: "fix this".into(),
        }],
        note: Some("note".into()),
        rendered_text: "rendered".into(),
        accepted_at: Timestamp::from_unix_ms(i64::from(n)),
        delivery,
    }
}

#[test]
fn accepted_inputs_load_in_sequence_with_their_latest_delivery() {
    let conn = memory();
    let objective = objective(&conn, 1);
    let blocked = DeliveryState::Blocked {
        reason: BlockedReason::StaleReference,
    };
    insert_input(&conn, &input(2, objective, DeliveryState::Queued), 9).expect("later");
    insert_input(&conn, &input(1, objective, blocked), 4).expect("earlier");
    let dispatched = DeliveryState::Dispatched {
        generation: ExecutionGeneration::new(6),
    };
    save_delivery(&conn, InputId::new(id(2)), &dispatched, 20).expect("deliver");
    let loaded = load_inputs(&conn).expect("load");
    assert_eq!(
        loaded,
        [
            (4, input(1, objective, blocked)),
            (9, input(2, objective, dispatched))
        ]
    );
    assert!(
        conn.execute("UPDATE accepted_inputs SET note = NULL", [])
            .is_err(),
        "accepted inputs are immutable"
    );
    assert!(
        insert_input(&conn, &input(2, objective, DeliveryState::Queued), 10).is_err(),
        "an input is accepted once"
    );
}

#[test]
fn deliveries_parse_every_state_and_reject_inconsistent_rows() {
    let generation = ExecutionGeneration::new(2);
    for state in [
        DeliveryState::Queued,
        DeliveryState::Dispatched { generation },
        DeliveryState::Received { generation },
        DeliveryState::Uncertain { generation },
    ] {
        assert_eq!(
            feedback::parse_delivery(state.code(), Some(2), None).expect("parse"),
            state
        );
    }
    for reason in [
        BlockedReason::ExecutionUnavailable,
        BlockedReason::StaleAssociation,
        BlockedReason::StaleReference,
        BlockedReason::InvalidLifecycle,
    ] {
        assert_eq!(
            feedback::parse_delivery("blocked", None, Some(reason.code())).expect("parse"),
            DeliveryState::Blocked { reason }
        );
    }
    assert_corrupt(
        feedback::parse_delivery("dispatched", None, None),
        "delivery generation",
    );
    assert_corrupt(
        feedback::parse_delivery("received", Some(-1), None),
        "delivery generation",
    );
    assert_corrupt(
        feedback::parse_delivery("blocked", None, Some("busy")),
        "blocked reason",
    );
    assert_corrupt(feedback::parse_delivery("lost", None, None), "delivery");
}

#[test]
fn feedback_items_reject_a_non_array_or_an_item_missing_a_field() {
    assert_corrupt(feedback::parse_items("{}"), "items");
    assert_corrupt(feedback::parse_items("not json"), "items");
    assert_corrupt(feedback::parse_items(r#"[{"body": "x"}]"#), "item");
    assert!(feedback::parse_items("[]").expect("empty").is_empty());
    for mode in [InputMode::Prompt, InputMode::Steer, InputMode::FollowUp] {
        assert_eq!(feedback::parse_mode(mode.code()).expect("mode"), mode);
    }
    assert_corrupt(feedback::parse_mode("shout"), "mode");
}
