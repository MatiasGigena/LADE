//! Contained terminals (IMP-012): an interactive shell on a service-owned PTY, inside the same
//! bubblewrap sandbox and `prlimit` quotas as the worker.
//!
//! The service allocates the PTY (`posix_openpt` through rustix, the user side opened with
//! `TIOCGPTPEER`, both close-on-exec and without becoming the service's controlling terminal),
//! hands the user side to `prlimit … bwrap … setsid --ctty --wait <shell> -i` as its standard
//! streams and keeps the controller side. Nothing else of the service is inherited. Every process
//! of the terminal — including `setsid`/double-forked daemons — lives in the sandbox's PID
//! namespace, so killing the namespace init ends all of them, exactly as for workers.
//!
//! Threads per terminal: a reader (controller side → bounded ring, [`RING_BYTES`]) that observes
//! the exit once the user side is closed everywhere, and a writer (bounded input queue →
//! controller side). Output is never copied anywhere else (no diagnostic copy): the service drains
//! the ring on its tick. When the ring is full the reader waits up to [`FULL_WAIT`] for the
//! service to drain it (backpressure on the shell), then discards the oldest bytes and counts
//! them, so a flood costs bounded memory and the client sees a marked gap.

use std::collections::VecDeque;
use std::fmt;
use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use rustix::pty::{OpenptFlags, grantpt, ioctl_tiocgptpeer, openpt, unlockpt};
use rustix::termios::{Winsize, tcsetwinsize};

use super::sweep::{PidRecord, children_of, kill_record, write_pidfile};
use crate::adapters::sandbox::{Limits, SandboxTools, TerminalLaunch, terminal_spec};

/// Output retained per terminal between service drains.
pub const RING_BYTES: usize = 1 << 20;
/// How long the reader waits for the service to drain a full ring before discarding output.
pub const FULL_WAIT: Duration = Duration::from_millis(100);
/// Queued input bytes per terminal; beyond this `write` fails with `WouldBlock`.
pub const INPUT_QUEUE_BYTES: usize = 256 << 10;
const INPUT_QUEUE_FRAMES: usize = 256;
/// Accepted terminal sizes.
pub const MAX_COLS: u16 = 500;
pub const MAX_ROWS: u16 = 200;
/// Terminal processes get the worker's limits (§7 defaults: 4 GiB address space, 512 processes,
/// 1024 files, 512 MiB file size). A shell does not need Node's 8 GiB reservation.
pub const TERMINAL_LIMITS: Limits = Limits::WORKER;
const KILL_WAIT: Duration = Duration::from_secs(10);
const INIT_DISCOVERY: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(20);
const SHELLS: &[&str] = &["/usr/bin/bash", "/bin/bash", "/usr/bin/sh", "/bin/sh"];
const SETSID: &[&str] = &["/usr/bin/setsid", "/bin/setsid"];

/// One terminal to open.
#[derive(Clone, Debug)]
pub struct TerminalRequest {
    pub objective_id: String,
    pub terminal_id: String,
    pub worktree: PathBuf,
    pub repo_common_dir: PathBuf,
    /// `false` binds the worktree read-only (review writer exclusion).
    pub writable: bool,
    pub cols: u16,
    pub rows: u16,
    /// Private directory for the pidfile the startup sweep reads (`$STATE/run`).
    pub run_dir: PathBuf,
    pub limits: Limits,
}

/// Output taken from a terminal's ring.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Drained {
    pub bytes: Vec<u8>,
    /// Bytes discarded since the previous drain because the ring overflowed.
    pub dropped: u64,
}

/// How the terminal's sandbox ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalExit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

#[derive(Default)]
struct Ring {
    bytes: VecDeque<u8>,
    dropped: u64,
    /// The controller side reported end of file / `EIO` (every user side is closed).
    eof: bool,
}

struct Shared {
    child: Mutex<Child>,
    status: Mutex<Option<ExitStatus>>,
    init: Mutex<Option<PidRecord>>,
    ring: Mutex<Ring>,
    space: Condvar,
    pidfile: PathBuf,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    fn poll_exit(&self) -> Option<ExitStatus> {
        if let Some(status) = *lock(&self.status) {
            return Some(status);
        }
        let status = lock(&self.child).try_wait().ok().flatten()?;
        *lock(&self.status) = Some(status);
        Some(status)
    }

    fn wait_exit(&self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.poll_exit() {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(POLL);
        }
    }

    /// `SIGKILL` the namespace init (ending every process of the terminal) and bwrap, holding
    /// the child lock so an already-reaped pid is never signalled.
    fn kill(&self) {
        let mut child = lock(&self.child);
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        if let Some(init) = *lock(&self.init) {
            kill_record(&init);
        }
        let _ = child.kill();
    }

    fn record_pids(&self, pid: u32) -> io::Result<()> {
        let Some(outer) = PidRecord::of(pid) else {
            return Ok(());
        };
        write_pidfile(&self.pidfile, &[outer])?;
        let deadline = Instant::now() + INIT_DISCOVERY;
        while Instant::now() < deadline && self.poll_exit().is_none() {
            if let Some(init) = children_of(pid).into_iter().find_map(PidRecord::of) {
                *lock(&self.init) = Some(init);
                return write_pidfile(&self.pidfile, &[outer, init]);
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    /// Appends output, waiting briefly for room, then discarding the oldest bytes.
    fn push(&self, data: &[u8]) {
        let mut ring = lock(&self.ring);
        let deadline = Instant::now() + FULL_WAIT;
        while ring.bytes.len() + data.len() > RING_BYTES {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            ring = self
                .space
                .wait_timeout(ring, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        let data = &data[data.len().saturating_sub(RING_BYTES)..];
        let excess = (ring.bytes.len() + data.len()).saturating_sub(RING_BYTES);
        if excess > 0 {
            ring.bytes.drain(..excess);
            ring.dropped += excess as u64;
        }
        ring.bytes.extend(data);
    }
}

fn first_existing(candidates: &[&str]) -> Option<PathBuf> {
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

fn safe_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn invalid(what: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, what)
}

fn winsize(cols: u16, rows: u16) -> io::Result<Winsize> {
    if !(1..=MAX_COLS).contains(&cols) || !(1..=MAX_ROWS).contains(&rows) {
        return Err(invalid("terminal size out of range"));
    }
    Ok(Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    })
}

/// A running contained terminal. Dropping it kills the sandbox.
pub struct Terminal {
    shared: Arc<Shared>,
    controller: File,
    input: Option<SyncSender<Vec<u8>>>,
    queued: Arc<AtomicUsize>,
    link_failed: Arc<AtomicBool>,
    killed: bool,
}

impl fmt::Debug for Terminal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Terminal")
            .field("pidfile", &self.shared.pidfile)
            .finish_non_exhaustive()
    }
}

impl Terminal {
    /// Allocates the PTY and starts the shell in its sandbox. There is no unsandboxed path: the
    /// caller passes tools whose sandbox probe succeeded.
    pub fn open(tools: &SandboxTools, request: &TerminalRequest) -> io::Result<Self> {
        if !safe_id(&request.objective_id) || !safe_id(&request.terminal_id) {
            return Err(invalid("terminal identity"));
        }
        let size = winsize(request.cols, request.rows)?;
        let shell =
            first_existing(SHELLS).ok_or_else(|| invalid("no shell in /usr/bin or /bin"))?;
        let spec = terminal_spec(&TerminalLaunch {
            shell,
            setsid: first_existing(SETSID),
            worktree: request.worktree.clone(),
            repo_common_dir: request.repo_common_dir.clone(),
            writable: request.writable,
            limits: request.limits,
        })
        .map_err(|e| io::Error::new(ErrorKind::InvalidInput, e.to_string()))?;

        let flags = OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC;
        let controller = openpt(flags)?;
        grantpt(&controller)?;
        unlockpt(&controller)?;
        tcsetwinsize(&controller, size)?;
        let user = ioctl_tiocgptpeer(&controller, flags)?;
        let mut cmd = spec.command(tools);
        cmd.stdin(Stdio::from(user.try_clone()?))
            .stdout(Stdio::from(user.try_clone()?))
            .stderr(Stdio::from(user))
            .process_group(0);
        let spawned = cmd.spawn();
        // The command holds this process's only other copies of the user side.
        drop(cmd);
        let child = spawned?;
        let pid = child.id();
        let pidfile = request.run_dir.join(format!(
            "terminal-{}-{}.pid",
            request.objective_id, request.terminal_id
        ));
        let shared = Arc::new(Shared {
            child: Mutex::new(child),
            status: Mutex::new(None),
            init: Mutex::new(None),
            ring: Mutex::new(Ring::default()),
            space: Condvar::new(),
            pidfile,
        });
        let controller = File::from(controller);
        let mut terminal = Self {
            shared: Arc::clone(&shared),
            controller: controller.try_clone()?,
            input: None,
            queued: Arc::new(AtomicUsize::new(0)),
            link_failed: Arc::new(AtomicBool::new(false)),
            killed: false,
        };
        if let Err(e) = shared.record_pids(pid) {
            terminal.kill();
            return Err(e);
        }
        let reader = controller.try_clone()?;
        let reader_shared = Arc::clone(&shared);
        if let Err(e) = thread::Builder::new()
            .name("lade-terminal-read".into())
            .spawn(move || read_output(&reader_shared, reader))
        {
            terminal.kill();
            return Err(e);
        }
        let (tx, rx) = sync_channel::<Vec<u8>>(INPUT_QUEUE_FRAMES);
        let queued = Arc::clone(&terminal.queued);
        let failed = Arc::clone(&terminal.link_failed);
        let mut writer = controller;
        let spawned = thread::Builder::new()
            .name("lade-terminal-write".into())
            .spawn(move || {
                for chunk in rx {
                    let written = writer.write_all(&chunk).and_then(|()| writer.flush());
                    queued.fetch_sub(chunk.len(), Ordering::SeqCst);
                    if written.is_err() {
                        failed.store(true, Ordering::SeqCst);
                        break;
                    }
                }
            });
        if let Err(e) = spawned {
            terminal.kill();
            return Err(e);
        }
        terminal.input = Some(tx);
        Ok(terminal)
    }

    /// Queues input for the shell (never blocks the caller). `WouldBlock` when the bounded queue
    /// is full, `BrokenPipe` when the terminal is gone.
    pub fn write(&self, data: &[u8]) -> io::Result<()> {
        let gone = || io::Error::new(ErrorKind::BrokenPipe, "terminal_closed");
        let Some(tx) = self.input.as_ref() else {
            return Err(gone());
        };
        if self.killed || self.link_failed.load(Ordering::SeqCst) || self.exit().is_some() {
            return Err(gone());
        }
        let len = data.len();
        let full = || io::Error::new(ErrorKind::WouldBlock, "terminal_input_full");
        if self.queued.fetch_add(len, Ordering::SeqCst) + len > INPUT_QUEUE_BYTES {
            self.queued.fetch_sub(len, Ordering::SeqCst);
            return Err(full());
        }
        match tx.try_send(data.to_vec()) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.queued.fetch_sub(len, Ordering::SeqCst);
                Err(full())
            }
            Err(TrySendError::Disconnected(_)) => {
                self.queued.fetch_sub(len, Ordering::SeqCst);
                Err(gone())
            }
        }
    }

    /// Sets the PTY window size; the kernel signals `SIGWINCH` to the foreground job.
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        tcsetwinsize(&self.controller, winsize(cols, rows)?).map_err(io::Error::from)
    }

    /// Takes up to `max` bytes of buffered output and the count of discarded bytes.
    pub fn drain(&self, max: usize) -> Drained {
        let mut ring = lock(&self.shared.ring);
        let n = ring.bytes.len().min(max);
        let drained = Drained {
            bytes: ring.bytes.drain(..n).collect(),
            dropped: std::mem::take(&mut ring.dropped),
        };
        drop(ring);
        self.shared.space.notify_all();
        drained
    }

    /// Whether output is waiting.
    pub fn pending(&self) -> bool {
        let ring = lock(&self.shared.ring);
        !ring.bytes.is_empty() || ring.dropped > 0
    }

    /// The exit, once the sandbox ended and every user side is closed.
    pub fn exit(&self) -> Option<TerminalExit> {
        let status = self.shared.poll_exit()?;
        Some(TerminalExit {
            code: status.code(),
            signal: status.signal(),
        })
    }

    /// Whether the terminal ended and all its output was drained.
    pub fn finished(&self) -> Option<TerminalExit> {
        let eof = lock(&self.shared.ring).eof;
        let exit = self.exit()?;
        (eof && !self.pending()).then_some(exit)
    }

    /// Kills every process of the terminal (PID-namespace init, then bwrap) and waits for the
    /// sandbox to end; the pidfile is removed once it has.
    pub fn kill(&mut self) {
        if self.killed {
            return;
        }
        self.killed = true;
        self.input = None;
        self.shared.kill();
        loop {
            if self.shared.wait_exit(KILL_WAIT).is_some() {
                let _ = std::fs::remove_file(&self.shared.pidfile);
                break;
            }
            self.shared.kill();
        }
        // Unblock a reader waiting for room.
        self.shared.space.notify_all();
    }

    /// The pidfile recording this terminal's sandbox (removed once it has ended).
    pub fn pidfile(&self) -> &Path {
        &self.shared.pidfile
    }

    /// The PID-namespace init of the sandbox, once discovered.
    pub fn namespace_init(&self) -> Option<u32> {
        lock(&self.shared.init).map(|record| record.pid)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Reader thread: controller side → ring until `EIO`/EOF, then wait for the sandbox to end.
fn read_output(shared: &Shared, mut controller: File) {
    let mut buffer = vec![0u8; 16 << 10];
    loop {
        match controller.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => shared.push(&buffer[..n]),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            // `EIO` once the last user side is closed.
            Err(_) => break,
        }
    }
    drop(controller);
    lock(&shared.ring).eof = true;
    // Every process holding the user side is gone; the sandbox ends right after.
    if shared.wait_exit(KILL_WAIT).is_none() {
        shared.kill();
    }
    if shared.wait_exit(KILL_WAIT).is_some() {
        let _ = std::fs::remove_file(&shared.pidfile);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_bounded() {
        assert!(winsize(80, 24).is_ok());
        assert!(winsize(0, 24).is_err());
        assert!(winsize(80, 0).is_err());
        assert!(winsize(MAX_COLS + 1, 24).is_err());
        assert!(winsize(80, MAX_ROWS + 1).is_err());
    }
}
