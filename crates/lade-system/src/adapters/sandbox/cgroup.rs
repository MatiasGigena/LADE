use std::fmt;
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
const PROBE: &str = "delegation-probe";
const KILL_WAIT: Duration = Duration::from_secs(2);
const QUERY_WAIT: Duration = Duration::from_secs(5);
const MOVE_PASSES: usize = 8;
const CGROUP2_SUPER_MAGIC: i128 = 0x6367_7270;
const SYSTEMCTL: [&str; 2] = ["/usr/bin/systemctl", "/bin/systemctl"];

/// Why delegated cgroup limits are unavailable. The service reports it once at startup and runs
/// its sandboxes with the prlimit limits only.
#[derive(Debug)]
pub enum CgroupError {
    /// `LADE_CGROUPS=off` is set.
    Disabled,
    /// The service has no cgroup v2 membership (cgroup v1 or a hybrid hierarchy).
    NoV2Membership,
    /// cgroup v2 is not mounted where the membership points.
    NotMounted(PathBuf),
    /// The service cgroup belongs to another user.
    NotOwned { cgroup: String },
    /// The service cgroup is not delegated to the service; `why` names the missing evidence.
    NotDelegated { cgroup: String, why: String },
    /// The service cgroup is delegated, but one of its interface files is not writable.
    NotWritable {
        cgroup: String,
        file: &'static str,
        error: rustix::io::Errno,
    },
    /// Controllers the service unit does not delegate.
    MissingControllers(Vec<&'static str>),
    /// Processes left in the service cgroup that could not be moved into its leaf.
    Unmovable(Vec<u32>),
    /// A cgroup file could not be read or written.
    Io { what: String, kind: ErrorKind },
    /// The delegated limits are not enforced; `why` names the failed probe step.
    NotEnforced(String),
}

impl CgroupError {
    fn io(what: impl Into<String>) -> impl FnOnce(io::Error) -> Self {
        let what = what.into();
        move |e| Self::Io {
            what,
            kind: e.kind(),
        }
    }
}

impl fmt::Display for CgroupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => f.write_str("disabled by LADE_CGROUPS=off"),
            Self::NoV2Membership => {
                f.write_str("no cgroup v2 membership (cgroup v1 or hybrid hierarchy)")
            }
            Self::NotMounted(mount) => {
                write!(f, "cgroup v2 is not mounted at {}", mount.display())
            }
            Self::NotOwned { cgroup } => write!(
                f,
                "the service cgroup {cgroup} is not owned by the service user"
            ),
            Self::NotDelegated { cgroup, why } => write!(
                f,
                "the service cgroup {cgroup} is not delegated ({why}; run laded from the lade.service unit, which sets Delegate=)"
            ),
            Self::NotWritable {
                cgroup,
                file,
                error,
            } => write!(
                f,
                "the service cgroup {cgroup} is delegated but its {file} is not writable ({error})"
            ),
            Self::MissingControllers(missing) => write!(
                f,
                "controllers not delegated: {} (the unit needs Delegate=memory pids cpu)",
                missing.join(" ")
            ),
            Self::Unmovable(rest) => write!(
                f,
                "the service cgroup still holds processes that could not be moved: {rest:?}"
            ),
            Self::Io { what, kind } => write!(f, "{what}: {kind}"),
            Self::NotEnforced(why) => {
                write!(f, "delegated cgroup limits are not enforced: {why}")
            }
        }
    }
}

impl std::error::Error for CgroupError {}

#[derive(Debug)]
pub struct SandboxCgroups {
    root: PathBuf,
    limits: CgroupLimits,
    delegation: String,
}

/// Finds the service's delegated cgroup, prepares it for per-sandbox leaves and proves
/// that its limits are enforced.
///
/// # Errors
///
/// Returns the [`CgroupError`] that says why delegated limits are unavailable: disabled by
/// `LADE_CGROUPS=off`, no cgroup v2, not owned or not delegated, missing controllers, unmovable
/// processes, an unreadable or unwritable file, or limits that a probe shows are not enforced.
pub fn detect(limits: CgroupLimits) -> Result<SandboxCgroups, CgroupError> {
    if std::env::var_os("LADE_CGROUPS").is_some_and(|v| v == "off") {
        return Err(CgroupError::Disabled);
    }
    let membership =
        fs::read_to_string("/proc/self/cgroup").map_err(CgroupError::io("/proc/self/cgroup"))?;
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

/// The detection steps of [`detect`] against the given membership text, mount point and
/// user manager, without the enforcement probe, so tests can run them on a fake hierarchy.
///
/// # Errors
///
/// Returns the [`CgroupError`] of the first check that fails.
pub fn detect_in(
    membership: &str,
    mount: &Path,
    limits: CgroupLimits,
    pid: u32,
    user_manager: impl FnOnce(&str) -> Result<String, String>,
) -> Result<SandboxCgroups, CgroupError> {
    let own = own_cgroup(membership).ok_or(CgroupError::NoV2Membership)?;
    if !mount.join("cgroup.controllers").is_file() {
        return Err(CgroupError::NotMounted(mount.to_path_buf()));
    }
    let dir = mount.join(own.trim_start_matches('/'));
    let euid = rustix::process::geteuid().as_raw();
    let owner = fs::metadata(&dir).map_err(CgroupError::io(own))?.uid();
    if owner != euid {
        return Err(CgroupError::NotOwned { cgroup: own.into() });
    }
    let delegation =
        delegation(&dir, own, euid, user_manager).map_err(|why| CgroupError::NotDelegated {
            cgroup: own.into(),
            why,
        })?;
    for file in ["cgroup.procs", "cgroup.threads", "cgroup.subtree_control"] {
        rustix::fs::access(dir.join(file), rustix::fs::Access::WRITE_OK).map_err(|error| {
            CgroupError::NotWritable {
                cgroup: own.into(),
                file,
                error,
            }
        })?;
    }
    let cgroups = SandboxCgroups::prepare(&dir, limits, pid)?;
    Ok(SandboxCgroups {
        delegation,
        ..cgroups
    })
}

/// Names the evidence that `own` is delegated to the service, or why it is not.
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
    let Some(unit) = user_manager_unit(own, euid) else {
        return Err(format!(
            "no delegate attribute, and not a service or scope of the systemd user manager user@{euid}.service"
        ));
    };
    let shown = user_manager(unit).map_err(|e| {
        format!(
            "no delegate attribute, and the systemd user manager could not be asked about {unit}: {e}"
        )
    })?;
    let property = |key: &str| {
        shown
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .map(str::trim)
    };
    if property("ControlGroup") != Some(own) {
        return Err(format!(
            "the systemd user manager does not place {unit} in this cgroup"
        ));
    }
    match property("Delegate") {
        Some("yes") => Ok(format!("systemd user manager: {unit} has Delegate=yes")),
        other => Err(format!(
            "the systemd user manager reports Delegate={} for {unit}",
            other.unwrap_or("?")
        )),
    }
}

fn user_manager_unit(own: &str, euid: u32) -> Option<&str> {
    let (_, below) = own.split_once(&format!("/user@{euid}.service/"))?;
    let unit = below.rsplit('/').next()?;
    (unit.ends_with(".service") || unit.ends_with(".scope")).then_some(unit)
}

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

fn own_cgroup(membership: &str) -> Option<&str> {
    membership
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .map(str::trim)
        .filter(|path| path.starts_with('/'))
}

fn mkdir(path: &Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(()),
        other => other,
    }
}

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
    /// Moves the service (and every other process in `dir`) into a `service` leaf and creates
    /// the `sandboxes` subtree with the memory, pids and cpu controllers enabled.
    ///
    /// # Errors
    ///
    /// Returns [`CgroupError::MissingControllers`] when `dir` lacks a controller,
    /// [`CgroupError::Unmovable`] when processes stay in `dir`, or [`CgroupError::Io`] naming the step
    /// that failed.
    pub fn prepare(dir: &Path, limits: CgroupLimits, pid: u32) -> Result<Self, CgroupError> {
        let available = fs::read_to_string(dir.join("cgroup.controllers"))
            .map_err(CgroupError::io("cgroup.controllers"))?;
        let missing: Vec<&str> = CONTROLLERS
            .iter()
            .copied()
            .filter(|c| !available.split_whitespace().any(|a| a == *c))
            .collect();
        if !missing.is_empty() {
            return Err(CgroupError::MissingControllers(missing));
        }
        let service = dir.join(SERVICE_LEAF);
        mkdir(&service).map_err(CgroupError::io("service cgroup"))?;
        let procs = service.join("cgroup.procs");
        fs::write(&procs, pid.to_string())
            .map_err(CgroupError::io("moving the service into its leaf cgroup"))?;
        for _ in 0..MOVE_PASSES {
            let rest = members(dir);
            if rest.is_empty() {
                break;
            }
            for other in rest {
                let _ = fs::write(&procs, other.to_string());
            }
        }
        let rest = members(dir);
        if !rest.is_empty() {
            return Err(CgroupError::Unmovable(rest));
        }
        fs::write(dir.join("cgroup.subtree_control"), ENABLE)
            .map_err(CgroupError::io("enabling controllers"))?;
        let root = dir.join(SANDBOXES);
        mkdir(&root).map_err(CgroupError::io("sandboxes cgroup"))?;
        fs::write(root.join("cgroup.subtree_control"), ENABLE)
            .map_err(CgroupError::io("enabling sandbox controllers"))?;
        Ok(Self {
            root,
            limits,
            delegation: format!("prepared at {}", dir.display()),
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn limits(&self) -> CgroupLimits {
        self.limits
    }

    #[must_use]
    pub fn delegation(&self) -> &str {
        &self.delegation
    }

    fn prove_enforcement(&self) -> Result<(), CgroupError> {
        let probe = self.root.join(PROBE);
        kill_and_remove(&probe);
        fs::create_dir(&probe).map_err(CgroupError::io("enforcement probe cgroup"))?;
        let proved = prove_in(&probe);
        kill_and_remove(&probe);
        proved.map_err(CgroupError::NotEnforced)
    }

    /// Creates the leaf `name` under the Objective's group, creating the group with the
    /// configured limits first.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` when either name is empty, longer than 96 bytes or holds anything but
    /// ASCII letters, digits and `-`, or the operating system's error when a cgroup cannot be created
    /// or limited.
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
        fs::write(group.join("memory.max"), max(limits.memory_max))?;
        fs::write(group.join("pids.max"), max(limits.pids_max))?;
        let cpu = limits.cpu_max.map_or_else(
            || "max 100000".to_string(),
            |(quota, period)| format!("{quota} {period}"),
        );
        fs::write(group.join("cpu.max"), cpu)?;
        fs::write(group.join("cgroup.subtree_control"), ENABLE)
    }

    #[must_use]
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

/// Proves on `probe` that a pids limit is enforced, or names the failed step.
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
    fs::write(probe.join("pids.max"), "1").map_err(|e| fail("pids.max", e))?;
    let set = fs::read_to_string(probe.join("pids.max")).map_err(|e| fail("pids.max", e))?;
    if set.trim() != "1" {
        return Err(format!("pids.max reads {:?} after writing 1", set.trim()));
    }
    let mut shell = Command::new("/bin/sh")
        .args(["-c", "read line; /bin/true; echo forked"])
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| fail("probe shell", e))?;
    let released = fs::write(probe.join("cgroup.procs"), shell.id().to_string())
        .map_err(|e| fail("moving the probe shell", e))
        .and_then(|()| match shell.stdin.take() {
            Some(mut input) => input.write_all(b"\n").map_err(|e| fail("probe input", e)),
            None => Err("probe input".into()),
        });
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

fn kill_and_remove(leaf: &Path) {
    let _ = fs::write(leaf.join("cgroup.kill"), "1");
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
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Moves process `pid` into this leaf.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the process cannot be moved, for example because it
    /// already exited.
    pub fn add(&self, pid: u32) -> io::Result<()> {
        fs::write(self.path.join("cgroup.procs"), pid.to_string())
    }

    #[must_use]
    pub fn contains(&self, pid: u32) -> bool {
        let Ok(text) = fs::read_to_string(format!("/proc/{pid}/cgroup")) else {
            return false;
        };
        own_cgroup(&text)
            .is_some_and(|path| Path::new(MOUNT).join(path.trim_start_matches('/')) == self.path)
    }

    #[must_use]
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
