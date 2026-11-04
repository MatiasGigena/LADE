use std::ffi::OsString;
use std::time::Duration;

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub address_space_bytes: u64,
    pub processes: u64,
    pub open_files: u64,
    pub file_size_bytes: u64,
    pub cpu_seconds: Option<u64>,
}

impl Limits {
    pub const WORKER: Self = Self {
        address_space_bytes: 4 * GIB,
        processes: 512,
        open_files: 1024,
        file_size_bytes: 512 * MIB,
        cpu_seconds: None,
    };

    pub const VERIFICATION: Self = Self {
        cpu_seconds: Some(600),
        ..Self::WORKER
    };

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

pub const WORKER_LIMITS: Limits = Limits {
    address_space_bytes: 8 * GIB,
    ..Limits::WORKER
};

pub const TERMINAL_LIMITS: Limits = Limits::WORKER;

pub const VERIFICATION_TIMEOUT: Duration = Duration::from_mins(15);
pub const VERIFICATION_OUTPUT_LIMIT: usize = 4 << 20;

pub const SANDBOX_TMP_BYTES: u64 = GIB;

pub const WRITABLE_STORAGE_BYTES: u64 = 2 * GIB;

pub const STORAGE_CHECK_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CgroupLimits {
    pub memory_max: Option<u64>,
    pub pids_max: Option<u64>,
    pub cpu_max: Option<(u64, u64)>,
}

pub const OBJECTIVE_CGROUP_LIMITS: CgroupLimits = CgroupLimits {
    memory_max: Some(4 * GIB),
    pids_max: Some(512),
    cpu_max: None,
};
