//! The writable-storage limit of a running agent: its worktree and session directory are
//! measured off the service thread, and an agent over the limit is stopped.

use std::path::PathBuf;

use lade_core::domain::execution::ExecutionGeneration;
use lade_core::domain::ids::ObjectiveId;
use lade_protocol::client::{NoticeCode, NoticeLevel};

use crate::service::jobs;
use crate::service::{Completed, Service};
use crate::support::ts_now;

pub(crate) const LIMIT_REACHED: &str = "limit_reached";

pub(super) fn human_bytes(bytes: u64) -> String {
    const KIB: u64 = 1 << 10;
    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;
    let tenths = |unit: u64| {
        let tenths = bytes.saturating_mul(10) / unit;
        format!("{}.{}", tenths / 10, tenths % 10)
    };
    match bytes {
        b if b >= GIB => format!("{} GiB", tenths(GIB)),
        b if b >= MIB => format!("{} MiB", tenths(MIB)),
        b => format!("{} KiB", b.div_ceil(KIB)),
    }
}

pub(crate) struct StorageUsage {
    pub(super) oid: ObjectiveId,
    pub(super) generation: u64,
    pub(super) bytes: u64,
}

impl Service {
    pub(crate) fn check_storage(&mut self) {
        if self.storage.measuring {
            return;
        }
        let targets: Vec<(ObjectiveId, u64, [PathBuf; 2])> = self
            .objectives
            .iter()
            .filter(|(_, st)| st.row.objective.execution().state().is_live())
            .filter_map(|(oid, st)| {
                let worktree = st.row.worktree.clone()?;
                let generation = st.row.objective.execution().generation().value();
                Some((
                    *oid,
                    generation,
                    [worktree, self.paths.sessions.join(oid.to_string())],
                ))
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        self.storage.measuring = true;
        self.spawn(move || Completed::Storage {
            usage: targets
                .into_iter()
                .map(|(oid, generation, paths)| StorageUsage {
                    oid,
                    generation,
                    bytes: jobs::disk_usage(&paths),
                })
                .collect(),
        });
    }

    pub(crate) fn storage_measured(&mut self, usage: Vec<StorageUsage>) {
        self.storage.measuring = false;
        for measured in usage {
            if measured.bytes > self.storage.writable_limit {
                self.storage_exceeded(&measured);
            }
        }
    }

    fn storage_exceeded(&mut self, measured: &StorageUsage) {
        let StorageUsage {
            oid,
            generation,
            bytes,
        } = *measured;
        let Some(mut st) = self.objectives.get(&oid).cloned() else {
            return;
        };
        let execution = st.row.objective.execution();
        if execution.generation().value() != generation || !execution.state().is_live() {
            return;
        }
        let reason = format!(
            "{LIMIT_REACHED}: writable storage {} exceeds the {} limit (worktree and session directory)",
            human_bytes(bytes),
            human_bytes(self.storage.writable_limit)
        );
        if st
            .row
            .objective
            .observe_failure(ExecutionGeneration::new(generation), &reason, ts_now())
            .is_err()
        {
            return;
        }
        let recorded = self.commit_obj(None, None, st);
        if !Self::persisted("the storage limit", oid, recorded) {
            return;
        }
        eprintln!("laded: writable storage limit reached objective={oid} bytes={bytes}");
        if let Some(mut control) = self.rt(oid).control.take() {
            self.spawn(move || {
                control.terminate();
                Completed::Done
            });
        }
        self.notice(NoticeLevel::Error, NoticeCode::LimitReached, &reason);
        self.system_item(
            oid,
            "Storage limit reached",
            &format!(
                "The agent was stopped: {reason}. Free space in the worktree, then resume explicitly; the limit is never raised automatically."
            ),
            true,
        );
        self.emit_objective(oid);
        self.continue_freeze(oid);
    }
}
