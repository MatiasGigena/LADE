use std::path::Path;

use lade_core::domain::ids::ObjectiveId;
use lade_protocol::client::{ErrorCode, NoticeLevel, Rejection, SafeAction};

use super::Service;

const MIB: u64 = 1 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceLimits {
    pub max_workers: usize,
    pub max_terminals: usize,
    pub storage_warn_bytes: u64,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Pressure {
    #[default]
    Normal,
    Low,
    Critical,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Storage {
    pub level: Pressure,
    pub free: Option<u64>,
}

/// Storage limits: free space on the state volume, and the writable-storage limit per
/// Objective with whether a measurement is already running.
pub(crate) struct StorageGuard {
    pub free_space: Storage,
    pub writable_limit: u64,
    pub measuring: bool,
}

impl StorageGuard {
    pub fn new(writable_limit: u64) -> Self {
        Self {
            free_space: Storage::default(),
            writable_limit,
            measuring: false,
        }
    }
}

pub(crate) fn free_bytes(path: &Path) -> Option<u64> {
    rustix::fs::statvfs(path)
        .ok()
        .map(|s| s.f_bavail.saturating_mul(s.f_frsize))
}

fn mib(bytes: u64) -> u64 {
    bytes / MIB
}

impl Service {
    fn live_workers(&self, except: ObjectiveId) -> usize {
        self.objectives
            .iter()
            .filter(|(oid, st)| **oid != except && st.objective().execution().state().is_live())
            .count()
    }

    pub(crate) fn worker_blocker(&self, oid: ObjectiveId) -> Option<String> {
        let live = self.live_workers(oid);
        let max = self.limits.max_workers;
        if live >= max {
            return Some(format!(
                "{live} of {max} agents are already running, the most LADE runs at once. Stop one \
                 to start another."
            ));
        }
        self.storage_blocker()
    }

    pub(crate) fn storage_blocker(&self) -> Option<String> {
        (self.storage.free_space.level == Pressure::Critical).then(|| {
            format!(
                "The disk that holds LADE's data has {} MiB free, below the {} MiB LADE keeps in \
                 reserve. Free some space and try again.",
                self.storage.free_space.free.map_or(0, mib),
                mib(self.limits.storage_reserve_bytes)
            )
        })
    }

    pub(crate) fn quota_rejection(detail: String) -> Rejection {
        Rejection::new(ErrorCode::LimitReached, SafeAction::Refresh, &detail)
    }

    pub(crate) fn check_free_space(&mut self) {
        let free = free_bytes(&self.paths.state);
        let level = match free {
            Some(free) if free < self.limits.storage_reserve_bytes => Pressure::Critical,
            Some(free) if free < self.limits.storage_warn_bytes => Pressure::Low,
            _ => Pressure::Normal,
        };
        let changed = level != self.storage.free_space.level;
        self.storage.free_space = Storage { level, free };
        if changed {
            eprintln!(
                "laded: storage level={level:?} free_mib={}",
                free.map_or(0, mib)
            );
            self.announce_storage(true);
        }
    }

    pub(crate) fn announce_storage(&mut self, transition: bool) {
        let free = self.storage.free_space.free.map_or(0, mib);
        match self.storage.free_space.level {
            Pressure::Normal if transition => self.notice(
                NoticeLevel::Info,
                "storage_ok",
                &format!(
                    "The disk that holds LADE's data has {free} MiB free again. New work is \
                     accepted again."
                ),
            ),
            Pressure::Normal => {}
            Pressure::Low => self.notice(
                NoticeLevel::Warning,
                "storage_low",
                &format!(
                    "The disk that holds LADE's data has only {free} MiB free. New agents, \
                     terminals and checks stop below the {} MiB reserve.",
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
