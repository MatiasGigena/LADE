//! Per-Objective session preferences (the chosen thinking level) and the developer's
//! approve/reject decisions for executable profile extensions.

use lade_core::domain::ids::{ObjectiveId, Uuid};
use lade_protocol::types::ThinkingLevel;
use rusqlite::{Connection, params};

use super::rows::{Sql, corrupt};

/// Records the thinking level chosen for `objective`.
///
/// # Errors
///
/// Returns the SQLite error when the write fails, for example when the Objective is not recorded.
pub fn save_thinking(
    conn: &Connection,
    objective: ObjectiveId,
    level: ThinkingLevel,
    now: i64,
) -> Sql<()> {
    conn.execute(
        "INSERT INTO session_preferences (objective_id, thinking, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(objective_id) DO UPDATE SET thinking = excluded.thinking,
            updated_at = excluded.updated_at",
        params![objective.to_string(), level.as_str(), now],
    )?;
    Ok(())
}

/// The thinking level recorded for each Objective.
///
/// # Errors
///
/// Returns the SQLite error when the query fails, or `InvalidColumnType` when a stored id or level
/// no longer parses.
pub fn load_thinking(conn: &Connection) -> Sql<Vec<(ObjectiveId, ThinkingLevel)>> {
    let mut stmt = conn.prepare("SELECT objective_id, thinking FROM session_preferences")?;
    let rows = stmt.query_map([], |row| {
        let id = Uuid::parse(&row.get::<_, String>(0)?).map_err(|_| corrupt("uuid"))?;
        let level = ThinkingLevel::parse(&row.get::<_, String>(1)?)
            .ok_or_else(|| corrupt("thinking level"))?;
        Ok((ObjectiveId::new(id), level))
    })?;
    rows.collect()
}

/// One extension decision, keyed by the extension's name and the SHA-256 of its content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalRow {
    pub name: String,
    pub sha256: String,
    pub approved: bool,
}

/// Records the developer's decision for one extension version.
///
/// # Errors
///
/// Returns the SQLite error when the write fails.
pub fn save_approval(conn: &Connection, row: &ApprovalRow, now: i64) -> Sql<()> {
    conn.execute(
        "INSERT INTO resource_approvals (name, sha256, decision, decided_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(name, sha256) DO UPDATE SET decision = excluded.decision,
            decided_at = excluded.decided_at",
        params![
            row.name,
            row.sha256,
            if row.approved { "approved" } else { "rejected" },
            now
        ],
    )?;
    Ok(())
}

/// Every recorded extension decision.
///
/// # Errors
///
/// Returns the SQLite error when the query fails, or `InvalidColumnType` when a stored decision is
/// neither `approved` nor `rejected`.
pub fn load_approvals(conn: &Connection) -> Sql<Vec<ApprovalRow>> {
    let mut stmt = conn.prepare("SELECT name, sha256, decision FROM resource_approvals")?;
    let rows = stmt.query_map([], |row| {
        let approved = match row.get::<_, String>(2)?.as_str() {
            "approved" => true,
            "rejected" => false,
            _ => return Err(corrupt("approval decision")),
        };
        Ok(ApprovalRow {
            name: row.get(0)?,
            sha256: row.get(1)?,
            approved,
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use lade_core::domain::ids::{GitAlgorithm, GitId, Timestamp, WorkspaceId};
    use lade_core::domain::objectives::{BranchName, Intent, NewObjective, Objective, Title};

    use super::*;
    use crate::storage::{ObjectiveRow, WorkspaceRow, insert_workspace, memory, save_objective};

    fn objective(conn: &Connection, n: u8) -> ObjectiveId {
        let workspace_id = WorkspaceId::new(Uuid::from_random_bytes([n; 16]));
        insert_workspace(
            conn,
            &WorkspaceRow {
                id: workspace_id,
                path: PathBuf::from(format!("/repo/{n}")),
                name: "w".into(),
                default_branch: None,
                opened_at: 0,
            },
        )
        .expect("workspace");
        let objective = Objective::define(
            NewObjective {
                id: ObjectiveId::new(Uuid::from_random_bytes([n + 100; 16])),
                workspace_id,
                title: Title::new("t").expect("title"),
                intent: Intent::new("o", "c", "a").expect("intent"),
                integration_target: BranchName::new("main").expect("branch"),
                start_commit: GitId::new(GitAlgorithm::Sha1, &"a".repeat(40)).expect("commit"),
            },
            Timestamp::from_unix_ms(0),
        );
        let id = objective.id();
        let row = ObjectiveRow {
            objective,
            git_branch: None,
            worktree: None,
            integration_conflict: false,
            history_deleted: false,
        };
        save_objective(conn, &row).expect("objective");
        id
    }

    #[test]
    fn thinking_levels_are_replaced_per_objective_and_need_a_recorded_objective() {
        let conn = memory();
        let first = objective(&conn, 1);
        let second = objective(&conn, 2);
        save_thinking(&conn, first, ThinkingLevel::Low, 1).expect("save");
        save_thinking(&conn, second, ThinkingLevel::Max, 1).expect("save");
        save_thinking(&conn, first, ThinkingLevel::High, 2).expect("replace");
        let mut levels = load_thinking(&conn).expect("load");
        levels.sort_by_key(|(id, _)| id.to_string());
        let mut expected = vec![(first, ThinkingLevel::High), (second, ThinkingLevel::Max)];
        expected.sort_by_key(|(id, _)| id.to_string());
        assert_eq!(levels, expected);
        let unknown = ObjectiveId::new(Uuid::from_random_bytes([9; 16]));
        assert!(save_thinking(&conn, unknown, ThinkingLevel::Off, 3).is_err());
        conn.execute("UPDATE session_preferences SET thinking = 'loud'", [])
            .expect("corrupt");
        assert!(matches!(
            load_thinking(&conn),
            Err(rusqlite::Error::InvalidColumnType(_, what, _)) if what == "thinking level"
        ));
    }

    #[test]
    fn extension_decisions_are_keyed_by_name_and_content_hash() {
        let conn = memory();
        let decision = |sha256: &str, approved| ApprovalRow {
            name: "ext".into(),
            sha256: sha256.into(),
            approved,
        };
        save_approval(&conn, &decision("aa", true), 1).expect("approve");
        save_approval(&conn, &decision("bb", false), 1).expect("reject other version");
        save_approval(&conn, &decision("aa", false), 2).expect("revoke");
        let mut rows = load_approvals(&conn).expect("load");
        rows.sort_by(|a, b| a.sha256.cmp(&b.sha256));
        assert_eq!(rows, [decision("aa", false), decision("bb", false)]);
        assert!(
            conn.execute(
                "INSERT INTO resource_approvals VALUES ('x', 'cc', 'maybe', 0)",
                []
            )
            .is_err(),
            "the schema only admits approved or rejected"
        );
    }
}
