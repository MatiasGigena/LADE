//! Execution activity items shown in an Objective's activity page.

use lade_core::domain::ids::ObjectiveId;
use lade_protocol::types::{ActivityRole, Id};
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{Sql, corrupt, count, u, uuid};

#[derive(Clone, Debug)]
pub struct ActivityRow {
    pub item_id: Id,
    pub seq: u64,
    pub at: i64,
    pub role: ActivityRole,
    pub title: String,
    pub text: String,
    pub tool_name: Option<String>,
    pub is_error: bool,
    pub truncated: bool,
}

/// Records an activity item, or updates its text and flags when it is already recorded.
///
/// # Errors
///
/// Returns the SQLite error when the write fails, including the uniqueness violation for a sequence
/// number another item of the Objective already holds.
pub fn upsert_activity(conn: &Connection, objective: ObjectiveId, item: &ActivityRow) -> Sql<()> {
    conn.execute(
        "INSERT INTO activity (item_id, objective_id, seq, at, role, title, text, tool_name,
            is_error, truncated)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(item_id) DO UPDATE SET title = excluded.title, text = excluded.text,
            tool_name = excluded.tool_name, is_error = excluded.is_error,
            truncated = excluded.truncated",
        params![
            item.item_id.as_str(),
            objective.to_string(),
            u(item.seq),
            item.at,
            role_code(item.role),
            item.title,
            item.text,
            item.tool_name,
            item.is_error,
            item.truncated
        ],
    )?;
    Ok(())
}

fn role_code(role: ActivityRole) -> &'static str {
    match role {
        ActivityRole::User => "user",
        ActivityRole::Assistant => "assistant",
        ActivityRole::Thinking => "thinking",
        ActivityRole::Tool => "tool",
        ActivityRole::System => "system",
    }
}

fn parse_role(code: &str) -> Sql<ActivityRole> {
    Ok(match code {
        "user" => ActivityRole::User,
        "assistant" => ActivityRole::Assistant,
        "thinking" => ActivityRole::Thinking,
        "tool" => ActivityRole::Tool,
        "system" => ActivityRole::System,
        _ => return Err(corrupt("role")),
    })
}

fn activity_from_row(row: &Row<'_>) -> Sql<ActivityRow> {
    Ok(ActivityRow {
        item_id: Id::parse(row.get::<_, String>(0)?).map_err(|_| corrupt("item_id"))?,
        seq: count(row.get(1)?, "seq")?,
        at: row.get(2)?,
        role: parse_role(&row.get::<_, String>(3)?)?,
        title: row.get(4)?,
        text: row.get(5)?,
        tool_name: row.get(6)?,
        is_error: row.get(7)?,
        truncated: row.get(8)?,
    })
}

/// The activity item `item_id` and the Objective it belongs to, if recorded.
///
/// # Errors
///
/// Returns the SQLite error when the query fails, or `InvalidColumnType` when a stored id, role
/// or sequence number no longer parses.
pub fn load_activity_item(
    conn: &Connection,
    item_id: &Id,
) -> Sql<Option<(ObjectiveId, ActivityRow)>> {
    conn.query_row(
        "SELECT item_id, seq, at, role, title, text, tool_name, is_error, truncated, objective_id
         FROM activity WHERE item_id = ?1",
        params![item_id.as_str()],
        |row| {
            Ok((
                ObjectiveId::new(uuid(&row.get::<_, String>(9)?)?),
                activity_from_row(row)?,
            ))
        },
    )
    .optional()
}

/// Up to `limit` of `objective`'s activity items before sequence `before`, oldest first,
/// and whether older items remain.
///
/// # Errors
///
/// Returns the SQLite error when the query fails, or `InvalidColumnType` when a stored id, role
/// or sequence number no longer parses.
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

/// The highest activity sequence number recorded for each Objective.
///
/// # Errors
///
/// Returns the SQLite error when the query fails, or `InvalidColumnType` when a stored id or
/// sequence number no longer parses.
pub fn max_activity_seq(conn: &Connection) -> Sql<Vec<(ObjectiveId, u64)>> {
    let mut stmt =
        conn.prepare("SELECT objective_id, MAX(seq) FROM activity GROUP BY objective_id")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            ObjectiveId::new(uuid(&row.get::<_, String>(0)?)?),
            count(row.get(1)?, "seq")?,
        ))
    })?;
    rows.collect()
}
