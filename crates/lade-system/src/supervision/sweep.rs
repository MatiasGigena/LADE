//! Pidfiles for launched sandboxes and the startup sweep of stale ones.
//!
//! Each launch records `worker-<objective>-<generation>.pid` (a contained terminal:
//! `terminal-<objective>-<terminal>.pid`) in the run directory: one line
//! `<pid> <starttime>` per process to kill (the outer bwrap, then the sandbox's PID-namespace
//! init once known). `starttime` is field 22 of `/proc/<pid>/stat` (clock ticks since boot), so
//! a recycled pid never matches. The service calls [`sweep_stale`] at startup, before any launch,
//! to kill sandboxes that survived a previous service (for example the `--die-with-parent` race
//! during the first milliseconds of a launch) and to remove their gateway sockets and pidfiles.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::adapters::sandbox::find_program;

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
