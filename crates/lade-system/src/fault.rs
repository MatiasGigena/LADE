//! Test-only fault injection. **Debug builds only**: release builds compile every hook here to
//! nothing, so neither variable has any effect on an installed `laded`. Two mechanisms, each for
//! one kind of test:
//!
//! - **Crash points** (IMP-013, A09/A10 recovery tests): `LADE_FAULT_POINT=<name>` makes [`point`]
//!   abort the whole process the first time execution reaches `name`.
//! - **Operation faults** (native GUI evidence, `scripts/e2e-resilience`): `LADE_TEST_FAULTS_DIR`
//!   names a directory whose files make an operation fail or slow down while the service keeps
//!   running ([`operation`]).
//!
//! ## Crash points
//!
//! No destructor, flush or shutdown path runs, exactly as if the service had been killed between
//! two journaled steps. Points (each sits between two effects whose order recovery must
//! tolerate):
//!
//! | Point | Window |
//! | --- | --- |
//! | `input.accepted` | accepted input + outcome committed, reply and dispatch not yet sent |
//! | `dispatch.recorded` | `dispatched` committed, frame not yet written to the worker |
//! | `launch.provisioned` | worktree and branch created, not yet recorded on the Objective |
//! | `capture.retained` | capture retention ref written, capture row not yet recorded |
//! | `freeze.branch_moved` | Objective branch CAS-moved to the candidate commit, candidate not recorded |
//! | `freeze.recorded` | candidate committed with the outcome, reply not yet sent |
//! | `verify.copy` | verification copy and branch created, check not yet run |
//! | `integrate.intent` | integration intent journaled, target untouched |
//! | `integrate.ref_moved` | target ref CAS-moved, checked-out target not yet updated (`read-tree`) |
//! | `integrate.applied` | Git integration complete, outcome not yet recorded |
//! | `cleanup.removed` | worktree and branch removed, cleanup not yet recorded |
//!
//! ## Operation faults
//!
//! The service logs a warning at start when [`FAULTS_ENV`] names an absolute directory. While that
//! directory holds a file named after an operation, the operation misbehaves; the file is read at
//! every such request, so a test can create and remove it while the service runs:
//!
//! | File | Content | Effect |
//! | --- | --- | --- |
//! | `draft.save` | empty (or anything else) | `draft.save` is refused with `durability_failure` before anything is written |
//! | `draft.save` | `delay <ms>` | `draft.save` waits `<ms>` (≤ 10 s) on the service thread, then proceeds normally |

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Environment variable naming the operation-fault directory (test-only, debug builds).
pub const FAULTS_ENV: &str = "LADE_TEST_FAULTS_DIR";

const MAX_DELAY: Duration = Duration::from_secs(10);

/// Aborts the process when `LADE_FAULT_POINT` names `name` (debug builds only).
#[cfg(debug_assertions)]
pub fn point(name: &str) {
    use std::sync::OnceLock;
    static ARMED: OnceLock<Option<String>> = OnceLock::new();
    let armed = ARMED.get_or_init(|| std::env::var("LADE_FAULT_POINT").ok());
    if armed.as_deref() == Some(name) {
        eprintln!("laded: fault injection: aborting at {name}");
        std::process::abort();
    }
}

/// Release builds: no fault injection.
#[cfg(not(debug_assertions))]
#[inline(always)]
pub fn point(_name: &str) {}

/// What an injected operation fault asks the operation to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    Refuse,
    Delay(Duration),
}

/// The operation-fault directory from the environment, read once (debug builds only).
#[cfg(debug_assertions)]
pub fn directory() -> Option<&'static Path> {
    use std::sync::OnceLock;
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        std::env::var_os(FAULTS_ENV)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    })
    .as_deref()
}

/// Release builds: no fault injection.
#[cfg(not(debug_assertions))]
#[inline(always)]
pub fn directory() -> Option<&'static Path> {
    None
}

/// The fault currently injected for `operation` under `dir`, if any.
pub fn fault_in(dir: &Path, operation: &str) -> Option<Fault> {
    let text = std::fs::read_to_string(dir.join(operation)).ok()?;
    let delay = text
        .trim()
        .strip_prefix("delay ")
        .and_then(|ms| ms.trim().parse::<u64>().ok());
    Some(match delay {
        Some(ms) => Fault::Delay(Duration::from_millis(ms).min(MAX_DELAY)),
        None => Fault::Refuse,
    })
}

/// The fault injected for `operation`, when operation faults are enabled.
pub fn operation(operation: &str) -> Option<Fault> {
    fault_in(directory()?, operation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_faults_follow_the_files_in_the_directory() {
        let dir = std::env::temp_dir().join(format!("lade-faults-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(fault_in(&dir, "draft.save"), None);
        std::fs::write(dir.join("draft.save"), "").unwrap();
        assert_eq!(fault_in(&dir, "draft.save"), Some(Fault::Refuse));
        std::fs::write(dir.join("draft.save"), "delay 250\n").unwrap();
        assert_eq!(
            fault_in(&dir, "draft.save"),
            Some(Fault::Delay(Duration::from_millis(250)))
        );
        std::fs::write(dir.join("draft.save"), "delay 999999").unwrap();
        assert_eq!(fault_in(&dir, "draft.save"), Some(Fault::Delay(MAX_DELAY)));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(fault_in(&dir, "draft.save"), None);
    }
}
