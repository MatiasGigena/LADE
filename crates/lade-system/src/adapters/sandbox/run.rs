use std::collections::VecDeque;
use std::io::{self, Read};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use super::{SandboxCommand, find_program};
use crate::support::process::wait_until;

const DRAIN_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunOutcome {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub output_tail: Vec<u8>,
    pub total_bytes: u64,
    pub truncated: bool,
    pub elapsed: Duration,
}

struct Tail {
    bytes: VecDeque<u8>,
    total: u64,
    cap: usize,
}

impl Tail {
    fn push(&mut self, chunk: &[u8]) {
        self.total += chunk.len() as u64;
        let chunk = &chunk[chunk.len().saturating_sub(self.cap)..];
        self.bytes.extend(chunk);
        let excess = self.bytes.len().saturating_sub(self.cap);
        self.bytes.drain(..excess);
    }
}

/// Runs `cmd` with its output merged and bounded to the last `max_output` bytes, killing
/// its process group after `timeout`.
///
/// # Errors
///
/// Fails as [`run_bounded_with`] does.
pub fn run_bounded(
    cmd: impl Into<SandboxCommand>,
    timeout: Duration,
    max_output: usize,
) -> io::Result<RunOutcome> {
    run_bounded_with(cmd, timeout, max_output, |_| Ok(()))
}

/// [`run_bounded`], calling `place` with the child's process-group id before its gate
/// opens, so the caller can place it in a cgroup first.
///
/// # Errors
///
/// Returns the operating system's error when the output pipe cannot be created, the command cannot
/// be spawned or waited on, or `place` or opening the gate fails (the child is killed then).
pub fn run_bounded_with(
    cmd: impl Into<SandboxCommand>,
    timeout: Duration,
    max_output: usize,
    place: impl FnOnce(u32) -> io::Result<()>,
) -> io::Result<RunOutcome> {
    let mut cmd = cmd.into();
    let (mut reader, writer) = io::pipe()?;
    let writer_err = writer.try_clone()?;
    cmd.command_mut()
        .stdin(Stdio::null())
        .stdout(writer)
        .stderr(writer_err)
        .process_group(0);
    let start = Instant::now();
    let (mut child, mut gate) = cmd.spawn()?;
    let pgid = child.id();
    if let Err(e) = place(pgid).and_then(|()| gate.open()) {
        kill_group(pgid);
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }
    let tail = Arc::new(Mutex::new(Tail {
        bytes: VecDeque::new(),
        total: 0,
        cap: max_output,
    }));
    let (done_tx, done_rx) = mpsc::channel();
    let sink = Arc::clone(&tail);
    thread::spawn(move || {
        let mut buf = vec![0u8; 64 << 10];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => sink
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = done_tx.send(());
    });
    let deadline = start + timeout;
    let mut timed_out = false;
    let status = match wait_until(&mut child, deadline)? {
        Some(status) => status,
        None => {
            timed_out = true;
            kill_group(pgid);
            let _ = child.kill();
            child.wait()?
        }
    };
    let elapsed = start.elapsed();
    if done_rx.recv_timeout(DRAIN_GRACE).is_err() {
        kill_group(pgid);
        let _ = done_rx.recv_timeout(DRAIN_GRACE);
    }
    let tail = tail.lock().unwrap_or_else(PoisonError::into_inner);
    Ok(RunOutcome {
        exit_code: status.code(),
        signal: status.signal(),
        timed_out,
        output_tail: tail.bytes.iter().copied().collect(),
        total_bytes: tail.total,
        truncated: tail.total > max_output as u64,
        elapsed,
    })
}

fn kill_group(pgid: u32) {
    if let Some(kill) = find_program("kill") {
        let _ = Command::new(kill)
            .env_clear()
            .args(["-s", "KILL", "--", &format!("-{pgid}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}
