//! Every hard execution limit of the sandboxes, in one place (delivery plan §7).
//!
//! The values are the proposed defaults recorded in the delivery plan; approving or changing them
//! is Matias's decision (#14). Change them only here: the launcher, terminals, verification runs,
//! the service's writable-storage monitor and the cgroup placement all read these constants.

use std::ffi::OsString;
use std::time::Duration;

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;

/// Resource limits of one sandbox (soft = hard).
///
/// `processes` is `RLIMIT_NPROC`, set by the launch gate inside the sandbox right before it
/// executes the program; every other limit is set by `prlimit` on bwrap. The kernel (Linux ≥
/// 5.14) counts `RLIMIT_NPROC` per user per user namespace, so inside the sandbox's own user
/// namespace it counts only the sandbox's processes and threads. Set on bwrap it would count every
/// process of the user's desktop: bwrap could not create the namespace once those exceed it, and
/// the kernel copies the creator's value into the new namespace as a cap on the user's host-wide
/// count. Not enforced for host root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub address_space_bytes: u64,
    pub processes: u64,
    pub open_files: u64,
    pub file_size_bytes: u64,
    pub cpu_seconds: Option<u64>,
}

impl Limits {
    /// §7 worker defaults: 4 GiB address space, 512 processes, 1024 files, 512 MiB file size.
    pub const WORKER: Self = Self {
        address_space_bytes: 4 * GIB,
        processes: 512,
        open_files: 1024,
        file_size_bytes: 512 * MIB,
        cpu_seconds: None,
    };

    /// Verification defaults: worker limits plus 600 s CPU time.
    pub const VERIFICATION: Self = Self {
        cpu_seconds: Some(600),
        ..Self::WORKER
    };

    /// The `prlimit` arguments: every limit except `processes`, which the launch gate sets.
    pub(super) fn prlimit_args(&self) -> Vec<OsString> {
        let mut args = vec![
            format!("--as={}", self.address_space_bytes),
            format!("--nofile={}", self.open_files),
            format!("--fsize={}", self.file_size_bytes),
        ];
        if let Some(cpu) = self.cpu_seconds {
            args.push(format!("--cpu={cpu}"));
        }
        args.into_iter().map(OsString::from).collect()
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::WORKER
    }
}

/// Worker sandbox limits used by the launcher: [`Limits::WORKER`] with 8 GiB of address space.
/// Node 22 cannot start under the 4 GiB §7 default: V8 reserves its heap and code ranges up
/// front. `RLIMIT_AS` bounds virtual address space only, never resident memory.
pub const WORKER_LIMITS: Limits = Limits {
    address_space_bytes: 8 * GIB,
    ..Limits::WORKER
};

/// Terminal processes get the §7 worker limits; a shell does not need Node's 8 GiB reservation.
pub const TERMINAL_LIMITS: Limits = Limits::WORKER;

/// Wall-clock timeout for verification runs (§7).
pub const VERIFICATION_TIMEOUT: Duration = Duration::from_secs(900);
/// Captured output for verification runs (tail retained, §7).
pub const VERIFICATION_OUTPUT_LIMIT: usize = 4 << 20;

/// Size of every sandbox's private `/tmp` (a memory-backed tmpfs, `bwrap --size`). Writes beyond
/// it fail with `ENOSPC`. Under delegated cgroups the pages also count against the Objective's
/// `memory.max`.
pub const SANDBOX_TMP_BYTES: u64 = GIB;

/// Writable-storage budget of one Objective's execution: the worktree plus the session directory
/// (§7, 2 GiB). The service measures it periodically and stops the agent with `limit_reached`
/// when it is exceeded. This is a monitor, not a quota: writes between two checks succeed.
pub const WRITABLE_STORAGE_BYTES: u64 = 2 * GIB;

/// How often the service measures writable storage of live executions.
pub const STORAGE_CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// cgroup v2 limits of one Objective (worker, terminals and verification runs together), applied
/// only when the service's cgroup is delegated. `None` writes `max` (no limit).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CgroupLimits {
    /// `memory.max` in bytes (resident memory including page cache and tmpfs pages).
    pub memory_max: Option<u64>,
    /// `pids.max`: processes and threads.
    pub pids_max: Option<u64>,
    /// `cpu.max` as (quota, period) in microseconds; quota/period CPUs.
    pub cpu_max: Option<(u64, u64)>,
}

/// Proposed per-Objective cgroup limits: the §7 4 GiB memory and 512 processes/threads as real
/// (resident, per-Objective) limits; no CPU ceiling, matching §7 (workers have no CPU limit).
pub const OBJECTIVE_CGROUP_LIMITS: CgroupLimits = CgroupLimits {
    memory_max: Some(4 * GIB),
    pids_max: Some(512),
    cpu_max: None,
};
