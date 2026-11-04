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

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;
    use crate::adapters_support::TempDir;

    fn refusal(dir: &Path) -> String {
        let error = ensure_private_dir(dir).expect_err("refused");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        error.to_string()
    }

    #[test]
    fn private_directories_are_created_with_owner_only_permissions() {
        let tmp = TempDir::new("private-dir");
        let dir = tmp.path().join("a/b/c");
        ensure_private_dir(&dir).expect("create");
        let meta = fs::metadata(&dir).expect("metadata");
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
        assert_eq!(meta.uid(), service_uid());
        ensure_private_dir(&dir).expect("an existing private directory is accepted");
        assert_eq!(service_uid(), rustix::process::geteuid().as_raw());
    }

    #[test]
    fn existing_directories_are_verified_never_repaired() {
        let tmp = TempDir::new("private-dir-refused");
        let open = tmp.path().join("open");
        fs::create_dir(&open).expect("dir");
        fs::set_permissions(&open, fs::Permissions::from_mode(0o750)).expect("chmod");
        assert!(refusal(&open).contains("group or other permissions"));
        assert_eq!(
            fs::metadata(&open).expect("metadata").permissions().mode() & 0o777,
            0o750,
            "the mode is left as it was"
        );

        let target = tmp.path().join("target");
        ensure_private_dir(&target).expect("target");
        let link = tmp.path().join("link");
        symlink(&target, &link).expect("symlink");
        assert!(refusal(&link).contains("not a directory"));

        let file = tmp.path().join("file");
        fs::write(&file, "x").expect("file");
        assert!(ensure_private_dir(&file).is_err());
    }
}
