//! Optional cgroup v2 placement of sandboxes inside the service's delegated subtree.
//!
//! When `laded` runs from the shipped unit (`Delegate=memory pids cpu`), systemd hands it its own
//! cgroup. [`detect`] accepts that cgroup only on unambiguous evidence of delegation (see
//! [`detect_in`]) and then arranges, below it:
//!
//! ```text
//! <service cgroup>/service              laded, and whatever else was in the cgroup (cgroup v2
//!                                       forbids processes in inner nodes)
//! <service cgroup>/sandboxes/<objective>/<kind>-<name>
//! ```
//!
//! Each Objective gets one cgroup with `memory.max`, `pids.max` and `cpu.max` from
//! [`OBJECTIVE_CGROUP_LIMITS`](super::OBJECTIVE_CGROUP_LIMITS), shared by its worker, terminals
//! and verification runs (one leaf each). The launcher moves a sandbox's outer bwrap and its
//! PID-namespace init into the leaf while the launch gate is closed, before the sandboxed
//! program starts, so every descendant is accounted. Without delegation (no cgroup v2, a cgroup
//! not delegated by systemd, missing controllers, limits that do not bite) [`detect`] returns the
//! reason and execution falls back to the `prlimit` limits only; it never writes into a cgroup it
//! was not delegated.
//!
//! Ownership alone is not evidence of delegation. Below `user@<uid>.service` the system manager
//! hands the whole tree to the user, so every cgroup of the user manager (a terminal's app scope,
//! a service without `Delegate=`) is owned by and writable for the user. Writing into a cgroup
//! the user manager did not delegate would reorganise another application's processes, or leave
//! the limits at the mercy of the manager, which considers that cgroup's `cgroup.subtree_control`
//! its own and may rewrite it (dropping the `pids` and `memory` controllers, and with them every
//! sandbox limit) without LADE noticing. Delegation must therefore be stated by the manager that
//! owns the cgroup, and the limits are then proven to bite before they are relied on.

use std::fs;
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::limits::CgroupLimits;

const MOUNT: &str = "/sys/fs/cgroup";
const CONTROLLERS: [&str; 3] = ["memory", "pids", "cpu"];
const ENABLE: &str = "+memory +pids +cpu";
const SERVICE_LEAF: &str = "service";
const SANDBOXES: &str = "sandboxes";
/// The throw-away cgroup [`detect`] proves enforcement in (never a valid Objective id).
const PROBE: &str = "delegation-probe";
const KILL_WAIT: Duration = Duration::from_secs(2);
/// How long the systemd user manager, or the enforcement probe, may take before detection gives
/// up (and falls back).
const QUERY_WAIT: Duration = Duration::from_secs(5);
/// Passes over `cgroup.procs` when moving the delegated cgroup's processes into the service leaf.
const MOVE_PASSES: usize = 8;
const CGROUP2_SUPER_MAGIC: i128 = 0x6367_7270;
const SYSTEMCTL: [&str; 2] = ["/usr/bin/systemctl", "/bin/systemctl"];

/// The service's delegated cgroup subtree, prepared for sandbox placement.
#[derive(Debug)]
pub struct SandboxCgroups {
    root: PathBuf,
    limits: CgroupLimits,
    delegation: String,
}

/// Detects a delegated cgroup v2 subtree for this process, prepares it (moving the cgroup's
/// processes into `<own cgroup>/service`) and proves on the real kernel that a `pids.max` set
/// below it refuses a fork. `Err` is the human-readable reason it is unavailable;
/// `LADE_CGROUPS=off` disables it explicitly.
pub fn detect(limits: CgroupLimits) -> Result<SandboxCgroups, String> {
    if std::env::var_os("LADE_CGROUPS").is_some_and(|v| v == "off") {
        return Err("disabled by LADE_CGROUPS=off".into());
    }
    let membership = fs::read_to_string("/proc/self/cgroup")
        .map_err(|e| format!("/proc/self/cgroup unreadable ({})", e.kind()))?;
    let cgroups = detect_in(
        &membership,
        Path::new(MOUNT),
        limits,
        std::process::id(),
        ask_user_manager,
    )?;
    cgroups.prove_enforcement()?;
    Ok(cgroups)
}

/// [`detect`] for a given `/proc/<pid>/cgroup` text and cgroup v2 mount point, without the
/// enforcement proof (which needs the real kernel). `user_manager` returns what
/// `systemctl --user show --property=ControlGroup --property=Delegate -- <unit>` prints.
///
/// The cgroup is accepted only when all of this holds, checked before anything is written:
/// - it is the process's cgroup v2 cgroup and is owned by the effective user;
/// - its manager states the delegation: the `trusted.delegate` or `user.delegate` extended
///   attribute is `1` (set by the system manager), or the cgroup is a `.service` or `.scope` of
///   the systemd user manager `user@<euid>.service`, which reports that unit with exactly this
///   `ControlGroup=` and `Delegate=yes` (a user manager sets no delegation attribute, at least up
///   to systemd 255);
/// - its `cgroup.procs`, `cgroup.threads` and `cgroup.subtree_control` are writable, and its
///   `cgroup.controllers` offers `memory pids cpu` (see [`SandboxCgroups::prepare`]).
pub fn detect_in(
    membership: &str,
    mount: &Path,
    limits: CgroupLimits,
    pid: u32,
    user_manager: impl FnOnce(&str) -> Result<String, String>,
) -> Result<SandboxCgroups, String> {
    let own =
        own_cgroup(membership).ok_or("no cgroup v2 membership (cgroup v1 or hybrid hierarchy)")?;
    if !mount.join("cgroup.controllers").is_file() {
        return Err(format!("cgroup v2 is not mounted at {}", mount.display()));
    }
    let dir = mount.join(own.trim_start_matches('/'));
    let euid = rustix::process::geteuid().as_raw();
    let owner = fs::metadata(&dir)
        .map_err(|e| format!("{own}: {}", e.kind()))?
        .uid();
    if owner != euid {
        return Err(format!(
            "the service cgroup {own} is not owned by the service user"
        ));
    }
    let delegation = delegation(&dir, own, euid, user_manager)?;
    for file in ["cgroup.procs", "cgroup.threads", "cgroup.subtree_control"] {
        rustix::fs::access(dir.join(file), rustix::fs::Access::WRITE_OK).map_err(|e| {
            format!("the service cgroup {own} is delegated but its {file} is not writable ({e})")
        })?;
    }
    let cgroups = SandboxCgroups::prepare(&dir, limits, pid)?;
    Ok(SandboxCgroups {
        delegation,
        ..cgroups
    })
}

/// How delegation of `dir` (the cgroup `own`) is established, or why it is not.
fn delegation(
    dir: &Path,
    own: &str,
    euid: u32,
    user_manager: impl FnOnce(&str) -> Result<String, String>,
) -> Result<String, String> {
    let marked = ["trusted.delegate", "user.delegate"]
        .into_iter()
        .find(|name| {
            let mut value = [0u8; 8];
            rustix::fs::getxattr(dir, *name, &mut value).is_ok_and(|n| &value[..n] == b"1")
        });
    if let Some(name) = marked {
        return Ok(format!("{name} attribute on {own}"));
    }
    let not_delegated = |why: String| {
        format!(
            "the service cgroup {own} is not delegated ({why}; run laded from the lade.service unit, which sets Delegate=)"
        )
    };
    let Some(unit) = user_manager_unit(own, euid) else {
        return Err(not_delegated(format!(
            "no delegate attribute, and not a service or scope of the systemd user manager user@{euid}.service"
        )));
    };
    let shown = user_manager(unit).map_err(|e| {
        not_delegated(format!(
            "no delegate attribute, and the systemd user manager could not be asked about {unit}: {e}"
        ))
    })?;
    let property = |key: &str| {
        shown
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .map(str::trim)
    };
    if property("ControlGroup") != Some(own) {
        return Err(not_delegated(format!(
            "the systemd user manager does not place {unit} in this cgroup"
        )));
    }
    match property("Delegate") {
        Some("yes") => Ok(format!("systemd user manager: {unit} has Delegate=yes")),
        other => Err(not_delegated(format!(
            "the systemd user manager reports Delegate={} for {unit}",
            other.unwrap_or("?")
        ))),
    }
}

/// The unit name when `own` is a `.service` or `.scope` cgroup below `user@<euid>.service`, the
/// only cgroups that user manager is authoritative for.
fn user_manager_unit(own: &str, euid: u32) -> Option<&str> {
    let (_, below) = own.split_once(&format!("/user@{euid}.service/"))?;
    let unit = below.rsplit('/').next()?;
    (unit.ends_with(".service") || unit.ends_with(".scope")).then_some(unit)
}

/// `systemctl --user show` for `unit`: absolute path, an empty environment but for the runtime
/// directory (which locates the manager's private socket), bounded in time.
fn ask_user_manager(unit: &str) -> Result<String, String> {
    let systemctl = SYSTEMCTL
        .into_iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .ok_or("systemctl not found in /usr/bin or /bin")?;
    let mut command = Command::new(systemctl);
    command
        .args([
            "--user",
            "--no-pager",
            "show",
            "--property=ControlGroup",
            "--property=Delegate",
            "--",
            unit,
        ])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        command.env("XDG_RUNTIME_DIR", runtime);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("systemctl: {}", e.kind()))?;
    // The answer is two short lines, far below the pipe buffer: waiting first cannot block it.
    let status = wait_bounded(&mut child).ok_or("systemctl did not answer in time")?;
    let mut shown = String::new();
    child
        .stdout
        .take()
        .ok_or("systemctl output")?
        .take(64 << 10)
        .read_to_string(&mut shown)
        .map_err(|e| format!("systemctl output: {}", e.kind()))?;
    if !status.success() {
        return Err(format!("systemctl failed ({status})"));
    }
    Ok(shown)
}

/// Waits up to [`QUERY_WAIT`] for `child`; kills and reaps it on timeout (`None`).
fn wait_bounded(child: &mut Child) -> Option<ExitStatus> {
    let deadline = Instant::now() + QUERY_WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// The cgroup v2 path (`0::<path>`) from `/proc/<pid>/cgroup` text.
fn own_cgroup(membership: &str) -> Option<&str> {
    membership
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .map(str::trim)
        .filter(|path| path.starts_with('/'))
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

/// The process ids in `dir/cgroup.procs` (none when it is unreadable).
fn members(dir: &Path) -> Vec<u32> {
    fs::read_to_string(dir.join("cgroup.procs"))
        .map(|procs| {
            procs
                .lines()
                .filter_map(|l| l.trim().parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

impl SandboxCgroups {
    /// Prepares `dir` (a delegated cgroup, or a test directory): moves `pid`, then any other
    /// process still in `dir`, into `dir/service`, and enables the controllers for `dir` and
    /// `dir/sandboxes`.
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
        let procs = service.join("cgroup.procs");
        write(&procs, &pid.to_string())
            .map_err(|e| fail("moving the service into its leaf cgroup", e))?;
        // cgroup v2 refuses controllers to a cgroup that still holds processes. Whatever else is
        // in the delegated cgroup (the parent of a test harness, say) belongs to the delegatee
        // too: move it along. A process that exits meanwhile fails its write; the next pass
        // re-reads the list.
        for _ in 0..MOVE_PASSES {
            let rest = members(dir);
            if rest.is_empty() {
                break;
            }
            for other in rest {
                let _ = write(&procs, &other.to_string());
            }
        }
        let rest = members(dir);
        if !rest.is_empty() {
            return Err(format!(
                "the service cgroup still holds processes that could not be moved: {rest:?}"
            ));
        }
        write(&dir.join("cgroup.subtree_control"), ENABLE)
            .map_err(|e| fail("enabling controllers", e))?;
        let root = dir.join(SANDBOXES);
        mkdir(&root).map_err(|e| fail("sandboxes cgroup", e))?;
        write(&root.join("cgroup.subtree_control"), ENABLE)
            .map_err(|e| fail("enabling sandbox controllers", e))?;
        Ok(Self {
            root,
            limits,
            delegation: format!("prepared at {}", dir.display()),
        })
    }

    /// `<service cgroup>/sandboxes`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn limits(&self) -> CgroupLimits {
        self.limits
    }

    /// How delegation was established, for the service log.
    pub fn delegation(&self) -> &str {
        &self.delegation
    }

    /// Proves on the real kernel that limits set below [`root`](Self::root) bite: a probe cgroup
    /// on cgroupfs offers the controllers and takes `pids.max` = 1, and a shell moved into it is
    /// refused its next fork (counted in `pids.events`). The probe is removed afterwards.
    fn prove_enforcement(&self) -> Result<(), String> {
        let probe = self.root.join(PROBE);
        kill_and_remove(&probe);
        fs::create_dir(&probe).map_err(|e| format!("enforcement probe cgroup: {}", e.kind()))?;
        let proved = prove_in(&probe);
        kill_and_remove(&probe);
        proved.map_err(|why| format!("delegated cgroup limits are not enforced: {why}"))
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

/// [`SandboxCgroups::prove_enforcement`] in the fresh cgroup `probe`.
fn prove_in(probe: &Path) -> Result<(), String> {
    let fail = |what: &str, e: io::Error| format!("{what}: {}", e.kind());
    let fs_type = rustix::fs::statfs(probe)
        .map_err(|e| format!("statfs: {e}"))?
        .f_type;
    if i128::from(fs_type) != CGROUP2_SUPER_MAGIC {
        return Err(format!("{} is not on cgroupfs v2", probe.display()));
    }
    let offered = fs::read_to_string(probe.join("cgroup.controllers"))
        .map_err(|e| fail("cgroup.controllers", e))?;
    if let Some(missing) = CONTROLLERS
        .iter()
        .find(|c| !offered.split_whitespace().any(|a| a == **c))
    {
        return Err(format!(
            "the {missing} controller is not active below the service"
        ));
    }
    write(&probe.join("pids.max"), "1").map_err(|e| fail("pids.max", e))?;
    let set = fs::read_to_string(probe.join("pids.max")).map_err(|e| fail("pids.max", e))?;
    if set.trim() != "1" {
        return Err(format!("pids.max reads {:?} after writing 1", set.trim()));
    }
    // The shell waits for a line on its input, sent only once it is in the probe, and must then
    // fork for /bin/true: one process more than pids.max allows.
    let mut shell = Command::new("/bin/sh")
        .args(["-c", "read line; /bin/true; echo forked"])
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| fail("probe shell", e))?;
    let released = write(&probe.join("cgroup.procs"), &shell.id().to_string())
        .map_err(|e| fail("moving the probe shell", e))
        .and_then(|()| match shell.stdin.take() {
            Some(mut input) => input.write_all(b"\n").map_err(|e| fail("probe input", e)),
            None => Err("probe input".into()),
        });
    // Done when the refused fork is counted or the shell exits (a shell that retries a failed
    // fork, like bash, would otherwise hold the probe for seconds).
    let deadline = Instant::now() + QUERY_WAIT;
    while released.is_ok()
        && fork_refusals(probe) == 0
        && matches!(shell.try_wait(), Ok(None))
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(5));
    }
    let refused = fork_refusals(probe);
    let _ = shell.kill();
    let _ = shell.wait();
    released?;
    let mut said = String::new();
    if let Some(mut out) = shell.stdout.take() {
        let _ = out.read_to_string(&mut said);
    }
    if refused == 0 || said.contains("forked") {
        return Err("a fork beyond pids.max = 1 was not refused".into());
    }
    Ok(())
}

/// `max` in `pids.events`: the forks the `pids` controller refused in this cgroup.
fn fork_refusals(group: &Path) -> u64 {
    fs::read_to_string(group.join("pids.events"))
        .ok()
        .and_then(|events| {
            events
                .lines()
                .find_map(|l| l.strip_prefix("max ")?.trim().parse().ok())
        })
        .unwrap_or(0)
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

    #[test]
    fn only_services_and_scopes_of_the_own_user_manager_are_asked_about() {
        let base = "/user.slice/user-1000.slice/user@1000.service";
        let unit = user_manager_unit;
        assert_eq!(
            unit(&format!("{base}/app.slice/lade.service"), 1000),
            Some("lade.service")
        );
        assert_eq!(
            unit(&format!("{base}/app.slice/run-r1.scope"), 1000),
            Some("run-r1.scope")
        );
        // Another user's manager, a slice, a sub-cgroup of a unit, a system unit.
        assert_eq!(unit(&format!("{base}/app.slice/lade.service"), 1001), None);
        assert_eq!(unit(&format!("{base}/app.slice"), 1000), None);
        assert_eq!(
            unit(&format!("{base}/app.slice/lade.service/service"), 1000),
            None
        );
        assert_eq!(unit("/system.slice/lade.service", 1000), None);
        assert_eq!(
            unit(&format!("{base}/init.scope"), 1000),
            Some("init.scope")
        );
    }
}
