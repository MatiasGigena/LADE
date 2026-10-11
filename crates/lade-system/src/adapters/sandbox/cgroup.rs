//! Optional cgroup v2 placement of sandboxes inside the service's delegated subtree.
//!
//! When `laded` runs from the shipped unit (`Delegate=memory pids cpu`), systemd hands it its own
//! cgroup, marked with the `user.delegate` (or `trusted.delegate`) extended attribute. [`detect`]
//! then arranges, below that cgroup:
//!
//! ```text
//! <service cgroup>/service              laded itself (cgroup v2 forbids processes in inner nodes)
//! <service cgroup>/sandboxes/<objective>/<kind>-<name>
//! ```
//!
//! Each Objective gets one cgroup with `memory.max`, `pids.max` and `cpu.max` from
//! [`OBJECTIVE_CGROUP_LIMITS`](super::OBJECTIVE_CGROUP_LIMITS), shared by its worker, terminals
//! and verification runs (one leaf each). The launcher moves a sandbox's outer bwrap and its
//! PID-namespace init into the leaf while the launch gate is closed, before the sandboxed
//! program starts, so every descendant is accounted. Without delegation (no cgroup v2, a cgroup
//! not delegated by systemd, missing controllers) [`detect`] returns the reason and execution
//! falls back to the `prlimit` limits only; it never writes into a cgroup it was not delegated.

use std::fs;
use std::io::{self, ErrorKind};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use super::limits::CgroupLimits;

const MOUNT: &str = "/sys/fs/cgroup";
const CONTROLLERS: [&str; 3] = ["memory", "pids", "cpu"];
const ENABLE: &str = "+memory +pids +cpu";
const SERVICE_LEAF: &str = "service";
const SANDBOXES: &str = "sandboxes";
const KILL_WAIT: Duration = Duration::from_secs(2);

/// The service's delegated cgroup subtree, prepared for sandbox placement.
#[derive(Debug)]
pub struct SandboxCgroups {
    root: PathBuf,
    limits: CgroupLimits,
}

/// Detects a delegated cgroup v2 subtree for this process and prepares it (moving this process
/// into `<own cgroup>/service`). `Err` is the human-readable reason it is unavailable;
/// `LADE_CGROUPS=off` disables it explicitly.
pub fn detect(limits: CgroupLimits) -> Result<SandboxCgroups, String> {
    if std::env::var_os("LADE_CGROUPS").is_some_and(|v| v == "off") {
        return Err("disabled by LADE_CGROUPS=off".into());
    }
    let membership = fs::read_to_string("/proc/self/cgroup")
        .map_err(|e| format!("/proc/self/cgroup unreadable ({})", e.kind()))?;
    detect_in(&membership, Path::new(MOUNT), limits, std::process::id())
}

/// [`detect`] for a given `/proc/<pid>/cgroup` text and cgroup v2 mount point.
pub fn detect_in(
    membership: &str,
    mount: &Path,
    limits: CgroupLimits,
    pid: u32,
) -> Result<SandboxCgroups, String> {
    let own =
        own_cgroup(membership).ok_or("no cgroup v2 membership (cgroup v1 or hybrid hierarchy)")?;
    if !mount.join("cgroup.controllers").is_file() {
        return Err(format!("cgroup v2 is not mounted at {}", mount.display()));
    }
    let dir = mount.join(own.trim_start_matches('/'));
    if !delegated(&dir) {
        return Err(format!(
            "the service cgroup {own} is not delegated (run laded from the lade.service unit, which sets Delegate=)"
        ));
    }
    let owner = fs::metadata(&dir)
        .map_err(|e| format!("{own}: {}", e.kind()))?
        .uid();
    if owner != rustix::process::geteuid().as_raw() {
        return Err(format!(
            "the service cgroup {own} is not owned by the service user"
        ));
    }
    SandboxCgroups::prepare(&dir, limits, pid)
}

/// The cgroup v2 path (`0::<path>`) from `/proc/<pid>/cgroup` text.
fn own_cgroup(membership: &str) -> Option<&str> {
    membership
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .map(str::trim)
        .filter(|path| path.starts_with('/'))
}

fn delegated(dir: &Path) -> bool {
    ["user.delegate", "trusted.delegate"].iter().any(|name| {
        let mut value = [0u8; 8];
        rustix::fs::getxattr(dir, *name, &mut value).is_ok_and(|n| &value[..n] == b"1")
    })
}

fn write(path: &Path, value: &str) -> io::Result<()> {
    fs::write(path, value)
}

fn mkdir(path: &Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(()),
        other => other,
    }
}

impl SandboxCgroups {
    /// Prepares `dir` (a delegated cgroup, or a test directory): moves `pid` into `dir/service`,
    /// enables the controllers for `dir` and `dir/sandboxes`.
    pub fn prepare(dir: &Path, limits: CgroupLimits, pid: u32) -> Result<Self, String> {
        let fail = |what: &str, e: io::Error| format!("{what}: {}", e.kind());
        let available = fs::read_to_string(dir.join("cgroup.controllers"))
            .map_err(|e| fail("cgroup.controllers", e))?;
        let missing: Vec<&str> = CONTROLLERS
            .iter()
            .copied()
            .filter(|c| !available.split_whitespace().any(|a| a == *c))
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "controllers not delegated: {} (the unit needs Delegate=memory pids cpu)",
                missing.join(" ")
            ));
        }
        let service = dir.join(SERVICE_LEAF);
        mkdir(&service).map_err(|e| fail("service cgroup", e))?;
        write(&service.join("cgroup.procs"), &pid.to_string())
            .map_err(|e| fail("moving the service into its leaf cgroup", e))?;
        write(&dir.join("cgroup.subtree_control"), ENABLE)
            .map_err(|e| fail("enabling controllers", e))?;
        let root = dir.join(SANDBOXES);
        mkdir(&root).map_err(|e| fail("sandboxes cgroup", e))?;
        write(&root.join("cgroup.subtree_control"), ENABLE)
            .map_err(|e| fail("enabling sandbox controllers", e))?;
        Ok(Self { root, limits })
    }

    /// `<service cgroup>/sandboxes`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn limits(&self) -> CgroupLimits {
        self.limits
    }

    /// Creates (or reuses) the Objective's limited cgroup and a fresh leaf `name` in it.
    pub fn leaf(&self, objective: &str, name: &str) -> io::Result<CgroupLeaf> {
        let safe = |s: &str| {
            (1..=96).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        };
        if !safe(objective) || !safe(name) {
            return Err(io::Error::new(ErrorKind::InvalidInput, "cgroup name"));
        }
        let group = self.root.join(objective);
        let path = group.join(name);
        // A leaf of the same Objective that ends concurrently may remove the (then empty)
        // Objective cgroup between the two steps: recreate it once.
        for attempt in 0..2 {
            match fs::create_dir(&group) {
                Ok(()) => self.apply_limits(&group)?,
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
            match fs::create_dir(&path) {
                Ok(()) => return Ok(CgroupLeaf { path, group }),
                Err(e) if e.kind() == ErrorKind::NotFound && attempt == 0 => {}
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::from(ErrorKind::NotFound))
    }

    fn apply_limits(&self, group: &Path) -> io::Result<()> {
        let limits = self.limits;
        let max = |v: Option<u64>| v.map_or_else(|| "max".to_string(), |v| v.to_string());
        write(&group.join("memory.max"), &max(limits.memory_max))?;
        write(&group.join("pids.max"), &max(limits.pids_max))?;
        let cpu = limits.cpu_max.map_or_else(
            || "max 100000".to_string(),
            |(quota, period)| format!("{quota} {period}"),
        );
        write(&group.join("cpu.max"), &cpu)?;
        write(&group.join("cgroup.subtree_control"), ENABLE)
    }

    /// Kills and removes every sandbox cgroup a previous instance left (service start only).
    /// Returns how many leaves were removed.
    pub fn sweep(&self) -> usize {
        let mut removed = 0;
        for group in subdirs(&self.root) {
            for leaf in subdirs(&group) {
                kill_and_remove(&leaf);
                removed += usize::from(!leaf.exists());
            }
            let _ = fs::remove_dir(&group);
        }
        removed
    }
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default()
}

/// `cgroup.kill` (Linux ≥ 5.14) ends every process of the cgroup; then wait for it to empty and
/// remove it.
fn kill_and_remove(leaf: &Path) {
    let _ = write(&leaf.join("cgroup.kill"), "1");
    let deadline = Instant::now() + KILL_WAIT;
    while populated(leaf) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    let _ = fs::remove_dir(leaf);
}

fn populated(leaf: &Path) -> bool {
    fs::read_to_string(leaf.join("cgroup.events"))
        .is_ok_and(|events| events.lines().any(|l| l == "populated 1"))
}

/// One sandbox's leaf cgroup. Dropping it kills what is left in it and removes it (and the
/// Objective's cgroup once that is empty).
#[derive(Debug)]
pub struct CgroupLeaf {
    path: PathBuf,
    group: PathBuf,
}

impl CgroupLeaf {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Moves `pid` (all its threads) into the leaf.
    pub fn add(&self, pid: u32) -> io::Result<()> {
        write(&self.path.join("cgroup.procs"), &pid.to_string())
    }

    /// Whether `pid` is a member (from `/proc/<pid>/cgroup`).
    pub fn contains(&self, pid: u32) -> bool {
        let Ok(text) = fs::read_to_string(format!("/proc/{pid}/cgroup")) else {
            return false;
        };
        own_cgroup(&text)
            .is_some_and(|path| Path::new(MOUNT).join(path.trim_start_matches('/')) == self.path)
    }

    /// OOM kills recorded for the Objective (`memory.events` `oom_kill`).
    pub fn oom_kills(&self) -> u64 {
        fs::read_to_string(self.path.join("memory.events"))
            .ok()
            .and_then(|events| {
                events
                    .lines()
                    .find_map(|l| l.strip_prefix("oom_kill ")?.trim().parse().ok())
            })
            .unwrap_or(0)
    }
}

impl Drop for CgroupLeaf {
    fn drop(&mut self) {
        kill_and_remove(&self.path);
        let _ = fs::remove_dir(&self.group);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn membership_parsing() {
        assert_eq!(
            own_cgroup("0::/user.slice/user-1000.slice/user@1000.service/app.slice/lade.service\n"),
            Some("/user.slice/user-1000.slice/user@1000.service/app.slice/lade.service")
        );
        assert_eq!(own_cgroup("9:name=systemd:/\n4:memory:/x\n"), None);
        assert_eq!(own_cgroup("1:cpu:/\n0::/\n"), Some("/"));
    }
}
