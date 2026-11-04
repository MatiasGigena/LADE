//! Waiting on a child process with a deadline.

use std::io;
use std::process::{Child, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

/// How often a deadline wait re-checks a child that has not exited yet. `std` has no timed
/// wait on a [`Child`], so the wait polls `try_wait` at this interval.
pub const EXIT_POLL: Duration = Duration::from_millis(10);

/// Waits for `child` to exit until `deadline`, returning `Ok(None)` if it is still running
/// then. The child is left as it is either way; the caller decides how to stop it.
///
/// # Errors
///
/// Returns the error from `try_wait` when the child's status cannot be queried.
pub fn wait_until(child: &mut Child, deadline: Instant) -> io::Result<Option<ExitStatus>> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        thread::sleep(EXIT_POLL.min(deadline - now));
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::time::Duration;

    use super::*;

    #[test]
    fn returns_the_status_of_a_child_that_exits_in_time() {
        let mut child = Command::new("true").spawn().unwrap();
        let status = wait_until(&mut child, Instant::now() + Duration::from_secs(10)).unwrap();
        assert!(status.is_some_and(|s| s.success()));
    }

    #[test]
    fn returns_none_for_a_child_still_running_at_the_deadline() {
        let mut child = Command::new("sleep").arg("10").spawn().unwrap();
        let started = Instant::now();
        let status = wait_until(&mut child, started + Duration::from_millis(50)).unwrap();
        assert!(status.is_none());
        assert!(started.elapsed() < Duration::from_secs(5));
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
