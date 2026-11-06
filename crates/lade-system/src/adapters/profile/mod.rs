use std::fs;
use std::io;
use std::io::Read as _;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::support::hex;

mod copy;
mod models;
#[cfg(test)]
mod tests;

pub use copy::{ImportReport, Materialized, import, materialize};
use models::read_models;
pub use models::{CustomProvider, custom_providers, profile_providers};

pub const MAX_EXTENSION_FILES: usize = 256;
pub const MAX_EXTENSION_BYTES: u64 = 8 << 20;
const MAX_DEPTH: usize = 8;
const MAX_COPY_FILES: usize = 2048;
const MAX_COPY_BYTES: u64 = 32 << 20;
pub const MAX_ENTRIES: usize = 200;
const MAX_MODELS_JSON: u64 = 1 << 20;
pub const CUSTOM_VAR_PREFIX: &str = "LADE_PROVIDER_";

const EXTENSION_SUFFIXES: &[&str] = &[".ts", ".js", ".mjs", ".cjs", ".mts", ".cts"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Context,
    Skill,
    Prompt,
    Extension,
    Models,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    pub name: String,
    pub sha256: Option<String>,
    pub problem: Option<String>,
}

fn sha256(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn walk(root: &Path, dir: &Path, depth: usize, files: &mut Vec<Vec<u8>>) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("extension directory is nested too deeply".into());
    }
    let entries = fs::read_dir(dir).map_err(|e| format!("unreadable: {}", e.kind()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("unreadable: {}", e.kind()))?;
        let path = entry.path();
        let kind = entry
            .file_type()
            .map_err(|e| format!("unreadable: {}", e.kind()))?;
        if kind.is_symlink() {
            return Err("extension contains a symbolic link".into());
        }
        if kind.is_dir() {
            walk(root, &path, depth + 1, files)?;
        } else if kind.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(|_| "path outside extension".to_string())?;
            files.push(rel.as_os_str().as_bytes().to_vec());
            if files.len() > MAX_EXTENSION_FILES {
                return Err("extension has too many files".into());
            }
        } else {
            return Err("extension contains a special file".into());
        }
    }
    Ok(())
}

fn read_extension_file(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let unreadable = |e: io::Error| format!("unreadable: {}", e.kind());
    let file = fs::File::open(path).map_err(unreadable)?;
    if file.metadata().map_err(unreadable)?.len() > limit {
        return Err("extension is too large".into());
    }
    let mut bytes = Vec::new();
    // Cap the read too: the file can grow after its metadata was checked.
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    if bytes.len() as u64 > limit {
        return Err("extension is too large".into());
    }
    Ok(bytes)
}

/// The SHA-256 identity of an extension: of the file itself, or of a sorted manifest of
/// every file's digest and path for a directory. The worker computes the same value.
///
/// # Errors
///
/// Returns a reason the developer can read when the extension is a symbolic link or special file,
/// contains one, is nested more than eight levels deep, has too many files or bytes, or cannot be
/// read.
pub fn extension_hash(path: &Path) -> Result<String, String> {
    let meta = fs::symlink_metadata(path).map_err(|e| format!("unreadable: {}", e.kind()))?;
    if meta.file_type().is_symlink() {
        return Err("extension is a symbolic link".into());
    }
    if meta.is_file() {
        let bytes = read_extension_file(path, MAX_EXTENSION_BYTES)?;
        return Ok(sha256(&bytes));
    }
    if !meta.is_dir() {
        return Err("extension is not a file or directory".into());
    }
    let mut files = Vec::new();
    walk(path, path, 0, &mut files)?;
    files.sort();
    let mut manifest = Sha256::new();
    let mut total = 0u64;
    for rel in files {
        let file = path.join(std::ffi::OsStr::from_bytes(&rel));
        let bytes = read_extension_file(&file, MAX_EXTENSION_BYTES - total)?;
        total += bytes.len() as u64;
        manifest.update(sha256(&bytes).as_bytes());
        manifest.update(b"  ");
        manifest.update(&rel);
        manifest.update(b"\n");
    }
    Ok(hex(&manifest.finalize()))
}

fn name_of(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    (!name.starts_with('.') && !name.is_empty() && name.len() <= 256).then(|| name.to_string())
}

fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = read.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    paths
}

fn is_markdown(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "md")
}

#[must_use]
pub fn scan(profile: &Path) -> Vec<Entry> {
    let mut entries = Vec::new();
    let plain = |kind, name: String, path: &Path| {
        let regular = fs::symlink_metadata(path).is_ok_and(|m| m.is_file());
        Entry {
            kind,
            name,
            sha256: None,
            problem: (!regular).then(|| "not a regular file".to_string()),
        }
    };
    let context = profile.join("AGENTS.md");
    if context.exists() {
        entries.push(plain(Kind::Context, "AGENTS.md".into(), &context));
    }
    for path in sorted_entries(&profile.join("skills")) {
        let Some(name) = name_of(&path) else { continue };
        if path.is_dir() {
            let skill = path.join("SKILL.md");
            if skill.exists() {
                entries.push(plain(Kind::Skill, name, &skill));
            }
        } else if is_markdown(&path) {
            let stem = name.trim_end_matches(".md").to_string();
            entries.push(plain(Kind::Skill, stem, &path));
        }
    }
    for path in sorted_entries(&profile.join("prompts")) {
        let Some(name) = name_of(&path) else { continue };
        if is_markdown(&path) {
            entries.push(plain(
                Kind::Prompt,
                name.trim_end_matches(".md").into(),
                &path,
            ));
        }
    }
    for path in sorted_entries(&profile.join("extensions")) {
        let Some(name) = name_of(&path) else { continue };
        let file = fs::symlink_metadata(&path).is_ok_and(|m| m.is_file());
        if file && !EXTENSION_SUFFIXES.iter().any(|s| name.ends_with(s)) {
            continue;
        }
        let (sha256, problem) = match extension_hash(&path) {
            Ok(hash) => (Some(hash), None),
            Err(why) => (None, Some(why)),
        };
        entries.push(Entry {
            kind: Kind::Extension,
            name,
            sha256,
            problem,
        });
    }
    let models = profile.join("models.json");
    if models.exists() {
        let (sha256, problem) = match read_models(&models) {
            Ok((bytes, _)) => (Some(self::sha256(&bytes)), None),
            Err(why) => (None, Some(why)),
        };
        entries.push(Entry {
            kind: Kind::Models,
            name: "models.json".into(),
            sha256,
            problem,
        });
    }
    entries.truncate(MAX_ENTRIES);
    entries
}
