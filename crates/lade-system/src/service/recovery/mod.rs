use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use lade_core::domain::ids::{GitId, ObjectiveId, Timestamp};
use lade_core::domain::recovery::recover_after_restart;
use lade_core::domain::review::IntegrationPlan;
use lade_protocol::client::{ErrorCode, Outcome, Rejection, SafeAction, Settings};

use crate::adapters::git::CommitSpec;
use crate::storage::{self, JournalRow, JournalState, OperationKey, OperationState};

use super::state::{ObjState, WsState};
use super::{Service, StartError};
use crate::supervision::{sweep_sockets, sweep_stale};
use crate::support::fs::service_uid;
use crate::support::now_ms;

impl Service {
    pub(crate) fn load(&mut self) -> Result<(), StartError> {
        let conn = self.store.conn();
        self.settings = storage::load_settings(conn)?.unwrap_or(Settings {
            verification_commands: Vec::new(),
            default_model: None,
        });
        for row in storage::load_workspaces(conn)? {
            match super::jobs::open_repository(&row.path) {
                Ok(repo) => {
                    self.workspaces.insert(
                        row.id,
                        WsState {
                            id: row.id,
                            repo,
                            path: row.path,
                            name: row.name,
                            default_branch: row.default_branch,
                        },
                    );
                }
                Err(e) => eprintln!(
                    "laded: workspace unavailable workspace={} code={}",
                    row.id,
                    e.code()
                ),
            }
        }
        let mut drafts: HashMap<ObjectiveId, Vec<_>> = HashMap::new();
        for draft in storage::load_drafts(conn)? {
            drafts
                .entry(draft.record().objective_id)
                .or_default()
                .push(draft);
        }
        let mut inputs: HashMap<ObjectiveId, Vec<_>> = HashMap::new();
        for (seq, input) in storage::load_inputs(conn)? {
            self.input_seq = self.input_seq.max(seq);
            inputs
                .entry(input.objective_id)
                .or_default()
                .push((seq, input));
        }
        let mut captures: HashMap<ObjectiveId, _> = storage::latest_captures(conn)?
            .into_iter()
            .map(|c| (c.objective_id, c))
            .collect();
        let tails: HashMap<_, _> = storage::load_verification_tails(conn)?
            .into_iter()
            .collect();
        for (oid, seq) in storage::max_activity_seq(conn)? {
            self.activity_seq.insert(oid, seq);
        }
        for row in storage::load_objectives(conn)? {
            let oid = row.objective.id();
            let review = storage::load_review(conn, oid)?;
            let review_tails = review
                .verifications()
                .iter()
                .filter_map(|v| tails.get(&v.id).map(|t| (v.id, t.clone())))
                .collect();
            self.objectives.insert(
                oid,
                ObjState {
                    row,
                    review,
                    drafts: drafts.remove(&oid).unwrap_or_default(),
                    draft_stale: HashMap::new(),
                    inputs: inputs.remove(&oid).unwrap_or_default(),
                    capture: captures.remove(&oid),
                    tails: review_tails,
                },
            );
        }
        Ok(())
    }

    pub(crate) fn recover(&mut self) -> Result<(), StartError> {
        let now = now_ms();
        for operation in storage::pending_operations(self.store.conn())? {
            let Some(name) = operation.name else {
                continue;
            };
            let outcome = Outcome::Rejected {
                name,
                rejection: Rejection::new(
                    ErrorCode::Interrupted,
                    SafeAction::Refresh,
                    "the service restarted before the operation completed; re-read state",
                ),
            };
            storage::record_operation(
                self.store.conn(),
                OperationKey {
                    id: &operation.id,
                    digest: &operation.digest,
                    name,
                },
                OperationState::Uncertain,
                Some(&outcome),
                now,
            )?;
        }
        self.reconcile_journal()?;
        let ids: Vec<ObjectiveId> = self.objectives.keys().copied().collect();
        for oid in ids {
            let mut st = self.objectives[&oid].clone();
            let mut inputs: Vec<_> = st.inputs.iter().map(|(_, i)| i.clone()).collect();
            let before = (st.row.objective.clone(), st.review.clone(), inputs.clone());
            recover_after_restart(
                &mut st.row.objective,
                &mut st.review,
                &mut inputs,
                Timestamp::from_unix_ms(now),
            )
            .map_err(|e| StartError::Recovery(format!("objective {oid}: {e}")))?;
            if (st.row.objective.clone(), st.review.clone(), inputs.clone()) == before {
                continue;
            }
            let changed: Vec<_> = inputs
                .iter()
                .zip(&before.2)
                .filter(|(a, b)| a.delivery != b.delivery)
                .map(|(a, _)| a.clone())
                .collect();
            for ((_, slot), input) in st.inputs.iter_mut().zip(inputs) {
                *slot = input;
            }
            let tx = self.store.begin()?;
            Self::save_tx(&tx, &st)?;
            for input in &changed {
                storage::save_delivery(&tx, input.id, &input.delivery, now)?;
            }
            tx.commit()?;
            eprintln!("laded: recovered objective={oid} after restart");
            self.objectives.insert(oid, st);
        }
        self.sweep();
        Ok(())
    }

    fn reconcile_journal(&mut self) -> Result<(), StartError> {
        for row in storage::open_journal(self.store.conn())? {
            self.reconcile_integration(&row)?;
        }
        Ok(())
    }

    /// Resolves one integration left in the `intent` state: if the target now holds exactly
    /// the squash commit the journal describes, the integration completed and is applied to the
    /// Objective; otherwise it never happened and is recorded as aborted.
    fn reconcile_integration(&mut self, row: &JournalRow) -> Result<(), StartError> {
        let Some(st) = self.objectives.get(&row.objective_id).cloned() else {
            return Ok(());
        };
        let Some(ws) = self.workspaces.get(&st.row.objective.record().workspace_id) else {
            return Ok(());
        };
        let probed = ws
            .repo
            .resolve_ref(&format!("refs/heads/{}", row.target_branch))
            .and_then(|current| {
                let expected = ws.repo.commit_tree(&CommitSpec {
                    tree: row.candidate_tree.clone(),
                    parents: vec![row.expected_target.clone()],
                    message: row.message.clone(),
                    timestamp: Some(row.commit_time),
                })?;
                Ok((current, expected))
            });
        let (current, expected_commit) = match probed {
            Ok(found) => found,
            Err(e) => {
                eprintln!(
                    "laded: integration journal left pending objective={} code={}",
                    row.objective_id,
                    e.code()
                );
                self.recovery_notes.push(format!(
                    "LADE could not check whether the interrupted integration into {} completed ({}); it will check again on the next start.",
                    row.target_branch,
                    e.code()
                ));
                return Ok(());
            }
        };
        match current {
            Some(commit) if commit == expected_commit => {
                self.integration_found_done(row, st, &commit)
            }
            _ => {
                storage::update_journal(
                    self.store.conn(),
                    row.id,
                    JournalState::ReconciledAborted,
                    None,
                    Some("target does not hold the intended squash commit"),
                    now_ms(),
                )?;
                eprintln!(
                    "laded: reconciled integration objective={} not applied",
                    row.objective_id
                );
                Ok(())
            }
        }
    }

    /// Applies an integration the target shows completed, records it in the journal, and
    /// brings the target's checked-out files up to the squash commit.
    fn integration_found_done(
        &mut self,
        row: &JournalRow,
        mut st: ObjState,
        commit: &GitId,
    ) -> Result<(), StartError> {
        let now = now_ms();
        let candidate = st.review.candidate(row.candidate_id).cloned();
        let completed = candidate.is_some_and(|candidate| {
            let plan = IntegrationPlan {
                approval_id: row.approval_id,
                candidate_id: row.candidate_id,
                candidate_commit: candidate.commit,
                candidate_tree: row.candidate_tree.clone(),
                expected_target: row.expected_target.clone(),
            };
            match st.review.complete_integration(
                &mut st.row.objective,
                &plan,
                Timestamp::from_unix_ms(now),
            ) {
                Ok(()) => true,
                Err(e) => {
                    eprintln!(
                        "laded: completed integration not applied to objective={}: {e}",
                        row.objective_id
                    );
                    false
                }
            }
        });
        let tx = self.store.begin()?;
        if completed {
            Self::save_tx(&tx, &st)?;
        }
        storage::update_journal(
            &tx,
            row.id,
            JournalState::ReconciledDone,
            Some(commit),
            Some("integration found completed after restart"),
            now,
        )?;
        tx.commit()?;
        let workspace = st.row.objective.record().workspace_id;
        if completed {
            self.objectives.insert(row.objective_id, st);
        }
        eprintln!(
            "laded: reconciled integration objective={} done",
            row.objective_id
        );
        let Some(ws) = self.workspaces.get(&workspace) else {
            return Ok(());
        };
        let finished =
            ws.repo
                .finish_integration_checkout(&row.target_branch, &row.expected_target, commit);
        if let Err(e) = finished {
            eprintln!(
                "laded: integrated target checkout not updated objective={} code={}",
                row.objective_id,
                e.code()
            );
            self.recovery_notes.push(format!(
                "The integration into {} completed before the restart, but its checked-out files could not be updated ({}); run `git status` there before continuing.",
                row.target_branch,
                e.code()
            ));
        }
        Ok(())
    }

    fn sweep(&mut self) {
        match sweep_stale(&self.paths.run) {
            Ok(report) if !report.killed.is_empty() => {
                eprintln!(
                    "laded: killed {} stale worker sandbox(es)",
                    report.killed.len()
                );
            }
            Ok(_) => {}
            Err(e) => eprintln!("laded: stale worker sweep failed: {}", e.kind()),
        }
        if let Err(e) = sweep_sockets(&self.paths.sockets) {
            eprintln!("laded: stale gateway socket sweep failed: {}", e.kind());
        }
        kill_stale_sandboxes(&self.paths.state);
        for ws in self.workspaces.values() {
            let Ok(branches) = ws.repo.list_branches() else {
                continue;
            };
            for branch in branches
                .iter()
                .filter(|b| b.name.starts_with("lade/verify/"))
            {
                let id = &branch.name["lade/verify/".len()..];
                let path = self.paths.verify.join(id);
                if let Ok(worktree) = ws.repo.open_worktree(&path) {
                    let _ = ws.repo.worktree_remove(&worktree, true);
                }
                let _ = fs::remove_dir_all(&path);
                let _ = ws
                    .repo
                    .delete_ref_cas(&format!("refs/heads/{}", branch.name), &branch.commit);
            }
        }
        for dir in [&self.paths.verify, &self.paths.tmp] {
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let _ = if path.is_dir() {
                        fs::remove_dir_all(&path)
                    } else {
                        fs::remove_file(&path)
                    };
                }
            }
        }
    }
}

fn kill_stale_sandboxes(state: &Path) {
    let me = std::process::id();
    let uid = service_uid();
    let state_bytes = state.as_os_str().as_encoded_bytes();
    let mut targets = Vec::new();
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            if pid == me || entry.metadata().map(|m| m.uid()).ok() != Some(uid) {
                continue;
            }
            let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else {
                continue;
            };
            let mut args = cmdline.split(|b| *b == 0);
            let program = args.next().unwrap_or_default();
            let name = program.rsplit(|b| *b == b'/').next().unwrap_or_default();
            if !(name == b"bwrap" || name == b"prlimit") {
                continue;
            }
            if args.any(|arg| {
                arg.starts_with(state_bytes)
                    && arg.get(state_bytes.len()).is_none_or(|b| *b == b'/')
            }) {
                targets.push(pid);
            }
        }
    }
    for pid in targets {
        if pid == me {
            continue;
        }
        let still_ours = fs::read(format!("/proc/{pid}/cmdline"))
            .is_ok_and(|c| c.windows(state_bytes.len()).any(|w| w == state_bytes));
        let Some(raw) = i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        else {
            continue;
        };
        if still_ours {
            eprintln!("laded: killing stale sandbox pid={pid}");
            let _ = rustix::process::kill_process(raw, rustix::process::Signal::KILL);
        }
    }
}

#[cfg(test)]
mod tests;
