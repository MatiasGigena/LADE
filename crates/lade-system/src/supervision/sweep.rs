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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PidRecord {
    pub pid: u32,
    pub start_time: u64,
}

impl PidRecord {
    #[must_use]
    pub fn of(pid: u32) -> Option<Self> {
        let (state, start_time) = proc_stat(pid)?;
        (state != 'Z' && state != 'X').then_some(Self { pid, start_time })
    }

    #[must_use]
    pub fn is_alive(&self) -> bool {
        Self::of(self.pid).is_some_and(|now| now == *self)
    }
}

fn proc_stat(pid: u32) -> Option<(char, u64)> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    let mut fields = rest.split_ascii_whitespace();
    let state = fields.next()?.chars().next()?;
    let start_time = fields.nth(18)?.parse().ok()?;
    Some((state, start_time))
}

pub(crate) fn parent_pid(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_ascii_whitespace().nth(1)?.parse().ok()
}

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

const INIT_DISCOVERY: Duration = Duration::from_secs(5);

pub(crate) fn record_sandbox(
    pidfile: &Path,
    pid: u32,
    leaf: Option<&CgroupLeaf>,
    ended: impl Fn() -> bool,
) -> io::Result<Option<PidRecord>> {
    let Some(outer) = PidRecord::of(pid) else {
        return Ok(None);
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

#[must_use]
pub fn sandbox_marker(run_dir: &Path) -> String {
    let canonical = fs::canonicalize(run_dir).unwrap_or_else(|_| run_dir.to_path_buf());
    let digest = Sha256::digest(canonical.as_os_str().as_encoded_bytes());
    let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("LADE_SANDBOX_{hex}")
}

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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub killed: Vec<u32>,
    pub skipped: Vec<u32>,
    pub removed_pidfiles: usize,
    pub removed_sockets: usize,
}

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
    for pid in kill_marked(&sandbox_marker(run_dir)) {
        report.killed.push(pid);
    }
    for path in &pidfiles {
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
