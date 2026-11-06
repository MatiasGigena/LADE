//! Copying a profile: the bounded copy of approved resources into a session snapshot, and the
//! copy-only import from another pi profile.

use std::fs;
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::support::fs::ensure_private_dir;

use super::models::read_models;
use super::{
    Kind, MAX_COPY_BYTES, MAX_COPY_FILES, MAX_DEPTH, extension_hash, name_of, scan, sorted_entries,
};

pub(super) struct Budget {
    files: usize,
    bytes: u64,
}

pub(super) fn copy_file(from: &Path, to: &Path, budget: &mut Budget) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    budget.files += 1;
    budget.bytes += meta.len();
    if budget.files > MAX_COPY_FILES || budget.bytes > MAX_COPY_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            "profile too large",
        ));
    }
    let bytes = fs::read(from)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(to)?;
    io::Write::write_all(&mut file, &bytes)
}

pub(super) fn copy_tree(
    from: &Path,
    to: &Path,
    budget: &mut Budget,
    depth: usize,
) -> io::Result<()> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    ensure_private_dir(to)?;
    for path in sorted_entries(from) {
        let Some(name) = path.file_name() else {
            continue;
        };
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            copy_tree(&path, &to.join(name), budget, depth + 1)?;
        } else if meta.is_file() {
            copy_file(&path, &to.join(name), budget)?;
        }
    }
    Ok(())
}

pub(super) fn copy_entry(from: &Path, to: &Path, budget: &mut Budget) -> io::Result<()> {
    if fs::symlink_metadata(from)?.is_dir() {
        copy_tree(from, to, budget, 0)
    } else {
        copy_file(from, to, budget)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Materialized {
    pub name: String,
    pub path: PathBuf,
    pub sha256: String,
}

/// Copies the profile's context, skills, prompts, valid `models.json` and approved
/// extensions into a fresh `dest`, returning the extensions whose copy still hashes as approved.
///
/// # Errors
///
/// Returns the operating system's error when `dest` cannot be replaced or a copy fails, including
/// `FileTooLarge` when the profile exceeds the copy budget.
pub fn materialize(
    profile: &Path,
    dest: &Path,
    approved: impl Fn(&str, &str) -> bool,
) -> io::Result<Vec<Materialized>> {
    if dest.exists() {
        fs::remove_dir_all(dest)?;
    }
    ensure_private_dir(dest)?;
    let mut budget = Budget { files: 0, bytes: 0 };
    let context = profile.join("AGENTS.md");
    if fs::symlink_metadata(&context).is_ok_and(|m| m.is_file()) {
        copy_file(&context, &dest.join("AGENTS.md"), &mut budget)?;
    }
    for dir in ["skills", "prompts"] {
        if profile.join(dir).is_dir() {
            copy_tree(&profile.join(dir), &dest.join(dir), &mut budget, 0)?;
        }
    }
    let models = profile.join("models.json");
    if models.exists() && read_models(&models).is_ok() {
        copy_file(&models, &dest.join("models.json"), &mut budget)?;
    }
    let mut extensions = Vec::new();
    for entry in scan(profile) {
        let (Kind::Extension, Some(hash)) = (entry.kind, entry.sha256.as_deref()) else {
            continue;
        };
        if !approved(&entry.name, hash) {
            continue;
        }
        ensure_private_dir(&dest.join("extensions"))?;
        let target = dest.join("extensions").join(&entry.name);
        copy_entry(
            &profile.join("extensions").join(&entry.name),
            &target,
            &mut budget,
        )?;
        if extension_hash(&target).ok().as_deref() == Some(hash) {
            extensions.push(Materialized {
                name: entry.name,
                path: target,
                sha256: hash.to_string(),
            });
        } else {
            let _ = fs::remove_dir_all(&target).or_else(|_| fs::remove_file(&target));
        }
    }
    Ok(extensions)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub copied: Vec<String>,
    pub skipped: Vec<String>,
}

/// Copies a pi agent directory's context, skills, prompts, extensions and `models.json`
/// into the LADE profile, never overwriting what is already there.
///
/// # Errors
///
/// Returns the operating system's error when `source` is not a readable directory or the profile
/// directories cannot be created; problems with single entries are reported as skipped instead.
pub fn import(source: &Path, profile: &Path) -> io::Result<ImportReport> {
    let meta = fs::metadata(source)?;
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a directory",
        ));
    }
    ensure_private_dir(profile)?;
    let mut report = ImportReport::default();
    let mut budget = Budget { files: 0, bytes: 0 };
    let mut item = |from: PathBuf, to: PathBuf, label: String, report: &mut ImportReport| {
        if fs::symlink_metadata(&to).is_ok() {
            report
                .skipped
                .push(format!("{label}: already in the LADE profile"));
            return;
        }
        match fs::symlink_metadata(&from) {
            Ok(meta) if meta.file_type().is_symlink() => {
                report.skipped.push(format!("{label}: symbolic link"));
            }
            Ok(_) => match copy_entry(&from, &to, &mut budget) {
                Ok(()) => report.copied.push(label),
                Err(e) => report.skipped.push(format!("{label}: {}", e.kind())),
            },
            Err(_) => {}
        }
    };
    item(
        source.join("AGENTS.md"),
        profile.join("AGENTS.md"),
        "AGENTS.md".into(),
        &mut report,
    );
    for dir in ["skills", "prompts", "extensions"] {
        if !source.join(dir).is_dir() {
            continue;
        }
        ensure_private_dir(&profile.join(dir))?;
        for path in sorted_entries(&source.join(dir)) {
            let Some(name) = name_of(&path) else { continue };
            item(
                path.clone(),
                profile.join(dir).join(&name),
                format!("{dir}/{name}"),
                &mut report,
            );
        }
    }
    let models = source.join("models.json");
    if models.exists() {
        match read_models(&models) {
            Ok(_) => item(
                models,
                profile.join("models.json"),
                "models.json".into(),
                &mut report,
            ),
            Err(why) => report.skipped.push(format!("models.json: {why}")),
        }
    }
    report.copied.truncate(256);
    report.skipped.truncate(256);
    Ok(report)
}
