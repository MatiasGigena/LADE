//! Row mapping between the product tables and domain records. Every loaded record is validated by
//! the domain's own `restore`; an inconsistent row surfaces as a corruption error, never a reset.

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
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
    ApprovalId, CandidateId, CaptureId, DraftId, GitId, InputId, ObjectiveId, OperationId,
    SessionId, Timestamp, Uuid, VerificationId, WorkspaceId,
};
use lade_core::domain::inspection::ArtifactDigest;
use lade_core::domain::objectives::{BranchName, Intent, Objective, ObjectiveRecord, Title};
use lade_core::domain::review::{
    Approval, ApprovalState, Candidate, CandidateStatus, ChangeCounts, InvalidationReason,
    Provenance, Review, Verification, VerificationOutcome,
};
use lade_protocol::client::{self as wire, Outcome};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::{Value, json};

use crate::service::convert::{core_reference, hex_git, parse_lifecycle, wire_reference};

type Sql<T> = rusqlite::Result<T>;

fn corrupt(what: &'static str) -> rusqlite::Error {
    rusqlite::Error::InvalidColumnType(0, what.to_string(), rusqlite::types::Type::Null)
}

fn uuid(text: &str) -> Sql<Uuid> {
    Uuid::parse(text).map_err(|_| corrupt("uuid"))
}

fn git(text: &str) -> Sql<GitId> {
    hex_git(text).ok_or_else(|| corrupt("git id"))
}

fn rev(value: i64) -> Sql<Revision> {
    u64::try_from(value)
        .map(Revision::new)
        .map_err(|_| corrupt("revision"))
}

fn u(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn ts(value: i64) -> Timestamp {
    Timestamp::from_unix_ms(value)
}

// ---------------------------------------------------------------------------------------------
// Settings.

pub fn load_settings(conn: &Connection) -> Sql<Option<wire::Settings>> {
    let json: Option<String> = conn
        .query_row("SELECT json FROM settings WHERE id = 1", [], |row| {
            row.get(0)
        })
        .optional()?;
    json.map(|text| serde_json::from_str(&text).map_err(|_| corrupt("settings")))
        .transpose()
}

pub fn save_settings(conn: &Connection, settings: &wire::Settings) -> Sql<()> {
    let text = serde_json::to_string(settings).map_err(|_| corrupt("settings"))?;
    conn.execute(
        "INSERT INTO settings (id, json) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET json = excluded.json",
        params![text],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Workspaces.

#[derive(Clone, Debug)]
pub struct WorkspaceRow {
    pub id: WorkspaceId,
    pub path: PathBuf,
    pub name: String,
    pub default_branch: Option<String>,
    pub opened_at: i64,
}

pub fn insert_workspace(conn: &Connection, row: &WorkspaceRow) -> Sql<()> {
    conn.execute(
        "INSERT INTO workspaces (id, path, name, default_branch, opened_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            row.id.to_string(),
            row.path.as_os_str().as_bytes(),
            row.name,
            row.default_branch,
            row.opened_at
        ],
    )?;
    Ok(())
}

pub fn load_workspaces(conn: &Connection) -> Sql<Vec<WorkspaceRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, name, default_branch, opened_at FROM workspaces ORDER BY opened_at, id",
    )?;
    let rows = stmt.query_map([], |row| {
        let path: Vec<u8> = row.get(1)?;
        Ok(WorkspaceRow {
            id: WorkspaceId::new(uuid(&row.get::<_, String>(0)?)?),
            path: PathBuf::from(OsString::from_vec(path)),
            name: row.get(2)?,
            default_branch: row.get(3)?,
            opened_at: row.get(4)?,
        })
    })?;
    rows.collect()
}

// ---------------------------------------------------------------------------------------------
// Objectives.

/// The stored Objective plus service-only columns.
#[derive(Clone, Debug)]
pub struct ObjectiveRow {
    pub objective: Objective,
    /// The real Git branch (differs from `lade/<short-id>` after a name collision).
    pub git_branch: Option<String>,
    pub worktree: Option<PathBuf>,
    pub integration_conflict: bool,
    /// The retention refs and branch were deleted by `objective.delete_history`.
    pub history_deleted: bool,
}

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
    let start = crate::service::convert::stored_git(&text(8)?, &text(9)?)
        .ok_or_else(|| corrupt("start commit"))?;
    let record = ObjectiveRecord {
        id: ObjectiveId::new(uuid(&text(0)?)?),
        workspace_id: WorkspaceId::new(uuid(&text(1)?)?),
        title: Title::new(&text(2)?).map_err(|_| corrupt("title"))?,
        intent,
        intent_revision: rev(row.get(6)?)?,
        integration_target: BranchName::new(&text(7)?).map_err(|_| corrupt("target"))?,
        start_commit: start,
        branch: BranchName::new(&text(10)?).map_err(|_| corrupt("branch"))?,
        lifecycle: parse_lifecycle(&text(11)?).ok_or_else(|| corrupt("lifecycle"))?,
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

// ---------------------------------------------------------------------------------------------
// Sessions.

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

// ---------------------------------------------------------------------------------------------
// Captures.

#[derive(Clone, Debug)]
pub struct CaptureRow {
    pub id: CaptureId,
    pub objective_id: ObjectiveId,
    pub tree: GitId,
    pub target_commit: GitId,
    pub counts: ChangeCounts,
    pub retention_ref: String,
    pub created_at: i64,
}

pub fn insert_capture(conn: &Connection, row: &CaptureRow) -> Sql<()> {
    conn.execute(
        "INSERT INTO captures (id, objective_id, tree, target_commit, files, added, removed,
            retention_ref, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            row.id.to_string(),
            row.objective_id.to_string(),
            row.tree.hex(),
            row.target_commit.hex(),
            row.counts.files,
            u(row.counts.added),
            u(row.counts.removed),
            row.retention_ref,
            row.created_at
        ],
    )?;
    Ok(())
}

fn capture_from_row(row: &Row<'_>) -> Sql<CaptureRow> {
    Ok(CaptureRow {
        id: CaptureId::new(uuid(&row.get::<_, String>(0)?)?),
        objective_id: ObjectiveId::new(uuid(&row.get::<_, String>(1)?)?),
        tree: git(&row.get::<_, String>(2)?)?,
        target_commit: git(&row.get::<_, String>(3)?)?,
        counts: ChangeCounts {
            files: row.get(4)?,
            added: u64::try_from(row.get::<_, i64>(5)?).unwrap_or(0),
            removed: u64::try_from(row.get::<_, i64>(6)?).unwrap_or(0),
        },
        retention_ref: row.get(7)?,
        created_at: row.get(8)?,
    })
}

const CAPTURE_COLUMNS: &str =
    "id, objective_id, tree, target_commit, files, added, removed, retention_ref, created_at";

pub fn load_capture(conn: &Connection, id: CaptureId) -> Sql<Option<CaptureRow>> {
    conn.query_row(
        &format!("SELECT {CAPTURE_COLUMNS} FROM captures WHERE id = ?1"),
        params![id.to_string()],
        capture_from_row,
    )
    .optional()
}

/// Every retention ref recorded for the captures of `objective`.
pub fn capture_retention_refs(conn: &Connection, objective: ObjectiveId) -> Sql<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT retention_ref FROM captures WHERE objective_id = ?1 ORDER BY rowid")?;
    let rows = stmt.query_map(params![objective.to_string()], |row| row.get(0))?;
    rows.collect()
}

pub fn latest_captures(conn: &Connection) -> Sql<Vec<CaptureRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {CAPTURE_COLUMNS} FROM captures c WHERE rowid = (
            SELECT rowid FROM captures WHERE objective_id = c.objective_id
            ORDER BY created_at DESC, rowid DESC LIMIT 1)"
    ))?;
    let rows = stmt.query_map([], capture_from_row)?;
    rows.collect()
}

// ---------------------------------------------------------------------------------------------
// Drafts.

fn reference_json(draft: &DraftRecord) -> Sql<String> {
    serde_json::to_string(&wire_reference(&draft.reference)).map_err(|_| corrupt("reference"))
}

/// Upserts the current draft row and appends its revision to the history.
pub fn save_draft(conn: &Connection, draft: &Draft, record_revision: bool) -> Sql<()> {
    let r = draft.record();
    let reference = reference_json(r)?;
    let (state, input) = match r.state {
        DraftState::Unsent => ("unsent", None),
        DraftState::Submitted { input_id } => ("submitted", Some(input_id.to_string())),
    };
    conn.execute(
        "INSERT INTO drafts (id, objective_id, destination_session_id, reference, body, revision,
            state, input_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(id) DO UPDATE SET destination_session_id = excluded.destination_session_id,
            reference = excluded.reference, body = excluded.body, revision = excluded.revision,
            state = excluded.state, input_id = excluded.input_id, updated_at = excluded.updated_at",
        params![
            r.id.to_string(),
            r.objective_id.to_string(),
            r.destination_session_id.to_string(),
            reference,
            r.body,
            u(r.revision.value()),
            state,
            input,
            r.created_at.unix_ms(),
            r.updated_at.unix_ms()
        ],
    )?;
    if record_revision {
        conn.execute(
            "INSERT INTO draft_revisions (draft_id, revision, reference, body, saved_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                r.id.to_string(),
                u(r.revision.value()),
                reference,
                r.body,
                r.updated_at.unix_ms()
            ],
        )?;
    }
    Ok(())
}

pub fn delete_draft(conn: &Connection, id: DraftId, now: i64) -> Sql<()> {
    conn.execute(
        "UPDATE drafts SET deleted = 1, updated_at = ?2 WHERE id = ?1",
        params![id.to_string(), now],
    )?;
    Ok(())
}

pub fn load_drafts(conn: &Connection) -> Sql<Vec<Draft>> {
    let mut stmt = conn.prepare(
        "SELECT id, objective_id, destination_session_id, reference, body, revision, state,
            input_id, created_at, updated_at
         FROM drafts WHERE deleted = 0 ORDER BY created_at, id",
    )?;
    let rows = stmt.query_map([], |row| {
        let reference: String = row.get(3)?;
        let reference: wire::Reference =
            serde_json::from_str(&reference).map_err(|_| corrupt("reference"))?;
        let state = match (
            row.get::<_, String>(6)?.as_str(),
            row.get::<_, Option<String>>(7)?,
        ) {
            ("unsent", _) => DraftState::Unsent,
            ("submitted", Some(input)) => DraftState::Submitted {
                input_id: InputId::new(uuid(&input)?),
            },
            _ => return Err(corrupt("draft state")),
        };
        let record = DraftRecord {
            id: DraftId::new(uuid(&row.get::<_, String>(0)?)?),
            objective_id: ObjectiveId::new(uuid(&row.get::<_, String>(1)?)?),
            destination_session_id: SessionId::new(uuid(&row.get::<_, String>(2)?)?),
            reference: core_reference(&reference).map_err(|_| corrupt("reference"))?,
            body: row.get(4)?,
            revision: rev(row.get(5)?)?,
            state,
            created_at: ts(row.get(8)?),
            updated_at: ts(row.get(9)?),
        };
        Draft::restore(record).map_err(|_| corrupt("draft"))
    })?;
    rows.collect()
}

// ---------------------------------------------------------------------------------------------
// Accepted inputs and delivery.

fn mode_code(mode: InputMode) -> &'static str {
    mode.code()
}

fn parse_mode(code: &str) -> Sql<InputMode> {
    match code {
        "prompt" => Ok(InputMode::Prompt),
        "steer" => Ok(InputMode::Steer),
        "follow_up" => Ok(InputMode::FollowUp),
        _ => Err(corrupt("mode")),
    }
}

fn items_json(items: &[FeedbackItem]) -> Value {
    Value::Array(
        items
            .iter()
            .map(|item| {
                json!({
                    "draft_id": item.draft_id.to_string(),
                    "draft_revision": item.draft_revision.value(),
                    "reference": serde_json::to_value(wire_reference(&item.reference))
                        .unwrap_or(Value::Null),
                    "body": item.body,
                })
            })
            .collect(),
    )
}

fn parse_items(text: &str) -> Sql<Vec<FeedbackItem>> {
    let value: Value = serde_json::from_str(text).map_err(|_| corrupt("items"))?;
    let Value::Array(items) = value else {
        return Err(corrupt("items"));
    };
    items
        .into_iter()
        .map(|item| {
            let field = |name: &str| item.get(name).cloned().ok_or_else(|| corrupt("item"));
            let reference: wire::Reference =
                serde_json::from_value(field("reference")?).map_err(|_| corrupt("item"))?;
            Ok(FeedbackItem {
                draft_id: DraftId::new(uuid(field("draft_id")?.as_str().unwrap_or_default())?),
                draft_revision: Revision::new(field("draft_revision")?.as_u64().unwrap_or(1)),
                reference: core_reference(&reference).map_err(|_| corrupt("item"))?,
                body: field("body")?.as_str().unwrap_or_default().to_string(),
            })
        })
        .collect()
}

pub fn insert_input(conn: &Connection, input: &AcceptedInput, seq: u64) -> Sql<()> {
    conn.execute(
        "INSERT INTO accepted_inputs (id, operation_id, objective_id, session_id,
            association_revision, mode, items, note, rendered_text, accepted_at, seq)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            input.id.to_string(),
            input.operation_id.to_string(),
            input.objective_id.to_string(),
            input.session_id.to_string(),
            u(input.association_revision.value()),
            mode_code(input.mode),
            items_json(&input.items).to_string(),
            input.note,
            input.rendered_text,
            input.accepted_at.unix_ms(),
            u(seq)
        ],
    )?;
    save_delivery(conn, input.id, &input.delivery, input.accepted_at.unix_ms())
}

pub fn save_delivery(
    conn: &Connection,
    id: InputId,
    delivery: &DeliveryState,
    now: i64,
) -> Sql<()> {
    let (generation, reason) = match delivery {
        DeliveryState::Queued => (None, None),
        DeliveryState::Dispatched { generation }
        | DeliveryState::Received { generation }
        | DeliveryState::Uncertain { generation } => (Some(u(generation.value())), None),
        DeliveryState::Blocked { reason } => (None, Some(reason.code())),
    };
    conn.execute(
        "INSERT INTO input_delivery (input_id, state, generation, blocked_reason, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(input_id) DO UPDATE SET state = excluded.state,
            generation = excluded.generation, blocked_reason = excluded.blocked_reason,
            updated_at = excluded.updated_at",
        params![id.to_string(), delivery.code(), generation, reason, now],
    )?;
    Ok(())
}

fn parse_delivery(
    state: &str,
    generation: Option<i64>,
    reason: Option<String>,
) -> Sql<DeliveryState> {
    let generation = || {
        generation
            .and_then(|g| u64::try_from(g).ok())
            .map(ExecutionGeneration::new)
            .ok_or_else(|| corrupt("delivery generation"))
    };
    Ok(match state {
        "queued" => DeliveryState::Queued,
        "dispatched" => DeliveryState::Dispatched {
            generation: generation()?,
        },
        "received" => DeliveryState::Received {
            generation: generation()?,
        },
        "uncertain" => DeliveryState::Uncertain {
            generation: generation()?,
        },
        "blocked" => DeliveryState::Blocked {
            reason: match reason.as_deref() {
                Some("execution_unavailable") => BlockedReason::ExecutionUnavailable,
                Some("stale_association") => BlockedReason::StaleAssociation,
                Some("stale_reference") => BlockedReason::StaleReference,
                Some("invalid_lifecycle") => BlockedReason::InvalidLifecycle,
                _ => return Err(corrupt("blocked reason")),
            },
        },
        _ => return Err(corrupt("delivery")),
    })
}

/// All accepted inputs in acceptance order, with their sequence numbers.
pub fn load_inputs(conn: &Connection) -> Sql<Vec<(u64, AcceptedInput)>> {
    let mut stmt = conn.prepare(
        "SELECT a.id, a.operation_id, a.objective_id, a.session_id, a.association_revision,
            a.mode, a.items, a.note, a.rendered_text, a.accepted_at, a.seq, d.state, d.generation,
            d.blocked_reason
         FROM accepted_inputs a JOIN input_delivery d ON d.input_id = a.id ORDER BY a.seq",
    )?;
    let rows = stmt.query_map([], |row| {
        let seq = u64::try_from(row.get::<_, i64>(10)?).map_err(|_| corrupt("seq"))?;
        Ok((
            seq,
            AcceptedInput {
                id: InputId::new(uuid(&row.get::<_, String>(0)?)?),
                operation_id: OperationId::new(uuid(&row.get::<_, String>(1)?)?),
                objective_id: ObjectiveId::new(uuid(&row.get::<_, String>(2)?)?),
                session_id: SessionId::new(uuid(&row.get::<_, String>(3)?)?),
                association_revision: rev(row.get(4)?)?,
                mode: parse_mode(&row.get::<_, String>(5)?)?,
                items: parse_items(&row.get::<_, String>(6)?)?,
                note: row.get(7)?,
                rendered_text: row.get(8)?,
                accepted_at: ts(row.get(9)?),
                delivery: parse_delivery(&row.get::<_, String>(11)?, row.get(12)?, row.get(13)?)?,
            },
        ))
    })?;
    rows.collect()
}

// ---------------------------------------------------------------------------------------------
// Operations journal.

#[derive(Clone, Debug)]
pub struct OperationRow {
    pub digest: String,
    pub name: String,
    /// `pending` | `completed` | `uncertain`.
    pub state: String,
    pub outcome: Option<Outcome>,
}

pub fn load_operation(conn: &Connection, operation_id: &str) -> Sql<Option<OperationRow>> {
    conn.query_row(
        "SELECT digest, name, state, outcome FROM operations WHERE operation_id = ?1",
        params![operation_id],
        |row| {
            let outcome: Option<String> = row.get(3)?;
            Ok(OperationRow {
                digest: row.get(0)?,
                name: row.get(1)?,
                state: row.get(2)?,
                outcome: outcome
                    .map(|text| serde_json::from_str(&text).map_err(|_| corrupt("outcome")))
                    .transpose()?,
            })
        },
    )
    .optional()
}

pub fn record_operation(
    conn: &Connection,
    operation_id: &str,
    digest: &str,
    name: &str,
    state: &str,
    outcome: Option<&Outcome>,
    now: i64,
) -> Sql<()> {
    let outcome = outcome
        .map(|o| serde_json::to_string(o).map_err(|_| corrupt("outcome")))
        .transpose()?;
    conn.execute(
        "INSERT INTO operations (operation_id, digest, name, state, outcome, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
         ON CONFLICT(operation_id) DO UPDATE SET state = excluded.state,
            outcome = excluded.outcome, updated_at = excluded.updated_at",
        params![operation_id, digest, name, state, outcome, now],
    )?;
    Ok(())
}

/// Operations left `pending` by a previous service instance.
pub fn pending_operations(conn: &Connection) -> Sql<Vec<(String, String, String)>> {
    let mut stmt =
        conn.prepare("SELECT operation_id, digest, name FROM operations WHERE state = 'pending'")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    rows.collect()
}

// ---------------------------------------------------------------------------------------------
// Review records.

fn candidate_status(code: &str) -> Sql<CandidateStatus> {
    Ok(match code {
        "current" => CandidateStatus::Current,
        "historical" => CandidateStatus::Historical,
        "invalidated" => CandidateStatus::Invalidated,
        _ => return Err(corrupt("candidate status")),
    })
}

fn verification_outcome(code: &str) -> Sql<VerificationOutcome> {
    Ok(match code {
        "running" => VerificationOutcome::Running,
        "passed" => VerificationOutcome::Passed,
        "failed" => VerificationOutcome::Failed,
        "interrupted" => VerificationOutcome::Interrupted,
        "error" => VerificationOutcome::Error,
        _ => return Err(corrupt("verification outcome")),
    })
}

fn invalidation(code: Option<String>) -> Sql<InvalidationReason> {
    Ok(match code.as_deref() {
        Some("returned_for_revision") => InvalidationReason::ReturnedForRevision,
        Some("target_moved") => InvalidationReason::TargetMoved,
        Some("candidate_changed") => InvalidationReason::CandidateChanged,
        Some("service_restart") => InvalidationReason::ServiceRestart,
        Some("discarded") => InvalidationReason::Discarded,
        _ => return Err(corrupt("invalidation reason")),
    })
}

/// Upserts every candidate, verification and approval of `review` (small per-Objective sets).
pub fn save_review(conn: &Connection, review: &Review) -> Sql<()> {
    let objective = review.objective_id().to_string();
    for c in review.candidates() {
        conn.execute(
            "INSERT INTO candidates (id, objective_id, ordinal, commit_id, tree, base_target_commit,
                manifest_digest, files, added, removed, created_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET status = excluded.status",
            params![
                c.id.to_string(),
                objective,
                c.ordinal,
                c.commit.hex(),
                c.tree.hex(),
                c.base_target_commit.hex(),
                c.manifest_digest.hex(),
                c.changes.files,
                u(c.changes.added),
                u(c.changes.removed),
                c.created_at.unix_ms(),
                c.status.code()
            ],
        )?;
    }
    for (seq, v) in review.verifications().iter().enumerate() {
        let argv = serde_json::to_string(&v.argv).map_err(|_| corrupt("argv"))?;
        conn.execute(
            "INSERT INTO verifications (id, objective_id, candidate_id, argv, cwd_label, started_at,
                ended_at, exit_code, outcome, output_artifact, truncated, seq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET ended_at = excluded.ended_at,
                exit_code = excluded.exit_code, outcome = excluded.outcome,
                output_artifact = excluded.output_artifact, truncated = excluded.truncated",
            params![
                v.id.to_string(),
                objective,
                v.candidate_id.to_string(),
                argv,
                v.cwd_label,
                v.started_at.unix_ms(),
                v.ended_at.map(Timestamp::unix_ms),
                v.exit_code,
                v.outcome.code(),
                v.output_artifact.as_ref().map(ArtifactDigest::hex),
                v.truncated,
                u(seq as u64)
            ],
        )?;
    }
    for (seq, a) in review.approvals().iter().enumerate() {
        let (state, reason) = match a.state {
            ApprovalState::Valid => ("valid", None),
            ApprovalState::Consumed => ("consumed", None),
            ApprovalState::Invalidated { reason } => ("invalidated", Some(reason.code())),
        };
        conn.execute(
            "INSERT INTO approvals (id, objective_id, candidate_id, target_commit, override_reason,
                state, invalidation_reason, approved_at, seq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET state = excluded.state,
                invalidation_reason = excluded.invalidation_reason",
            params![
                a.id.to_string(),
                objective,
                a.candidate_id.to_string(),
                a.target_commit.hex(),
                a.override_reason,
                state,
                reason,
                a.approved_at.unix_ms(),
                u(seq as u64)
            ],
        )?;
    }
    Ok(())
}

pub fn set_verification_tail(conn: &Connection, id: VerificationId, tail: &str) -> Sql<()> {
    conn.execute(
        "UPDATE verifications SET output_tail = ?2 WHERE id = ?1",
        params![id.to_string(), tail],
    )?;
    Ok(())
}

/// Output tails of every verification (keyed by id).
pub fn load_verification_tails(conn: &Connection) -> Sql<Vec<(VerificationId, String)>> {
    let mut stmt = conn.prepare("SELECT id, output_tail FROM verifications")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            VerificationId::new(uuid(&row.get::<_, String>(0)?)?),
            row.get(1)?,
        ))
    })?;
    rows.collect()
}

pub fn load_review(conn: &Connection, objective: ObjectiveId) -> Sql<Review> {
    let id = objective.to_string();
    let mut stmt = conn.prepare(
        "SELECT id, ordinal, commit_id, tree, base_target_commit, manifest_digest, files, added,
            removed, created_at, status
         FROM candidates WHERE objective_id = ?1 ORDER BY ordinal",
    )?;
    let candidates = stmt
        .query_map(params![id], |row| {
            Ok(Candidate {
                id: CandidateId::new(uuid(&row.get::<_, String>(0)?)?),
                objective_id: objective,
                ordinal: row.get(1)?,
                commit: git(&row.get::<_, String>(2)?)?,
                tree: git(&row.get::<_, String>(3)?)?,
                base_target_commit: git(&row.get::<_, String>(4)?)?,
                manifest_digest: ArtifactDigest::parse(&row.get::<_, String>(5)?)
                    .map_err(|_| corrupt("manifest"))?,
                changes: ChangeCounts {
                    files: row.get(6)?,
                    added: u64::try_from(row.get::<_, i64>(7)?).unwrap_or(0),
                    removed: u64::try_from(row.get::<_, i64>(8)?).unwrap_or(0),
                },
                created_at: ts(row.get(9)?),
                status: candidate_status(&row.get::<_, String>(10)?)?,
            })
        })?
        .collect::<Sql<Vec<_>>>()?;
    let mut stmt = conn.prepare(
        "SELECT id, candidate_id, argv, cwd_label, started_at, ended_at, exit_code, outcome,
            output_artifact, truncated
         FROM verifications WHERE objective_id = ?1 ORDER BY seq",
    )?;
    let verifications = stmt
        .query_map(params![id], |row| {
            let argv: String = row.get(2)?;
            Ok(Verification {
                id: VerificationId::new(uuid(&row.get::<_, String>(0)?)?),
                candidate_id: CandidateId::new(uuid(&row.get::<_, String>(1)?)?),
                argv: serde_json::from_str(&argv).map_err(|_| corrupt("argv"))?,
                cwd_label: row.get(3)?,
                started_at: ts(row.get(4)?),
                ended_at: row.get::<_, Option<i64>>(5)?.map(ts),
                exit_code: row.get(6)?,
                outcome: verification_outcome(&row.get::<_, String>(7)?)?,
                output_artifact: row
                    .get::<_, Option<String>>(8)?
                    .map(|hex| ArtifactDigest::parse(&hex).map_err(|_| corrupt("artifact")))
                    .transpose()?,
                truncated: row.get(9)?,
                provenance: Provenance::ServiceObserved,
            })
        })?
        .collect::<Sql<Vec<_>>>()?;
    let mut stmt = conn.prepare(
        "SELECT id, candidate_id, target_commit, override_reason, state, invalidation_reason,
            approved_at
         FROM approvals WHERE objective_id = ?1 ORDER BY seq",
    )?;
    let approvals = stmt
        .query_map(params![id], |row| {
            let state = match row.get::<_, String>(4)?.as_str() {
                "valid" => ApprovalState::Valid,
                "consumed" => ApprovalState::Consumed,
                "invalidated" => ApprovalState::Invalidated {
                    reason: invalidation(row.get(5)?)?,
                },
                _ => return Err(corrupt("approval state")),
            };
            Ok(Approval {
                id: ApprovalId::new(uuid(&row.get::<_, String>(0)?)?),
                candidate_id: CandidateId::new(uuid(&row.get::<_, String>(1)?)?),
                target_commit: git(&row.get::<_, String>(2)?)?,
                override_reason: row.get(3)?,
                state,
                approved_at: ts(row.get(6)?),
            })
        })?
        .collect::<Sql<Vec<_>>>()?;
    Review::restore(objective, candidates, verifications, approvals).map_err(|_| corrupt("review"))
}

// ---------------------------------------------------------------------------------------------
// Activity.

/// One stored activity item (wire-shaped values, unix-ms time).
#[derive(Clone, Debug)]
pub struct ActivityRow {
    pub item_id: String,
    pub seq: u64,
    pub at: i64,
    pub role: String,
    pub title: String,
    pub text: String,
    pub tool_name: Option<String>,
    pub is_error: bool,
    pub truncated: bool,
}

pub fn upsert_activity(conn: &Connection, objective: ObjectiveId, item: &ActivityRow) -> Sql<()> {
    conn.execute(
        "INSERT INTO activity (item_id, objective_id, seq, at, role, title, text, tool_name,
            is_error, truncated)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(item_id) DO UPDATE SET title = excluded.title, text = excluded.text,
            tool_name = excluded.tool_name, is_error = excluded.is_error,
            truncated = excluded.truncated",
        params![
            item.item_id,
            objective.to_string(),
            u(item.seq),
            item.at,
            item.role,
            item.title,
            item.text,
            item.tool_name,
            item.is_error,
            item.truncated
        ],
    )?;
    Ok(())
}

fn activity_from_row(row: &Row<'_>) -> Sql<ActivityRow> {
    Ok(ActivityRow {
        item_id: row.get(0)?,
        seq: u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
        at: row.get(2)?,
        role: row.get(3)?,
        title: row.get(4)?,
        text: row.get(5)?,
        tool_name: row.get(6)?,
        is_error: row.get(7)?,
        truncated: row.get(8)?,
    })
}

pub fn load_activity_item(
    conn: &Connection,
    item_id: &str,
) -> Sql<Option<(ObjectiveId, ActivityRow)>> {
    conn.query_row(
        "SELECT item_id, seq, at, role, title, text, tool_name, is_error, truncated, objective_id
         FROM activity WHERE item_id = ?1",
        params![item_id],
        |row| {
            Ok((
                ObjectiveId::new(uuid(&row.get::<_, String>(9)?)?),
                activity_from_row(row)?,
            ))
        },
    )
    .optional()
}

/// Up to `limit` items with `seq < before` (newest last), and whether older items exist.
pub fn activity_page(
    conn: &Connection,
    objective: ObjectiveId,
    before: Option<u64>,
    limit: u32,
) -> Sql<(Vec<ActivityRow>, bool)> {
    let before = before.map_or(i64::MAX, u);
    let mut stmt = conn.prepare(
        "SELECT item_id, seq, at, role, title, text, tool_name, is_error, truncated
         FROM activity WHERE objective_id = ?1 AND seq < ?2 ORDER BY seq DESC LIMIT ?3",
    )?;
    let mut items = stmt
        .query_map(
            params![objective.to_string(), before, i64::from(limit) + 1],
            activity_from_row,
        )?
        .collect::<Sql<Vec<_>>>()?;
    let has_more = items.len() > limit as usize;
    items.truncate(limit as usize);
    items.reverse();
    Ok((items, has_more))
}

pub fn max_activity_seq(conn: &Connection) -> Sql<Vec<(ObjectiveId, u64)>> {
    let mut stmt =
        conn.prepare("SELECT objective_id, MAX(seq) FROM activity GROUP BY objective_id")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            ObjectiveId::new(uuid(&row.get::<_, String>(0)?)?),
            u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
        ))
    })?;
    rows.collect()
}

// ---------------------------------------------------------------------------------------------
// Integration journal.

#[derive(Clone, Debug)]
pub struct JournalRow {
    pub id: String,
    pub objective_id: ObjectiveId,
    pub approval_id: ApprovalId,
    pub candidate_id: CandidateId,
    pub target_branch: String,
    pub expected_target: GitId,
    pub candidate_tree: GitId,
    /// `intent` | `done` | `failed` | `reconciled_done` | `reconciled_aborted`.
    pub state: String,
    pub result_commit: Option<GitId>,
}

/// The intended squash (message and commit time) is recorded so a restart can recompute the
/// exact commit and recognise a completed integration.
pub fn insert_journal(
    conn: &Connection,
    row: &JournalRow,
    message: &str,
    commit_time: i64,
    now: i64,
) -> Sql<()> {
    conn.execute(
        "INSERT INTO integration_journal (id, objective_id, approval_id, candidate_id,
            target_branch, expected_target, candidate_tree, state, created_at, updated_at,
            message, commit_time)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10, ?11)",
        params![
            row.id,
            row.objective_id.to_string(),
            row.approval_id.to_string(),
            row.candidate_id.to_string(),
            row.target_branch,
            row.expected_target.hex(),
            row.candidate_tree.hex(),
            row.state,
            now,
            message,
            commit_time
        ],
    )?;
    Ok(())
}

pub fn update_journal(
    conn: &Connection,
    id: &str,
    state: &str,
    result: Option<&GitId>,
    detail: Option<&str>,
    now: i64,
) -> Sql<()> {
    conn.execute(
        "UPDATE integration_journal SET state = ?2, result_commit = ?3, detail = ?4,
            updated_at = ?5 WHERE id = ?1",
        params![id, state, result.map(GitId::hex), detail, now],
    )?;
    Ok(())
}

/// Journal entries whose outcome was never recorded, with their message and commit time.
pub fn open_journal(conn: &Connection) -> Sql<Vec<(JournalRow, String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT id, objective_id, approval_id, candidate_id, target_branch, expected_target,
            candidate_tree, state, result_commit, message, commit_time
         FROM integration_journal WHERE state = 'intent' ORDER BY created_at",
    )?;
    let rows = stmt.query_map([], |row| {
        let journal = JournalRow {
            id: row.get(0)?,
            objective_id: ObjectiveId::new(uuid(&row.get::<_, String>(1)?)?),
            approval_id: ApprovalId::new(uuid(&row.get::<_, String>(2)?)?),
            candidate_id: CandidateId::new(uuid(&row.get::<_, String>(3)?)?),
            target_branch: row.get(4)?,
            expected_target: git(&row.get::<_, String>(5)?)?,
            candidate_tree: git(&row.get::<_, String>(6)?)?,
            state: row.get(7)?,
            result_commit: row
                .get::<_, Option<String>>(8)?
                .map(|hex| git(&hex))
                .transpose()?,
        };
        Ok((journal, row.get(9)?, row.get(10)?))
    })?;
    rows.collect()
}
