//! Pidfiles for launched sandboxes and the startup sweep of stale ones.
//!
//! Each launch records `worker-<objective>-<generation>.pid` (a contained terminal:
//! `terminal-<objective>-<terminal>.pid`) in the run directory: one line
//! `<pid> <starttime>` per process to kill (the outer bwrap, then the sandbox's PID-namespace
//! init once known). `starttime` is field 22 of `/proc/<pid>/stat` (clock ticks since boot), so
//! a recycled pid never matches. Launches write the pidfile (and place the sandbox in its cgroup)
//! while the sandbox's launch gate is still closed ([`record_sandbox`]), so the sandboxed program
//! never runs unrecorded while the supervisor lives. The service calls [`sweep_stale`] at
//! startup, before any launch, to kill sandboxes that survived a previous service — recorded
//! ones by pidfile, and every sandbox of this run directory by its command-line marker
//! ([`sandbox_marker`]), including one whose supervisor died after opening the gate but before
//! bwrap armed its parent-death signals — and to remove their gateway sockets and pidfiles.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal};
use sha2::{Digest, Sha256};

use crate::adapters::sandbox::{CgroupLeaf, find_program};

/// One recorded process identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PidRecord {
    pub pid: u32,
    pub start_time: u64,
}

impl PidRecord {
    /// The identity of a currently existing process (`None` if gone or a zombie).
    pub fn of(pid: u32) -> Option<Self> {
        let (state, start_time) = proc_stat(pid)?;
        (state != 'Z' && state != 'X').then_some(Self { pid, start_time })
    }

    /// True while the same (non-zombie) process still exists.
    pub fn is_alive(&self) -> bool {
        Self::of(self.pid).is_some_and(|now| now == *self)
    }
}

/// `(state, starttime)` from `/proc/<pid>/stat`.
fn proc_stat(pid: u32) -> Option<(char, u64)> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    let mut fields = rest.split_ascii_whitespace();
    let state = fields.next()?.chars().next()?;
    // Fields after the state start at field 4 (ppid); starttime is field 22.
    let start_time = fields.nth(18)?.parse().ok()?;
    Some((state, start_time))
}

/// Parent pid from `/proc/<pid>/stat` (field 4).
pub(crate) fn parent_pid(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_ascii_whitespace().nth(1)?.parse().ok()
}

/// Live direct children of `pid` (by scanning `/proc`).
pub(crate) fn children_of(pid: u32) -> Vec<u32> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|child| parent_pid(*child) == Some(pid))
        .collect()
}

/// Atomically write a pidfile (mode 0600).
pub fn write_pidfile(path: &Path, records: &[PidRecord]) -> io::Result<()> {
    let tmp = path.with_extension("pid.tmp");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    for record in records {
        writeln!(file, "{} {}", record.pid, record.start_time)?;
    }
    file.sync_all()?;
    fs::rename(&tmp, path)
}

/// How long a launch waits for bwrap to fork the sandbox's PID-namespace init.
const INIT_DISCOVERY: Duration = Duration::from_secs(5);

/// Records a sandbox spawned with a closed launch gate: writes `pidfile` with the outer process,
/// waits for bwrap's fork (the PID-namespace init, which waits on the gate), moves both into
/// `leaf` when given, then rewrites the pidfile with both. The caller opens the gate afterwards,
/// so the sandboxed program never runs unrecorded or outside its cgroup. Returns the init
/// (`None` when the sandbox already ended). `ended` reports whether the outer process exited.
pub(crate) fn record_sandbox(
    pidfile: &Path,
    pid: u32,
    leaf: Option<&CgroupLeaf>,
    ended: impl Fn() -> bool,
) -> io::Result<Option<PidRecord>> {
    let Some(outer) = PidRecord::of(pid) else {
        return Ok(None); // already exited; the monitor reports it
    };
    write_pidfile(pidfile, &[outer])?;
    let Some(init) = discover_init(pid, &ended)? else {
        return Ok(None);
    };
    if let Some(leaf) = leaf {
        place(leaf, pid, init.pid)?;
    }
    write_pidfile(pidfile, &[outer, init])?;
    Ok(Some(init))
}

/// Moves a sandbox spawned with a closed launch gate (outer process `pid` and bwrap's fork)
/// into `leaf` before the gate opens.
pub fn place_sandbox(pid: u32, leaf: &CgroupLeaf, ended: impl Fn() -> bool) -> io::Result<()> {
    match discover_init(pid, &ended)? {
        Some(init) => place(leaf, pid, init.pid),
        None => Ok(()),
    }
}

fn discover_init(pid: u32, ended: &impl Fn() -> bool) -> io::Result<Option<PidRecord>> {
    let deadline = Instant::now() + INIT_DISCOVERY;
    loop {
        if let Some(init) = children_of(pid).into_iter().find_map(PidRecord::of) {
            return Ok(Some(init));
        }
        if ended() {
            return Ok(None);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "sandbox namespace init not found",
            ));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// While the gate is closed the sandbox is the outer process and its init (no further children
/// yet; any that exist are moved too).
fn place(leaf: &CgroupLeaf, outer: u32, init: u32) -> io::Result<()> {
    let members = || [outer, init].into_iter().chain(children_of(init));
    for member in members() {
        leaf.add(member)?;
    }
    if members().all(|member| leaf.contains(member) || PidRecord::of(member).is_none()) {
        Ok(())
    } else {
        Err(io::Error::other("sandbox not placed in its cgroup"))
    }
}

/// Parse a pidfile; malformed lines are ignored.
pub fn read_pidfile(path: &Path) -> io::Result<Vec<PidRecord>> {
    let text = fs::read_to_string(path)?;
    Ok(text
        .lines()
        .take(16)
        .filter_map(|line| {
            let (pid, start) = line.trim().split_once(' ')?;
            Some(PidRecord {
                pid: pid.parse().ok().filter(|p| *p > 1)?,
                start_time: start.parse().ok()?,
            })
        })
        .collect())
}

/// `SIGKILL` one recorded process if its identity still matches. Returns whether it was sent.
pub(crate) fn kill_record(record: &PidRecord) -> bool {
    if !record.is_alive() {
        return false;
    }
    let Some(kill) = find_program("kill") else {
        return false;
    };
    Command::new(kill)
        .env_clear()
        .args(["-s", "KILL", "--", &record.pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The marker of every sandbox started for `run_dir` (see `SandboxSpec::marker`):
/// `LADE_SANDBOX_<16 hex of SHA-256 of the canonical run directory>`. One state directory has one
/// service (its lock), so a marker never matches another instance's sandboxes.
pub fn sandbox_marker(run_dir: &Path) -> String {
    let canonical = fs::canonicalize(run_dir).unwrap_or_else(|_| run_dir.to_path_buf());
    let digest = Sha256::digest(canonical.as_os_str().as_encoded_bytes());
    let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("LADE_SANDBOX_{hex}")
}

/// Live processes of this user whose command line carries `marker` as one whole argument: the
/// outer `prlimit`/`bwrap` of a sandbox and bwrap's own forks (the PID-namespace init among
/// them). A sandboxed program cannot appear here unless it forges the marker, which only gets
/// its own processes killed.
fn marked(marker: &str) -> Vec<u32> {
    let uid = rustix::process::geteuid().as_raw();
    let me = std::process::id();
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.metadata().is_ok_and(|m| m.uid() == uid))
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| *pid != me && PidRecord::of(*pid).is_some())
        .filter(|pid| {
            fs::read(format!("/proc/{pid}/cmdline"))
                .is_ok_and(|c| c.split(|b| *b == 0).any(|arg| arg == marker.as_bytes()))
        })
        .collect()
}

/// `SIGKILL`s every process carrying `marker` (see [`sandbox_marker`]) until none is left or 3 s
/// passed; returns the pids it signalled.
pub fn kill_marked(marker: &str) -> Vec<u32> {
    let mut killed = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let found = marked(marker);
        if found.is_empty() || Instant::now() >= deadline {
            return killed;
        }
        for pid in found {
            let target = i32::try_from(pid).ok().and_then(Pid::from_raw);
            if let Some(target) = target
                && rustix::process::kill_process(target, Signal::KILL).is_ok()
                && !killed.contains(&pid)
            {
                killed.push(pid);
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// What [`sweep_stale`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Processes killed because pid and start time still matched a pidfile.
    pub killed: Vec<u32>,
    /// Recorded processes that were gone or whose start time no longer matched (left alone).
    pub skipped: Vec<u32>,
    pub removed_pidfiles: usize,
    pub removed_sockets: usize,
}

/// Kill processes recorded in `run_dir`'s worker and terminal pidfiles whose identity still
/// matches, then remove those pidfiles and the gateway sockets there. Must only run while no
/// launcher or terminal is using `run_dir` (service startup). A missing `run_dir` is an empty
/// sweep.
pub fn sweep_stale(run_dir: &Path) -> io::Result<SweepReport> {
    let mut report = SweepReport::default();
    let entries = match fs::read_dir(run_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(report),
        Err(e) => return Err(e),
    };
    let mut pidfiles = Vec::new();
    let mut sockets = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let recorded = name.starts_with("worker-") || name.starts_with("terminal-");
        if recorded && (name.ends_with(".pid") || name.ends_with(".pid.tmp")) {
            pidfiles.push(entry.path());
        } else if name.starts_with("gw-") && name.ends_with(".sock") {
            sockets.push(entry.path());
        }
    }
    let mut killed = Vec::new();
    // Every sandbox started for this run directory carries its marker, recorded or not (a
    // supervisor that died before writing the pidfile): kill those first.
    for pid in kill_marked(&sandbox_marker(run_dir)) {
        report.killed.push(pid);
    }
    for path in &pidfiles {
        // Namespace init (killing it ends the whole PID namespace) before the outer bwrap.
        for record in read_pidfile(path).unwrap_or_default().iter().rev() {
            if kill_record(record) {
                report.killed.push(record.pid);
                killed.push(*record);
            } else {
                report.skipped.push(record.pid);
            }
        }
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while killed.iter().any(PidRecord::is_alive) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    for path in pidfiles {
        if fs::remove_file(path).is_ok() {
            report.removed_pidfiles += 1;
        }
    }
    for path in sockets {
        if fs::remove_file(path).is_ok() {
            report.removed_sockets += 1;
        }
    }
    Ok(report)
}

/// Remove every gateway socket (`*.sock`) left in `socket_dir` (`$RUNTIME/g`) by a previous
/// instance. Must only run while no launcher is using it (service startup). Returns the count.
pub fn sweep_sockets(socket_dir: &Path) -> io::Result<usize> {
    let entries = match fs::read_dir(socket_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let is_socket = entry.file_type().is_ok_and(|t| t.is_socket());
        if is_socket
            && entry.path().extension().is_some_and(|e| e == "sock")
            && fs::remove_file(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    Ok(removed)
}
