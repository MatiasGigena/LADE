//! Wall-clock bounded execution with bounded combined output capture.

use std::collections::VecDeque;
use std::io::{self, Read};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use super::find_program;

const POLL: Duration = Duration::from_millis(10);
/// How long to wait for the output pipe to close after the command exited.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Result of [`run_bounded`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunOutcome {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    /// The last `max_output` bytes of combined stdout+stderr.
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

/// Run `cmd` with stdin closed and stdout+stderr captured into one pipe, retaining the last
/// `max_output` bytes. The command runs in its own process group. On timeout the whole group is
/// sent `SIGKILL` and the direct child is killed; for a bwrap sandbox, killing bwrap tears down
/// its PID namespace (`--die-with-parent`), which kills every descendant, including ones that
/// called `setsid`. If descendants outside a PID namespace keep the output pipe open after the
/// command exits, their group is killed too and capture stops after a short grace period.
pub fn run_bounded(
    mut cmd: Command,
    timeout: Duration,
    max_output: usize,
) -> io::Result<RunOutcome> {
    let (mut reader, writer) = io::pipe()?;
    let writer_err = writer.try_clone()?;
    cmd.stdin(Stdio::null())
        .stdout(writer)
        .stderr(writer_err)
        .process_group(0);
    let start = Instant::now();
    let mut child = cmd.spawn()?;
    // Drop the parent's copies of the write end so EOF arrives when the children close theirs.
    drop(cmd);
    let pgid = child.id();
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
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            kill_group(pgid);
            let _ = child.kill();
            break child.wait()?;
        }
        thread::sleep(POLL);
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

/// Best-effort `SIGKILL` to a process group through the external `kill` utility (no `unsafe`).
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
