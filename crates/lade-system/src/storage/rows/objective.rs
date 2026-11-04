//! Objectives and the Agent Sessions associated with them.

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use lade_core::domain::execution::{
    Execution, ExecutionGeneration, ExecutionState, ModelRef, SessionAssociation,
};
use lade_core::domain::ids::{GitAlgorithm, GitId, ObjectiveId, SessionId, WorkspaceId};
use lade_core::domain::objectives::{
    BranchName, Intent, Lifecycle, Objective, ObjectiveRecord, Title,
};
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{Sql, corrupt, rev, ts, u, uuid};

#[derive(Clone, Debug)]
pub struct ObjectiveRow {
    pub objective: Objective,
    pub git_branch: Option<String>,
    pub worktree: Option<PathBuf>,
    pub integration_conflict: bool,
    pub history_deleted: bool,
}

/// Inserts or updates an Objective together with its service-side columns.
///
/// # Errors
///
/// Returns the SQLite error when the write fails, for example when its Workspace is not recorded.
pub fn save_objective(conn: &Connection, row: &ObjectiveRow) -> Sql<()> {
    let r = row.objective.record();
    let (state, interrupted, reason) = match r.execution.state() {
        ExecutionState::Cancelled { interrupted } => ("cancelled", *interrupted, None),
        ExecutionState::Failed { reason } => ("failed", false, Some(reason.clone())),
        other => (other.code(), false, None),
    };
    let session = r.execution.session();
    conn.execute(
        "INSERT INTO objectives (id, workspace_id, title, outcome, constraints_text, acceptance,
            intent_revision, integration_target, start_algorithm, start_commit, branch, lifecycle,
            exec_state, exec_interrupted, exec_reason, exec_generation, session_id,
            association_revision, model_provider, model_id, cleaned_up, created_at, updated_at,
            revision, git_branch, worktree_path, integration_conflict, history_deleted)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
            ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28)
         ON CONFLICT(id) DO UPDATE SET title = excluded.title, outcome = excluded.outcome,
            constraints_text = excluded.constraints_text, acceptance = excluded.acceptance,
            intent_revision = excluded.intent_revision, lifecycle = excluded.lifecycle,
            exec_state = excluded.exec_state, exec_interrupted = excluded.exec_interrupted,
            exec_reason = excluded.exec_reason, exec_generation = excluded.exec_generation,
            session_id = excluded.session_id, association_revision = excluded.association_revision,
            model_provider = excluded.model_provider, model_id = excluded.model_id,
            cleaned_up = excluded.cleaned_up, updated_at = excluded.updated_at,
            revision = excluded.revision, git_branch = excluded.git_branch,
            worktree_path = excluded.worktree_path,
            integration_conflict = excluded.integration_conflict,
            history_deleted = excluded.history_deleted",
        params![
            r.id.to_string(),
            r.workspace_id.to_string(),
            r.title.as_str(),
            r.intent.outcome(),
            r.intent.constraints(),
            r.intent.acceptance(),
            u(r.intent_revision.value()),
            r.integration_target.as_str(),
            r.start_commit.algorithm().code(),
            r.start_commit.hex(),
            r.branch.as_str(),
            r.lifecycle.code(),
            state,
            interrupted,
            reason,
            u(r.execution.generation().value()),
            session.map(|s| s.session_id.to_string()),
            session.map(|s| u(s.association_revision.value())),
            session.map(|s| s.model.provider().to_string()),
            session.map(|s| s.model.id().to_string()),
            r.cleaned_up,
            r.created_at.unix_ms(),
            r.updated_at.unix_ms(),
            u(r.revision.value()),
            row.git_branch,
            row.worktree
                .as_ref()
                .map(|p| p.as_os_str().as_bytes().to_vec()),
            row.integration_conflict,
            row.history_deleted,
        ],
    )?;
    Ok(())
}

fn objective_from_row(row: &Row<'_>) -> Sql<ObjectiveRow> {
    let text = |i: usize| row.get::<_, String>(i);
    let intent = Intent::new(&text(3)?, &text(4)?, &text(5)?).map_err(|_| corrupt("intent"))?;
    let state = match text(12)?.as_str() {
        "none" => ExecutionState::None,
        "starting" => ExecutionState::Starting,
        "running" => ExecutionState::Running,
        "idle" => ExecutionState::Idle,
        "stopping" => ExecutionState::Stopping,
        "cancelled" => ExecutionState::Cancelled {
            interrupted: row.get(13)?,
        },
        "failed" => ExecutionState::Failed {
            reason: row.get::<_, Option<String>>(14)?.unwrap_or_default(),
        },
        _ => return Err(corrupt("execution state")),
    };
    let session = match row.get::<_, Option<String>>(16)? {
        Some(id) => Some(SessionAssociation {
            session_id: SessionId::new(uuid(&id)?),
            association_revision: rev(row.get::<_, Option<i64>>(17)?.unwrap_or(1))?,
            model: ModelRef::new(
                &row.get::<_, Option<String>>(18)?.unwrap_or_default(),
                &row.get::<_, Option<String>>(19)?.unwrap_or_default(),
            )
            .map_err(|_| corrupt("model"))?,
        }),
        None => None,
    };
    let generation = u64::try_from(row.get::<_, i64>(15)?).map_err(|_| corrupt("generation"))?;
    let execution = Execution::restore(state, ExecutionGeneration::new(generation), session)
        .map_err(|_| corrupt("execution"))?;
    let start_algorithm = match text(8)?.as_str() {
        "sha1" => GitAlgorithm::Sha1,
        "sha256" => GitAlgorithm::Sha256,
        _ => return Err(corrupt("start commit")),
    };
    let start = GitId::new(start_algorithm, &text(9)?).map_err(|_| corrupt("start commit"))?;
    let lifecycle = text(11)?;
    let lifecycle = Lifecycle::ALL
        .into_iter()
        .find(|l| l.code() == lifecycle)
        .ok_or_else(|| corrupt("lifecycle"))?;
    let record = ObjectiveRecord {
        id: ObjectiveId::new(uuid(&text(0)?)?),
        workspace_id: WorkspaceId::new(uuid(&text(1)?)?),
        title: Title::new(&text(2)?).map_err(|_| corrupt("title"))?,
        intent,
        intent_revision: rev(row.get(6)?)?,
        integration_target: BranchName::new(&text(7)?).map_err(|_| corrupt("target"))?,
        start_commit: start,
        branch: BranchName::new(&text(10)?).map_err(|_| corrupt("branch"))?,
        lifecycle,
        execution,
        cleaned_up: row.get(20)?,
        created_at: ts(row.get(21)?),
        updated_at: ts(row.get(22)?),
        revision: rev(row.get(23)?)?,
    };
    let worktree: Option<Vec<u8>> = row.get(25)?;
    Ok(ObjectiveRow {
        objective: Objective::restore(record).map_err(|_| corrupt("objective"))?,
        git_branch: row.get(24)?,
        worktree: worktree.map(|bytes| PathBuf::from(OsString::from_vec(bytes))),
        integration_conflict: row.get(26)?,
        history_deleted: row.get(27)?,
    })
}

/// Every recorded Objective, oldest first.
///
/// # Errors
///
/// Returns the SQLite error when the query fails, or `InvalidColumnType` naming the field when a
/// stored value no longer satisfies the domain rules (intent, branch names, lifecycle, execution).
pub fn load_objectives(conn: &Connection) -> Sql<Vec<ObjectiveRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, workspace_id, title, outcome, constraints_text, acceptance, intent_revision,
            integration_target, start_algorithm, start_commit, branch, lifecycle, exec_state,
            exec_interrupted, exec_reason, exec_generation, session_id, association_revision,
            model_provider, model_id, cleaned_up, created_at, updated_at, revision, git_branch,
            worktree_path, integration_conflict, history_deleted
         FROM objectives ORDER BY created_at, id",
    )?;
    let rows = stmt.query_map([], objective_from_row)?;
    rows.collect()
}

/// Records an Agent Session association, or advances its last execution generation.
///
/// # Errors
///
/// Returns the SQLite error when the write fails, for example when the Objective is not recorded.
pub fn save_session(
    conn: &Connection,
    objective: ObjectiveId,
    association: &SessionAssociation,
    generation: u64,
    now: i64,
) -> Sql<()> {
    conn.execute(
        "INSERT INTO sessions (session_id, objective_id, association_revision, model_provider,
            model_id, created_at, last_generation)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(session_id) DO UPDATE SET last_generation = excluded.last_generation",
        params![
            association.session_id.to_string(),
            objective.to_string(),
            u(association.association_revision.value()),
            association.model.provider(),
            association.model.id(),
            now,
            u(generation),
        ],
    )?;
    Ok(())
}

/// Stores the pi session id and session file the worker reported for `session`.
///
/// # Errors
///
/// Returns the SQLite error when the update fails.
pub fn record_pi_session(
    conn: &Connection,
    session: SessionId,
    pi_session_id: &str,
    session_file: &str,
) -> Sql<()> {
    conn.execute(
        "UPDATE sessions SET pi_session_id = ?2, session_file = ?3 WHERE session_id = ?1",
        params![session.to_string(), pi_session_id, session_file],
    )?;
    Ok(())
}

/// The pi session file recorded for `session`, if the session exists and has one.
///
/// # Errors
///
/// Returns the SQLite error when the query fails.
pub fn pi_session_file(conn: &Connection, session: SessionId) -> Sql<Option<String>> {
    conn.query_row(
        "SELECT session_file FROM sessions WHERE session_id = ?1",
        params![session.to_string()],
        |row| row.get::<_, Option<String>>(0),
    )
    .optional()
    .map(Option::flatten)
}
