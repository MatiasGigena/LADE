use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, ExitStatus};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use super::sweep::{PidRecord, kill_record, record_sandbox};
use crate::adapters::sandbox::{CgroupLeaf, SandboxCommand};

/// How long one kill waits for the sandbox to be reaped.
const KILL_WAIT: Duration = Duration::from_secs(10);
/// How many kills teardown sends before it gives up waiting.
const KILL_ATTEMPTS: u32 = 3;
/// Upper bound between two exit polls while waiting.
const POLL: Duration = Duration::from_millis(10);

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The piped standard streams of a sandboxed child, taken before its gate opens.
pub(crate) struct ChildStdio {
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
}

/// A running sandbox (worker or terminal): its outer process, the recorded namespace init, its
/// cgroup leaf and its pidfile.
///
/// Teardown policy, shared by every owner: kill the namespace init and the outer process, wait up
/// to [`KILL_WAIT`] for the reap, and retry up to [`KILL_ATTEMPTS`] times. The cgroup leaf is then
/// released (which kills whatever is still in it). The pidfile is removed once the child is
/// reaped; when it is not, the pidfile stays for the startup sweep and the loss is logged. Dropping
/// the last handle tears the sandbox down the same way.
pub(crate) struct SandboxedChild {
    child: Mutex<Child>,
    status: Mutex<Option<ExitStatus>>,
    exited: Condvar,
    init: Mutex<Option<PidRecord>>,
    leaf: Mutex<Option<CgroupLeaf>>,
    pidfile: PathBuf,
}

impl SandboxedChild {
    /// Spawns the sandbox, records it in `pidfile`, places it in `leaf` and opens its gate, so the
    /// sandboxed program runs only once the sandbox can be swept and limited. On failure the
    /// sandbox is torn down before the error is returned.
    pub(crate) fn spawn(
        command: SandboxCommand,
        pidfile: PathBuf,
        leaf: Option<CgroupLeaf>,
    ) -> io::Result<(Self, ChildStdio)> {
        let (mut child, mut gate) = command.spawn()?;
        let stdio = ChildStdio {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
        };
        let pid = child.id();
        let this = Self {
            child: Mutex::new(child),
            status: Mutex::new(None),
            exited: Condvar::new(),
            init: Mutex::new(None),
            leaf: Mutex::new(None),
            pidfile,
        };
        let started = record_sandbox(&this.pidfile, pid, leaf.as_ref(), || {
            this.poll_exit().is_some()
        })
        .and_then(|init| {
            *lock(&this.init) = init;
            *lock(&this.leaf) = leaf;
            gate.open()
        });
        // On failure `this` is dropped here, which tears the sandbox down.
        started.map(|()| (this, stdio))
    }

    pub(crate) fn pidfile(&self) -> &Path {
        &self.pidfile
    }

    pub(crate) fn namespace_init(&self) -> Option<u32> {
        lock(&self.init).map(|record| record.pid)
    }

    pub(crate) fn poll_exit(&self) -> Option<ExitStatus> {
        if let Some(status) = *lock(&self.status) {
            return Some(status);
        }
        let status = lock(&self.child).try_wait().ok().flatten()?;
        *lock(&self.status) = Some(status);
        self.exited.notify_all();
        Some(status)
    }

    pub(crate) fn wait_exit(&self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.poll_exit() {
                return Some(status);
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let guard = lock(&self.status);
            let _ = self
                .exited
                .wait_timeout(guard, POLL.min(deadline - now))
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    pub(crate) fn kill(&self) {
        let mut child = lock(&self.child);
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        if let Some(init) = *lock(&self.init) {
            kill_record(&init);
        }
        let _ = child.kill();
    }

    /// Kills the sandbox and releases its cgroup leaf and pidfile (see the type's teardown
    /// policy). Idempotent; returns the exit status when the child was reaped.
    pub(crate) fn terminate(&self) -> Option<ExitStatus> {
        let mut status = self.poll_exit();
        for _ in 0..KILL_ATTEMPTS {
            if status.is_some() {
                break;
            }
            self.kill();
            status = self.wait_exit(KILL_WAIT);
        }
        if let Some(leaf) = lock(&self.leaf).take()
            && leaf.oom_kills() > 0
        {
            eprintln!(
                "laded: sandbox memory limit reached ({} OOM kill(s)) cgroup={}",
                leaf.oom_kills(),
                leaf.path().display()
            );
        }
        if status.is_some() {
            let _ = std::fs::remove_file(&self.pidfile);
        } else {
            eprintln!(
                "laded: sandbox not reaped after {KILL_ATTEMPTS} kills; {} is left for the startup sweep",
                self.pidfile.display()
            );
        }
        status
    }
}

impl Drop for SandboxedChild {
    fn drop(&mut self) {
        self.terminate();
    }
}
