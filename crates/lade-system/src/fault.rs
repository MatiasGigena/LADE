//! Crash-window fault injection for recovery tests (IMP-013, A09/A10).
//!
//! In debug builds, `LADE_FAULT_POINT=<name>` makes [`point`] abort the whole process the first
//! time execution reaches `name`: no destructor, flush or shutdown path runs, exactly as if the
//! service had been killed between two journaled steps. Release builds compile [`point`] to
//! nothing, so the variable has no effect on an installed `laded`.
//!
//! Points (each sits between two effects whose order recovery must tolerate):
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
