//! The one trusted way to create a directory that holds private service state.

use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::Path;

/// The effective user id, which owns every file and directory the service creates.
#[must_use]
pub fn service_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Creates `dir` (and any missing parents) with mode 0700, then verifies that it is a real
/// directory owned by the service user with no group or other permission bits. A directory
/// that already exists is verified the same way, never repaired.
///
/// # Errors
///
/// Returns the error from creating `dir` or reading its metadata, or `PermissionDenied` (with
/// the reason as its message) when it is a symbolic link or not a directory, belongs to another
/// user, or grants group or other permissions.
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let meta = fs::symlink_metadata(dir)?;
    let unsafe_dir = |why: &str| {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("unsafe private directory: {why}"),
        ))
    };
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return unsafe_dir("not a directory");
    }
    if meta.uid() != service_uid() {
        return unsafe_dir("not owned by the service user");
    }
    if meta.permissions().mode() & 0o077 != 0 {
        return unsafe_dir("group or other permissions are set");
    }
    Ok(())
}
