//! Drafts and accepted inputs with their delivery state.

use lade_core::Revision;
use lade_core::domain::execution::ExecutionGeneration;
use lade_core::domain::feedback::{
    AcceptedInput, BlockedReason, DeliveryState, Draft, DraftRecord, DraftState, FeedbackItem,
    InputMode,
};
use lade_core::domain::ids::{DraftId, InputId, ObjectiveId, OperationId, SessionId};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

use super::{Sql, corrupt, count, rev, ts, u, uuid};
use crate::storage::reference;

fn parse_reference(value: &Value) -> Sql<lade_core::domain::inspection::Reference> {
    reference::decode(value).ok_or_else(|| corrupt("reference"))
}

/// Inserts or updates a Draft, also recording its revision when `record_revision` is set.
///
/// # Errors
///
/// Returns the SQLite error when a write fails, including the uniqueness violation for a revision
/// that is already recorded.
pub fn save_draft(conn: &Connection, draft: &Draft, record_revision: bool) -> Sql<()> {
    let r = draft.record();
    let reference = reference::encode(&r.reference).to_string();
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

/// Marks a Draft deleted; its revisions stay for history.
///
/// # Errors
///
/// Returns the SQLite error when the update fails.
pub fn delete_draft(conn: &Connection, id: DraftId, now: i64) -> Sql<()> {
    conn.execute(
        "UPDATE drafts SET deleted = 1, updated_at = ?2 WHERE id = ?1",
        params![id.to_string(), now],
    )?;
    Ok(())
}

/// Every Draft that is not deleted, oldest first.
///
/// # Errors
///
/// Returns the SQLite error when the query fails, or `InvalidColumnType` naming the field when a
/// stored id, Reference or state no longer parses.
pub fn load_drafts(conn: &Connection) -> Sql<Vec<Draft>> {
    let mut stmt = conn.prepare(
        "SELECT id, objective_id, destination_session_id, reference, body, revision, state,
            input_id, created_at, updated_at
         FROM drafts WHERE deleted = 0 ORDER BY created_at, id",
    )?;
    let rows = stmt.query_map([], |row| {
        let reference: String = row.get(3)?;
        let reference: Value =
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
            reference: parse_reference(&reference)?,
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

pub(super) fn parse_mode(code: &str) -> Sql<InputMode> {
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
                    "reference": reference::encode(&item.reference),
                    "body": item.body,
                })
            })
            .collect(),
    )
}

pub(super) fn parse_items(text: &str) -> Sql<Vec<FeedbackItem>> {
    let value: Value = serde_json::from_str(text).map_err(|_| corrupt("items"))?;
    let Value::Array(items) = value else {
        return Err(corrupt("items"));
    };
    items
        .into_iter()
        .map(|item| {
            let field = |name: &str| item.get(name).cloned().ok_or_else(|| corrupt("item"));
            Ok(FeedbackItem {
                draft_id: DraftId::new(uuid(field("draft_id")?.as_str().unwrap_or_default())?),
                draft_revision: Revision::new(field("draft_revision")?.as_u64().unwrap_or(1)),
                reference: parse_reference(&field("reference")?)?,
                body: field("body")?.as_str().unwrap_or_default().to_string(),
            })
        })
        .collect()
}

/// Records an accepted input with sequence number `seq`, and its initial delivery state.
///
/// # Errors
///
/// Returns the SQLite error when a write fails, including the uniqueness violation for an id or
/// operation id that is already recorded.
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
            input.mode.code(),
            items_json(&input.items).to_string(),
            input.note,
            input.rendered_text,
            input.accepted_at.unix_ms(),
            u(seq)
        ],
    )?;
    save_delivery(conn, input.id, &input.delivery, input.accepted_at.unix_ms())
}

/// Records the current delivery state of the accepted input `id`.
///
/// # Errors
///
/// Returns the SQLite error when the write fails, for example when the input is not recorded.
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

pub(super) fn parse_delivery(
    state: &str,
    generation: Option<i64>,
    reason: Option<&str>,
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
            reason: match reason {
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

/// Every accepted input with its sequence number and delivery state, in sequence order.
///
/// # Errors
///
/// Returns the SQLite error when the query fails, or `InvalidColumnType` naming the field when a
/// stored id, mode, item list or delivery state no longer parses.
pub fn load_inputs(conn: &Connection) -> Sql<Vec<(u64, AcceptedInput)>> {
    let mut stmt = conn.prepare(
        "SELECT a.id, a.operation_id, a.objective_id, a.session_id, a.association_revision,
            a.mode, a.items, a.note, a.rendered_text, a.accepted_at, a.seq, d.state, d.generation,
            d.blocked_reason
         FROM accepted_inputs a JOIN input_delivery d ON d.input_id = a.id ORDER BY a.seq",
    )?;
    let rows = stmt.query_map([], |row| {
        let seq = count(row.get(10)?, "seq")?;
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
                delivery: parse_delivery(
                    &row.get::<_, String>(11)?,
                    row.get(12)?,
                    row.get::<_, Option<String>>(13)?.as_deref(),
                )?,
            },
        ))
    })?;
    rows.collect()
}
