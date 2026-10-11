//! Startup: load durable state, then restart recovery before anything is served.
//!
//! Recovery never replays effects: live executions become cancelled/interrupted, dispatched
//! inputs become uncertain, unconsumed approvals expire, running verifications are interrupted,
//! pending operations are recorded as interrupted, the integration journal is reconciled against
//! the actual target refs, and stale sandboxes/verification copies are swept (best effort).

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use lade_core::application::recover_after_restart;
use lade_core::domain::ids::{ObjectiveId, Timestamp};
use lade_core::domain::review::IntegrationPlan;
use lade_protocol::client::{ErrorCode, Outcome, Rejection, RequestName, SafeAction, Settings};

use crate::adapters::git::CommitSpec;
use crate::storage;

use super::Service;
use super::convert::{adapter_git, core_git, now_ms};
use super::state::{ObjState, WsState};

fn sql_text(error: &rusqlite::Error) -> String {
    format!("store: {error}")
}

impl Service {
    pub(crate) fn load(&mut self) -> Result<(), String> {
        let conn = self.store.conn();
        self.settings = storage::load_settings(conn)
            .map_err(|e| sql_text(&e))?
            .unwrap_or(Settings {
                verification_commands: Vec::new(),
                default_model: None,
            });
        for row in storage::load_workspaces(conn).map_err(|e| sql_text(&e))? {
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
        for draft in storage::load_drafts(conn).map_err(|e| sql_text(&e))? {
            drafts
                .entry(draft.record().objective_id)
                .or_default()
                .push(draft);
        }
        let mut inputs: HashMap<ObjectiveId, Vec<_>> = HashMap::new();
        for (seq, input) in storage::load_inputs(conn).map_err(|e| sql_text(&e))? {
            self.input_seq = self.input_seq.max(seq);
            inputs
                .entry(input.objective_id)
                .or_default()
                .push((seq, input));
        }
        let mut captures: HashMap<ObjectiveId, _> = storage::latest_captures(conn)
            .map_err(|e| sql_text(&e))?
            .into_iter()
            .map(|c| (c.objective_id, c))
            .collect();
        let tails: HashMap<_, _> = storage::load_verification_tails(conn)
            .map_err(|e| sql_text(&e))?
            .into_iter()
            .collect();
        for (oid, seq) in storage::max_activity_seq(conn).map_err(|e| sql_text(&e))? {
            self.activity_seq.insert(oid, seq);
        }
        for row in storage::load_objectives(conn).map_err(|e| sql_text(&e))? {
            let oid = row.objective.id();
            let review = storage::load_review(conn, oid).map_err(|e| sql_text(&e))?;
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

    pub(crate) fn recover(&mut self) -> Result<(), String> {
        let now = now_ms();
        for (operation, digest, name) in
            storage::pending_operations(self.store.conn()).map_err(|e| sql_text(&e))?
        {
            let Some(name) = RequestName::parse(&name) else {
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
                &operation,
                &digest,
                name.as_str(),
                "uncertain",
                Some(&outcome),
                now,
            )
            .map_err(|e| sql_text(&e))?;
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
            .map_err(|e| format!("objective {oid}: {e}"))?;
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
            let tx = self.store.begin().map_err(|e| sql_text(&e))?;
            Self::save_tx(&tx, &st).map_err(|e| sql_text(&e))?;
            for input in &changed {
                storage::save_delivery(&tx, input.id, &input.delivery, now)
                    .map_err(|e| sql_text(&e))?;
            }
            tx.commit().map_err(|e| sql_text(&e))?;
            eprintln!("laded: recovered objective={oid} after restart");
            self.objectives.insert(oid, st);
        }
        self.sweep();
        Ok(())
    }

    /// Integration intents without a recorded outcome: recompute the exact squash commit and
    /// compare with the actual target ref. Never re-runs the integration.
    fn reconcile_journal(&mut self) -> Result<(), String> {
        let open = storage::open_journal(self.store.conn()).map_err(|e| sql_text(&e))?;
        for (row, message, commit_time) in open {
            let now = now_ms();
            let Some(st) = self.objectives.get(&row.objective_id).cloned() else {
                continue;
            };
            let Some(ws) = self.workspaces.get(&st.objective().record().workspace_id) else {
                continue;
            };
            let current = ws
                .repo
                .resolve_ref(&format!("refs/heads/{}", row.target_branch))
                .ok()
                .flatten();
            let expected_commit = ws
                .repo
                .commit_tree(&CommitSpec {
                    tree: adapter_git(&row.candidate_tree),
                    parents: vec![adapter_git(&row.expected_target)],
                    message: message.clone(),
                    timestamp: Some(commit_time),
                })
                .ok();
            let integrated = current.is_some() && current == expected_commit;
            if integrated {
                let mut st = st;
                let candidate = st.review.candidate(row.candidate_id).cloned();
                let completed = candidate.is_some_and(|candidate| {
                    let plan = IntegrationPlan {
                        approval_id: row.approval_id,
                        candidate_id: row.candidate_id,
                        candidate_commit: candidate.commit,
                        candidate_tree: row.candidate_tree.clone(),
                        expected_target: row.expected_target.clone(),
                    };
                    st.review
                        .complete_integration(
                            &mut st.row.objective,
                            &plan,
                            Timestamp::from_unix_ms(now),
                        )
                        .is_ok()
                });
                let commit = current.as_ref().map(core_git);
                let tx = self.store.begin().map_err(|e| sql_text(&e))?;
                if completed {
                    Self::save_tx(&tx, &st).map_err(|e| sql_text(&e))?;
                }
                storage::update_journal(
                    &tx,
                    &row.id,
                    "reconciled_done",
                    commit.as_ref(),
                    Some("integration found completed after restart"),
                    now,
                )
                .map_err(|e| sql_text(&e))?;
                tx.commit().map_err(|e| sql_text(&e))?;
                if completed {
                    self.objectives.insert(row.objective_id, st);
                }
                eprintln!(
                    "laded: reconciled integration objective={} done",
                    row.objective_id
                );
            } else {
                storage::update_journal(
                    self.store.conn(),
                    &row.id,
                    "reconciled_aborted",
                    None,
                    Some("target does not hold the intended squash commit"),
                    now,
                )
                .map_err(|e| sql_text(&e))?;
                eprintln!(
                    "laded: reconciled integration objective={} not applied",
                    row.objective_id
                );
            }
        }
        Ok(())
    }

    /// Best-effort removal of leftovers of a previous instance.
    fn sweep(&mut self) {
        match crate::supervision::sweep_stale(&self.paths.run) {
            Ok(report) if !report.killed.is_empty() => {
                eprintln!(
                    "laded: killed {} stale worker sandbox(es)",
                    report.killed.len()
                );
            }
            Ok(_) => {}
            Err(e) => eprintln!("laded: stale worker sweep failed: {}", e.kind()),
        }
        if let Err(e) = crate::supervision::sweep_sockets(&self.paths.sockets) {
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

/// Kills sandbox roots (`bwrap`/`prlimit`) whose arguments reference this state directory and
/// that a dead instance left behind (the launcher pidfiles are swept by `sweep_stale` first).
fn kill_stale_sandboxes(state: &Path) {
    let me = std::process::id();
    let uid = crate::transport::service_uid();
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
