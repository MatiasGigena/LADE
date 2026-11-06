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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(limits: &Limits) -> Vec<String> {
        limits
            .prlimit_args()
            .into_iter()
            .map(|a| a.into_string().expect("utf-8"))
            .collect()
    }

    #[test]
    fn prlimit_arguments_name_each_limit_and_cpu_only_when_set() {
        assert_eq!(
            args(&Limits::WORKER),
            [
                format!("--as={}", 4 * GIB),
                "--nofile=1024".into(),
                format!("--fsize={}", 512 * MIB),
            ]
        );
        assert_eq!(
            args(&Limits::VERIFICATION).last().map(String::as_str),
            Some("--cpu=600")
        );
        assert!(
            !args(&Limits::WORKER)
                .iter()
                .any(|a| a.starts_with("--nproc")),
            "the process limit is enforced by the cgroup and the sandbox, not prlimit"
        );
    }

    #[test]
    fn the_named_limit_sets_differ_only_where_documented() {
        assert_eq!(Limits::default(), Limits::WORKER);
        assert_eq!(
            Limits {
                cpu_seconds: None,
                ..Limits::VERIFICATION
            },
            Limits::WORKER
        );
        assert_eq!(
            WORKER_LIMITS.address_space_bytes,
            2 * Limits::WORKER.address_space_bytes
        );
        assert_eq!(
            Limits {
                address_space_bytes: Limits::WORKER.address_space_bytes,
                ..WORKER_LIMITS
            },
            Limits::WORKER
        );
        assert_eq!(TERMINAL_LIMITS, Limits::WORKER);
        assert_eq!(
            OBJECTIVE_CGROUP_LIMITS.pids_max,
            Some(Limits::WORKER.processes)
        );
    }
}
