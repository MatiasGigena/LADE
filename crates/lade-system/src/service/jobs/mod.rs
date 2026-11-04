//! Work that runs off the service thread: Git capture, freeze, refresh, verification,
//! launch, cleanup and file reads. Each entry point takes an owned input and returns a value the
//! service applies on its own thread.
//!
//! Most entry points return `Result<_, Rejection>` because a failure simply rejects the
//! request. `verify` and `launch` instead report failure inside `VerifyOut` and `LaunchOut`:
//! their outcome is recorded either way (a failed check is a verification result, and a
//! launch that provisioned a worktree must persist it even if the worker did not start).

mod candidate;
mod closing;
mod launch;
mod pool;
mod read;
mod verify;

pub use candidate::{
    CaptureInput, CaptureOut, FreezeInput, FreezeOut, LatestCapture, RefreshInput, RefreshOut,
    capture, filtered_paths, freeze, refresh,
};
pub use closing::{change_set_empty, cleanup, delete_refs};
pub use launch::{LaunchInput, LaunchOut, ProfileSnapshot, Provision, launch};
pub(crate) use pool::{Pools, Task};
pub use read::{
    FileBytes, LargeBlobs, counts, open_repository, read_file_bytes, read_whole, target_commit,
};
pub use verify::{VerifyInput, VerifyOut, disk_usage, verify};
