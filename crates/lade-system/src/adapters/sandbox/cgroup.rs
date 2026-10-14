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

#[derive(Debug)]
pub struct SandboxCgroups {
    root: PathBuf,
    limits: CgroupLimits,
}

pub fn detect(limits: CgroupLimits) -> Result<SandboxCgroups, String> {
    if std::env::var_os("LADE_CGROUPS").is_some_and(|v| v == "off") {
        return Err("disabled by LADE_CGROUPS=off".into());
    }
    let membership = fs::read_to_string("/proc/self/cgroup")
        .map_err(|e| format!("/proc/self/cgroup unreadable ({})", e.kind()))?;
    detect_in(&membership, Path::new(MOUNT), limits, std::process::id())
}

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

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn limits(&self) -> CgroupLimits {
        self.limits
    }

    pub fn leaf(&self, objective: &str, name: &str) -> io::Result<CgroupLeaf> {
        let safe = |s: &str| {
            (1..=96).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        };
        if !safe(objective) || !safe(name) {
            return Err(io::Error::new(ErrorKind::InvalidInput, "cgroup name"));
        }
        let group = self.root.join(objective);
        let path = group.join(name);
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

#[derive(Debug)]
pub struct CgroupLeaf {
    path: PathBuf,
    group: PathBuf,
}

impl CgroupLeaf {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn add(&self, pid: u32) -> io::Result<()> {
        write(&self.path.join("cgroup.procs"), &pid.to_string())
    }

    pub fn contains(&self, pid: u32) -> bool {
        let Ok(text) = fs::read_to_string(format!("/proc/{pid}/cgroup")) else {
            return false;
        };
        own_cgroup(&text)
            .is_some_and(|path| Path::new(MOUNT).join(path.trim_start_matches('/')) == self.path)
    }

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
