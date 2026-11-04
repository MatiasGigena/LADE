//! Candidates, verifications and approvals of an Objective's Review.

use lade_core::domain::ids::{ApprovalId, CandidateId, ObjectiveId, Timestamp, VerificationId};
use lade_core::domain::inspection::ArtifactDigest;
use lade_core::domain::review::{
    Approval, ApprovalState, Candidate, CandidateStatus, ChangeCounts, InvalidationReason,
    Provenance, Review, Verification, VerificationOutcome,
};
use rusqlite::{Connection, params};

use super::{Sql, corrupt, count, git, ts, u, uuid};

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

/// Records the Review's candidates, verifications and approvals; already recorded rows
/// only have their mutable state updated.
///
/// # Errors
///
/// Returns the SQLite error when a write fails, or `InvalidColumnType` when a verification's argv
/// cannot be serialized.
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

/// Stores the bounded output tail of verification `id`.
///
/// # Errors
///
/// Returns the SQLite error when the update fails.
pub fn set_verification_tail(conn: &Connection, id: VerificationId, tail: &str) -> Sql<()> {
    conn.execute(
        "UPDATE verifications SET output_tail = ?2 WHERE id = ?1",
        params![id.to_string(), tail],
    )?;
    Ok(())
}

/// The stored output tail of every verification.
///
/// # Errors
///
/// Returns the SQLite error when the query fails, or `InvalidColumnType` when a stored id is not a
/// UUID.
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

/// Rebuilds `objective`'s Review from its candidates, verifications and approvals.
///
/// # Errors
///
/// Returns the SQLite error when a query fails, or `InvalidColumnType` naming the field when a
/// stored value no longer parses or the rows no longer form a valid Review.
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
                    added: count(row.get(7)?, "added")?,
                    removed: count(row.get(8)?, "removed")?,
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
