//! Aggregate accounting across Objectives (IMP-013): live agent executions, contained terminals
//! and free space on the state filesystem.
//!
//! Every limit is configurable (`laded serve --max-workers …`), printed at startup and named in
//! the refusal and in `available_actions`, so a limit is never a hidden cap. The defaults are
//! proposals awaiting approval (#14 policy); four workers is a measurement point of the
//! performance budgets, not a limit. Accounting lives in the service layer only: sandbox resource
//! limits (prlimit, cgroups) are the launcher's.
//!
//! Storage pressure is checked on the filesystem holding the state directory (store, artifacts,
//! worktrees, session files). Below the warning level a `service.notice {code: storage_low}`
//! is shown; below the reserve, work that starts new writers (agent start/resume, writable
//! terminals, verification copies) is refused with `limit_reached` and the reason, while
//! decisions, drafts, inputs and the activity history keep being recorded inside the reserve.
//! Nothing is deleted to make room and running executions are not stopped.

use std::path::Path;

use lade_core::domain::ids::ObjectiveId;
use lade_protocol::client::{ErrorCode, NoticeLevel, Rejection, SafeAction};

use super::Service;
use super::convert::rejection;

const MIB: u64 = 1 << 20;

/// Configurable aggregate limits of one service instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceLimits {
    /// Live agent executions (one sandboxed worker each) across every Objective.
    pub max_workers: usize,
    /// Contained terminals across every Objective (at most 4 per Objective).
    pub max_terminals: usize,
    /// Free bytes on the state filesystem below which a warning is shown.
    pub storage_warn_bytes: u64,
    /// Free bytes kept in reserve: below this, new writers are refused.
    pub storage_reserve_bytes: u64,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_workers: 16,
            max_terminals: 16,
            storage_warn_bytes: 2048 * MIB,
            storage_reserve_bytes: 512 * MIB,
        }
    }
}

impl std::fmt::Display for ResourceLimits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "max_workers={} max_terminals={} storage_warn={} MiB storage_reserve={} MiB",
            self.max_workers,
            self.max_terminals,
            self.storage_warn_bytes / MIB,
            self.storage_reserve_bytes / MIB
        )
    }
}

/// Pressure level of the state filesystem.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Pressure {
    #[default]
    Normal,
    Low,
    Critical,
}

/// Last observation of the state filesystem.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Storage {
    pub level: Pressure,
    pub free: Option<u64>,
}

/// Bytes available to the service's user on the filesystem holding `path`.
pub(crate) fn free_bytes(path: &Path) -> Option<u64> {
    rustix::fs::statvfs(path)
        .ok()
        .map(|s| s.f_bavail.saturating_mul(s.f_frsize))
}

fn mib(bytes: u64) -> u64 {
    bytes / MIB
}

impl Service {
    /// Number of Objectives other than `except` whose execution is live (one worker each).
    fn live_workers(&self, except: ObjectiveId) -> usize {
        self.objectives
            .iter()
            .filter(|(oid, st)| **oid != except && st.objective().execution().state().is_live())
            .count()
    }

    /// Why a new worker for `oid` may not start now (aggregate quota or storage pressure).
    pub(crate) fn worker_blocker(&self, oid: ObjectiveId) -> Option<String> {
        let live = self.live_workers(oid);
        let max = self.limits.max_workers;
        if live >= max {
            return Some(format!(
                "{live} of {max} agents are running (laded --max-workers {max}); stop one or raise the limit"
            ));
        }
        self.storage_blocker()
    }

    /// Why work that adds writers on the state filesystem is refused now.
    pub(crate) fn storage_blocker(&self) -> Option<String> {
        (self.free_space.level == Pressure::Critical).then(|| {
            format!(
                "storage pressure: {} MiB free on the state filesystem, below the {} MiB reserve (laded --storage-reserve-mib); free space and retry",
                self.free_space.free.map_or(0, mib),
                mib(self.limits.storage_reserve_bytes)
            )
        })
    }

    pub(crate) fn quota_rejection(detail: String) -> Rejection {
        rejection(ErrorCode::LimitReached, SafeAction::Refresh, &detail)
    }

    /// Re-reads free space (every second from the tick) and announces level changes.
    pub(crate) fn check_free_space(&mut self) {
        let free = free_bytes(&self.paths.state);
        let level = match free {
            Some(free) if free < self.limits.storage_reserve_bytes => Pressure::Critical,
            Some(free) if free < self.limits.storage_warn_bytes => Pressure::Low,
            // An unreadable filesystem state is not invented pressure; IO errors still surface.
            _ => Pressure::Normal,
        };
        let changed = level != self.free_space.level;
        self.free_space = Storage { level, free };
        if changed {
            eprintln!(
                "laded: storage level={level:?} free_mib={}",
                free.map_or(0, mib)
            );
            self.announce_storage(true);
        }
    }

    /// Sends the current storage notice (also re-sent to a newly attached client).
    pub(crate) fn announce_storage(&mut self, transition: bool) {
        let free = self.free_space.free.map_or(0, mib);
        match self.free_space.level {
            Pressure::Normal if transition => self.notice(
                NoticeLevel::Info,
                "storage_ok",
                &format!("{free} MiB free on the state filesystem; new work is accepted again"),
            ),
            Pressure::Normal => {}
            Pressure::Low => self.notice(
                NoticeLevel::Warning,
                "storage_low",
                &format!(
                    "only {free} MiB free on the state filesystem (warning below {} MiB); new agents, terminals and checks stop at {} MiB",
                    mib(self.limits.storage_warn_bytes),
                    mib(self.limits.storage_reserve_bytes)
                ),
            ),
            Pressure::Critical => {
                let detail = self.storage_blocker().unwrap_or_default();
                self.notice(NoticeLevel::Error, "storage_critical", &detail);
            }
        }
    }
}
