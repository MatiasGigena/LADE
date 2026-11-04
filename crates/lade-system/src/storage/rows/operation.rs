//! The client operation journal that makes retried requests idempotent.

use lade_protocol::client::{Outcome, RequestName};
use lade_protocol::types::Id;
use rusqlite::{Connection, OptionalExtension, params};

use super::{Sql, corrupt};

/// Where a journaled client operation stands. `Uncertain` means its outcome could not be
/// recorded (or the service restarted first), so the client must re-read state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationState {
    Pending,
    Uncertain,
    Completed,
}

impl OperationState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Uncertain => "uncertain",
            Self::Completed => "completed",
        }
    }

    fn parse(code: &str) -> Sql<Self> {
        Ok(match code {
            "pending" => Self::Pending,
            "uncertain" => Self::Uncertain,
            "completed" => Self::Completed,
            _ => return Err(corrupt("operation state")),
        })
    }
}

/// Identifies one client operation in the journal: its id, the digest of the request that
/// created it, and the request name.
#[derive(Clone, Copy, Debug)]
pub struct OperationKey<'a> {
    pub id: &'a Id,
    pub digest: &'a str,
    pub name: RequestName,
}

#[derive(Clone, Debug)]
pub struct OperationRow {
    pub digest: String,
    pub state: OperationState,
    pub outcome: Option<Outcome>,
}

/// The journal entry for `operation_id`, if the operation was seen before.
///
/// # Errors
///
/// Returns the SQLite error when the query fails, or `InvalidColumnType` when the stored state or
/// outcome no longer parses.
pub fn load_operation(conn: &Connection, operation_id: &Id) -> Sql<Option<OperationRow>> {
    conn.query_row(
        "SELECT digest, state, outcome FROM operations WHERE operation_id = ?1",
        params![operation_id.as_str()],
        |row| {
            let outcome: Option<String> = row.get(2)?;
            Ok(OperationRow {
                digest: row.get(0)?,
                state: OperationState::parse(&row.get::<_, String>(1)?)?,
                outcome: outcome
                    .map(|text| serde_json::from_str(&text).map_err(|_| corrupt("outcome")))
                    .transpose()?,
            })
        },
    )
    .optional()
}

/// Records or updates a client operation's state and outcome.
///
/// # Errors
///
/// Returns the SQLite error when the write fails, or `InvalidColumnType` when the outcome cannot be
/// serialized.
pub fn record_operation(
    conn: &Connection,
    key: OperationKey<'_>,
    state: OperationState,
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
        params![
            key.id.as_str(),
            key.digest,
            key.name.as_str(),
            state.as_str(),
            outcome,
            now
        ],
    )?;
    Ok(())
}

#[derive(Clone, Debug)]
pub struct PendingOperation {
    pub id: Id,
    pub digest: String,
    /// `None` when the stored request name is no longer part of the protocol.
    pub name: Option<RequestName>,
}

/// Every operation still recorded as pending, for restart recovery.
///
/// # Errors
///
/// Returns the SQLite error when the query fails, or `InvalidColumnType` when a stored operation id
/// is not a UUID.
pub fn pending_operations(conn: &Connection) -> Sql<Vec<PendingOperation>> {
    let mut stmt =
        conn.prepare("SELECT operation_id, digest, name FROM operations WHERE state = 'pending'")?;
    let rows = stmt.query_map([], |row| {
        Ok(PendingOperation {
            id: Id::parse(row.get::<_, String>(0)?).map_err(|_| corrupt("operation id"))?,
            digest: row.get(1)?,
            name: RequestName::parse(&row.get::<_, String>(2)?),
        })
    })?;
    rows.collect()
}
