use std::collections::VecDeque;
use std::fmt;
use std::fs::File;
use std::io::{self, ErrorKind, Read};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use rustix::pty::{OpenptFlags, grantpt, ioctl_tiocgptpeer, openpt, unlockpt};
use rustix::termios::{Winsize, tcsetwinsize};

use super::child::{SandboxedChild, lock};
use super::input::InputQueue;
use super::safe_id;
use super::sweep::sandbox_marker;
use crate::adapters::sandbox::{
    Limits, SandboxCgroups, SandboxTools, TerminalLaunch, terminal_spec,
};

pub const RING_BYTES: usize = 1 << 20;
pub const FULL_WAIT: Duration = Duration::from_millis(100);
pub const INPUT_QUEUE_BYTES: usize = 256 << 10;
const INPUT_QUEUE_FRAMES: usize = 256;
pub const MAX_COLS: u16 = 500;
pub const MAX_ROWS: u16 = 200;
/// How long a shell whose output closed may take to exit on its own before it is killed.
const EOF_GRACE: Duration = Duration::from_secs(10);
const SHELLS: &[&str] = &["/usr/bin/bash", "/bin/bash", "/usr/bin/sh", "/bin/sh"];
const SETSID: &[&str] = &["/usr/bin/setsid", "/bin/setsid"];

#[derive(Clone, Debug)]
pub struct TerminalRequest {
    pub objective_id: String,
    pub terminal_id: String,
    pub worktree: PathBuf,
    pub repo_common_dir: PathBuf,
    pub writable: bool,
    pub cols: u16,
    pub rows: u16,
    pub run_dir: PathBuf,
    pub limits: Limits,
    pub cgroups: Option<Arc<SandboxCgroups>>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Drained {
    pub bytes: Vec<u8>,
    pub dropped: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalExit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

#[derive(Default)]
struct Ring {
    bytes: VecDeque<u8>,
    dropped: u64,
    eof: bool,
}

struct Shared {
    child: SandboxedChild,
    ring: Mutex<Ring>,
    space: Condvar,
}

impl Shared {
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

pub struct Terminal {
    shared: Arc<Shared>,
    controller: File,
    input: Option<InputQueue>,
    killed: bool,
}

impl fmt::Debug for Terminal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Terminal")
            .field("pidfile", &self.shared.child.pidfile())
            .finish_non_exhaustive()
    }
}

impl Terminal {
    /// Opens a sandboxed interactive shell on a pseudo-terminal, as `request` describes.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for an unsafe identity, size or missing shell, or the operating system's
    /// error when the pseudo-terminal, the sandbox or its I/O threads cannot be set up.
    pub fn open(tools: &SandboxTools, request: &TerminalRequest) -> io::Result<Self> {
        if !safe_id(&request.objective_id) || !safe_id(&request.terminal_id) {
            return Err(invalid("terminal identity"));
        }
        let size = winsize(request.cols, request.rows)?;
        let shell =
            first_existing(SHELLS).ok_or_else(|| invalid("no shell in /usr/bin or /bin"))?;
        let mut spec = terminal_spec(&TerminalLaunch {
            shell,
            setsid: first_existing(SETSID),
            worktree: request.worktree.clone(),
            repo_common_dir: request.repo_common_dir.clone(),
            writable: request.writable,
            limits: request.limits,
        })?;
        spec.marker(&sandbox_marker(&request.run_dir))?;
        let leaf = match &request.cgroups {
            Some(cgroups) => Some(cgroups.leaf(
                &request.objective_id,
                &format!("terminal-{}", request.terminal_id),
            )?),
            None => None,
        };

        let flags = OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC;
        let controller = openpt(flags)?;
        grantpt(&controller)?;
        unlockpt(&controller)?;
        tcsetwinsize(&controller, size)?;
        let user = ioctl_tiocgptpeer(&controller, flags)?;
        let mut cmd = spec.command(tools)?;
        cmd.command_mut()
            .stdin(Stdio::from(user.try_clone()?))
            .stdout(Stdio::from(user.try_clone()?))
            .stderr(Stdio::from(user))
            .process_group(0);
        let pidfile = request.run_dir.join(format!(
            "terminal-{}-{}.pid",
            request.objective_id, request.terminal_id
        ));
        let (child, _) = SandboxedChild::spawn(cmd, pidfile, leaf)?;
        let shared = Arc::new(Shared {
            child,
            ring: Mutex::new(Ring::default()),
            space: Condvar::new(),
        });
        let controller = File::from(controller);
        let mut terminal = Self {
            shared: Arc::clone(&shared),
            controller: controller.try_clone()?,
            input: None,
            killed: false,
        };
        let reader = controller.try_clone()?;
        let reader_shared = Arc::clone(&shared);
        if let Err(e) = thread::Builder::new()
            .name("lade-terminal-read".into())
            .spawn(move || read_output(&reader_shared, reader))
        {
            terminal.kill();
            return Err(e);
        }
        let input = InputQueue::start(
            "lade-terminal-write",
            controller,
            INPUT_QUEUE_FRAMES,
            INPUT_QUEUE_BYTES,
            "terminal_input_full",
        );
        let input = match input {
            Ok(input) => input,
            Err(e) => {
                terminal.kill();
                return Err(e);
            }
        };
        terminal.input = Some(input);
        Ok(terminal)
    }

    /// Queues `data` as terminal input.
    ///
    /// # Errors
    ///
    /// Returns `BrokenPipe` when the terminal was closed or exited, or `WouldBlock` when its input
    /// queue is full.
    pub fn write(&self, data: &[u8]) -> io::Result<()> {
        let gone = || io::Error::new(ErrorKind::BrokenPipe, "terminal_closed");
        let Some(input) = self.input.as_ref() else {
            return Err(gone());
        };
        if self.killed || input.failed() || self.exit().is_some() {
            return Err(gone());
        }
        input.push(data.to_vec(), gone)
    }

    /// Sets the terminal's size.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for a zero or oversized size, or the error from setting the window size.
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        tcsetwinsize(&self.controller, winsize(cols, rows)?).map_err(io::Error::from)
    }

    #[must_use]
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

    #[must_use]
    pub fn pending(&self) -> bool {
        let ring = lock(&self.shared.ring);
        !ring.bytes.is_empty() || ring.dropped > 0
    }

    #[must_use]
    pub fn exit(&self) -> Option<TerminalExit> {
        let status = self.shared.child.poll_exit()?;
        Some(TerminalExit {
            code: status.code(),
            signal: status.signal(),
        })
    }

    #[must_use]
    pub fn finished(&self) -> Option<TerminalExit> {
        let eof = lock(&self.shared.ring).eof;
        let exit = self.exit()?;
        (eof && !self.pending()).then_some(exit)
    }

    pub fn kill(&mut self) {
        if self.killed {
            return;
        }
        self.killed = true;
        self.input = None;
        self.shared.child.terminate();
        self.shared.space.notify_all();
    }

    #[must_use]
    pub fn pidfile(&self) -> &Path {
        self.shared.child.pidfile()
    }

    #[must_use]
    pub fn namespace_init(&self) -> Option<u32> {
        self.shared.child.namespace_init()
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.kill();
    }
}

fn read_output(shared: &Shared, mut controller: File) {
    let mut buffer = vec![0u8; 16 << 10];
    loop {
        match controller.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => shared.push(&buffer[..n]),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    drop(controller);
    lock(&shared.ring).eof = true;
    shared.child.wait_exit(EOF_GRACE);
    shared.child.terminate();
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
