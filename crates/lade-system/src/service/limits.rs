use std::path::Path;

use lade_core::domain::ids::ObjectiveId;
use lade_protocol::client::{NoticeCode, NoticeLevel};

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

/// Free space on the state volume: its pressure level and the bytes free when they could be read.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FreeSpace {
    pub pressure: Pressure,
    pub bytes: Option<u64>,
}

/// Storage limits: free space on the state volume, and the writable-storage limit per
/// Objective with whether a measurement is already running.
pub(crate) struct StorageState {
    pub free_space: FreeSpace,
    pub writable_limit: u64,
    pub measuring: bool,
}

impl StorageState {
    pub fn new(writable_limit: u64) -> Self {
        Self {
            free_space: FreeSpace::default(),
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
            .filter(|(oid, st)| **oid != except && st.row.objective.execution().state().is_live())
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
        (self.storage.free_space.pressure == Pressure::Critical).then(|| {
            format!(
                "The disk that holds LADE's data has {} MiB free, below the {} MiB LADE keeps in \
                 reserve. Free some space and try again.",
                self.storage.free_space.bytes.map_or(0, mib),
                mib(self.limits.storage_reserve_bytes)
            )
        })
    }

    pub(crate) fn check_free_space(&mut self) {
        let free = free_bytes(&self.paths.state);
        let level = match free {
            Some(free) if free < self.limits.storage_reserve_bytes => Pressure::Critical,
            Some(free) if free < self.limits.storage_warn_bytes => Pressure::Low,
            _ => Pressure::Normal,
        };
        let changed = level != self.storage.free_space.pressure;
        self.storage.free_space = FreeSpace {
            pressure: level,
            bytes: free,
        };
        if changed {
            eprintln!(
                "laded: storage level={level:?} free_mib={}",
                free.map_or(0, mib)
            );
            self.announce_storage(true);
        }
    }

    pub(crate) fn announce_storage(&mut self, transition: bool) {
        let free = self.storage.free_space.bytes.map_or(0, mib);
        match self.storage.free_space.pressure {
            Pressure::Normal if transition => self.notice(
                NoticeLevel::Info,
                NoticeCode::StorageOk,
                &format!(
                    "The disk that holds LADE's data has {free} MiB free again. New work is \
                     accepted again."
                ),
            ),
            Pressure::Normal => {}
            Pressure::Low => self.notice(
                NoticeLevel::Warning,
                NoticeCode::StorageLow,
                &format!(
                    "The disk that holds LADE's data has only {free} MiB free. New agents, \
                     terminals and checks stop below the {} MiB reserve.",
                    mib(self.limits.storage_reserve_bytes)
                ),
            ),
            Pressure::Critical => {
                let detail = self.storage_blocker().unwrap_or_default();
                self.notice(NoticeLevel::Error, NoticeCode::StorageCritical, &detail);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use lade_protocol::client::Event;

    use super::*;
    use crate::service::test_harness::Harness;

    fn notices(harness: &mut Harness) -> Vec<NoticeCode> {
        harness
            .take_events()
            .into_iter()
            .filter_map(|e| match e {
                Event::ServiceNotice(n) => Some(n.code),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn limits_describe_themselves_in_mebibytes() {
        assert_eq!(
            ResourceLimits::default().to_string(),
            "max_workers=16 max_terminals=16 storage_warn=2048 MiB storage_reserve=512 MiB"
        );
    }

    #[test]
    fn free_space_moves_between_levels_and_announces_each_change_once() {
        let mut harness = Harness::new("limits-storage");
        assert!(harness.service.storage_blocker().is_none());
        harness.service.limits.storage_warn_bytes = u64::MAX;
        harness.service.check_free_space();
        assert_eq!(harness.service.storage.free_space.pressure, Pressure::Low);
        assert!(harness.service.storage.free_space.bytes.is_some());
        harness.service.check_free_space();
        assert_eq!(
            notices(&mut harness),
            [NoticeCode::StorageLow],
            "only the change"
        );

        harness.service.limits.storage_reserve_bytes = u64::MAX;
        harness.service.check_free_space();
        assert_eq!(
            harness.service.storage.free_space.pressure,
            Pressure::Critical
        );
        assert_eq!(notices(&mut harness), [NoticeCode::StorageCritical]);
        let blocker = harness.service.storage_blocker().expect("critical blocks");
        assert!(blocker.contains("MiB free"), "{blocker}");
        let (_, oid) = harness.create_objective("Blocked");
        assert_eq!(harness.service.worker_blocker(oid), Some(blocker));

        harness.service.limits = ResourceLimits {
            storage_warn_bytes: 0,
            storage_reserve_bytes: 0,
            ..ResourceLimits::default()
        };
        harness.service.check_free_space();
        assert_eq!(
            harness.service.storage.free_space.pressure,
            Pressure::Normal
        );
        assert_eq!(notices(&mut harness), [NoticeCode::StorageOk]);
        harness.service.announce_storage(false);
        assert!(
            notices(&mut harness).is_empty(),
            "normal is announced only as a transition"
        );
    }

    #[test]
    fn free_bytes_reads_the_volume_and_fails_soft() {
        assert!(free_bytes(Path::new("/")).is_some());
        assert_eq!(free_bytes(Path::new("/lade/no/such/volume")), None);
        assert_eq!(mib(3 * MIB - 1), 2);
    }
}
