//! The LADE profile: LADE-managed pi resources (context file, skills, prompt templates, executable
//! extensions and custom providers) under `$STATE/profile`. It is never the user's own pi profile:
//! `~/.pi` is read only by an explicit, copy-only [`import`], which never writes to its source.
//!
//! Executable extensions are identified by a content hash (the same algorithm as the worker's
//! `extensionHash` in `packages/pi-worker/src/worker/resources.ts`): a file hashes as its bytes; a
//! directory as the SHA-256 of `sha256sum`-style lines (`<hex>  <relative path>\n`) of every regular
//! file sorted by path bytes. Symbolic links and special files are refused. Each launch and reload
//! materialises a snapshot of the profile in the session directory containing only approved
//! extensions whose copied content still hashes to the approved value.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::net::IpAddr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::supervision::gateway::is_non_public;

/// Directory-extension hashing bounds (mirrored by the worker).
pub const MAX_EXTENSION_FILES: usize = 256;
pub const MAX_EXTENSION_BYTES: u64 = 8 << 20;
const MAX_DEPTH: usize = 8;
/// Bounds for copying a profile tree (snapshot or import).
const MAX_COPY_FILES: usize = 2048;
const MAX_COPY_BYTES: u64 = 32 << 20;
/// Inventory bound (the wire carries at most 256 entries).
pub const MAX_ENTRIES: usize = 200;
const MAX_MODELS_JSON: u64 = 1 << 20;
/// Custom-provider credential variables must carry this prefix (see the credentials module).
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

/// One profile resource. `problem` explains why it cannot be used at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    pub name: String,
    pub sha256: Option<String>,
    pub problem: Option<String>,
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
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

/// Content hash of one extension entry (see the module documentation).
pub fn extension_hash(path: &Path) -> Result<String, String> {
    let meta = fs::symlink_metadata(path).map_err(|e| format!("unreadable: {}", e.kind()))?;
    if meta.file_type().is_symlink() {
        return Err("extension is a symbolic link".into());
    }
    if meta.is_file() {
        if meta.len() > MAX_EXTENSION_BYTES {
            return Err("extension is too large".into());
        }
        let bytes = fs::read(path).map_err(|e| format!("unreadable: {}", e.kind()))?;
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
        let bytes = fs::read(&file).map_err(|e| format!("unreadable: {}", e.kind()))?;
        total += bytes.len() as u64;
        if total > MAX_EXTENSION_BYTES {
            return Err("extension is too large".into());
        }
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

/// Inventory of a profile directory (missing directory: empty). Bounded to [`MAX_ENTRIES`].
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

// ---------------------------------------------------------------------------------------------
// Custom providers (`models.json`).

/// A custom provider the LADE profile defines and LADE can reach through its gateway.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomProvider {
    pub id: String,
    pub host: String,
    pub port: u16,
    /// The endpoint is a loopback/private address (a local server such as Ollama).
    pub local: bool,
    /// Credential variables (`LADE_PROVIDER_*`) referenced as `$NAME` or `${NAME}`.
    pub vars: Vec<String>,
}

fn variable(value: &str) -> Option<&str> {
    let name = value
        .strip_prefix("${")
        .and_then(|v| v.strip_suffix('}'))
        .or_else(|| value.strip_prefix('$'))?;
    let valid = name.starts_with(CUSTOM_VAR_PREFIX)
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
    valid.then_some(name)
}

/// `scheme://host[:port][/path]` → (secure, host, port).
fn endpoint(url: &str) -> Result<(bool, String, u16), String> {
    let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err("baseUrl must be an http(s) URL".into());
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err("baseUrl needs a host and no user information".into());
    }
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let (host, tail) = v6.split_once(']').ok_or("invalid IPv6 host")?;
        (host.to_string(), tail.strip_prefix(':'))
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host.to_string(), Some(port)),
            None => (authority.to_string(), None),
        }
    };
    let port = match port {
        Some(port) => port.parse::<u16>().map_err(|_| "invalid baseUrl port")?,
        None if secure => 443,
        None => 80,
    };
    let host = host.to_ascii_lowercase();
    let valid = !host.is_empty()
        && host.len() <= 253
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':'));
    if !valid || port == 0 {
        return Err("invalid baseUrl host".into());
    }
    Ok((secure, host, port))
}

fn is_local(host: &str) -> bool {
    host == "localhost" || host.parse::<IpAddr>().is_ok_and(is_non_public)
}

fn read_models(path: &Path) -> Result<(Vec<u8>, Vec<CustomProvider>), String> {
    let meta = fs::symlink_metadata(path).map_err(|e| format!("unreadable: {}", e.kind()))?;
    if !meta.is_file() {
        return Err("models.json is not a regular file".into());
    }
    if meta.len() > MAX_MODELS_JSON {
        return Err("models.json is larger than 1 MiB".into());
    }
    let bytes = fs::read(path).map_err(|e| format!("unreadable: {}", e.kind()))?;
    let providers = custom_providers(&bytes)?;
    Ok((bytes, providers))
}

/// Validates the LADE profile's `models.json` against LADE's trust rules and returns its providers.
///
/// Refused: shell-command values (`!…`), environment interpolation other than one whole
/// `LADE_PROVIDER_*` variable, literal keys for non-local endpoints, `oauth` providers, plain HTTP
/// to public hosts, and ids that redefine a provider LADE already has a policy or credential
/// mapping for (`builtin`).
pub fn custom_providers(bytes: &[u8]) -> Result<Vec<CustomProvider>, String> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| "models.json is not valid JSON".to_string())?;
    let providers = value
        .get("providers")
        .and_then(Value::as_object)
        .ok_or("models.json needs a providers object")?;
    let mut found = Vec::new();
    for (id, config) in providers {
        let valid_id = !id.is_empty()
            && id.len() <= 64
            && id.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.' | b'_')
            });
        if !valid_id {
            return Err(format!("provider id {id:?} is not a lowercase label"));
        }
        if crate::supervision::credentials::provider_env_vars(id).is_some()
            || crate::supervision::gateway::default_policies().contains_key(id)
        {
            return Err(format!(
                "provider {id} redefines a built-in provider; its reviewed endpoint policy applies"
            ));
        }
        let config = config
            .as_object()
            .ok_or(format!("provider {id} must be an object"))?;
        if config.contains_key("oauth") {
            return Err(format!(
                "provider {id}: OAuth providers are not supported (no credential custody)"
            ));
        }
        let url = config
            .get("baseUrl")
            .and_then(Value::as_str)
            .ok_or(format!("provider {id} needs a baseUrl"))?;
        let (secure, host, port) = endpoint(url).map_err(|e| format!("provider {id}: {e}"))?;
        let local = is_local(&host);
        if !secure && !local {
            return Err(format!(
                "provider {id}: plain HTTP is allowed only for local servers"
            ));
        }
        let mut vars = BTreeSet::new();
        let mut check = |what: &str, value: &str| -> Result<(), String> {
            if value.starts_with('!') {
                return Err(format!(
                    "provider {id}: {what} runs a shell command, which LADE refuses"
                ));
            }
            if let Some(name) = variable(value) {
                vars.insert(name.to_string());
                return Ok(());
            }
            if value.contains('$') {
                return Err(format!(
                    "provider {id}: {what} may reference one whole {CUSTOM_VAR_PREFIX}* variable only"
                ));
            }
            if what == "apiKey" && !local {
                return Err(format!(
                    "provider {id}: put the key in credentials.env and reference it as ${CUSTOM_VAR_PREFIX}…"
                ));
            }
            Ok(())
        };
        if let Some(key) = config.get("apiKey") {
            check(
                "apiKey",
                key.as_str()
                    .ok_or(format!("provider {id}: apiKey must be text"))?,
            )?;
        }
        if let Some(headers) = config.get("headers") {
            let headers = headers
                .as_object()
                .ok_or(format!("provider {id}: headers must be an object"))?;
            for value in headers.values() {
                check(
                    "a header",
                    value
                        .as_str()
                        .ok_or(format!("provider {id}: header values must be text"))?,
                )?;
            }
        }
        found.push(CustomProvider {
            id: id.clone(),
            host,
            port,
            local,
            vars: vars.into_iter().collect(),
        });
    }
    Ok(found)
}

/// The custom providers of `profile/models.json` (none when absent; an error when invalid).
pub fn profile_providers(profile: &Path) -> Result<Vec<CustomProvider>, String> {
    let path = profile.join("models.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    read_models(&path).map(|(_, providers)| providers)
}

// ---------------------------------------------------------------------------------------------
// Copies.

struct Budget {
    files: usize,
    bytes: u64,
}

fn private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

/// Copies one regular file without following links on either side (destination created fresh).
fn copy_file(from: &Path, to: &Path, budget: &mut Budget) -> io::Result<()> {
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

/// Copies a tree of regular files and directories; symbolic links and special files are skipped.
fn copy_tree(from: &Path, to: &Path, budget: &mut Budget, depth: usize) -> io::Result<()> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    private_dir(to)?;
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

fn copy_entry(from: &Path, to: &Path, budget: &mut Budget) -> io::Result<()> {
    if fs::symlink_metadata(from)?.is_dir() {
        copy_tree(from, to, budget, 0)
    } else {
        copy_file(from, to, budget)
    }
}

/// An approved extension inside a materialised snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Materialized {
    pub name: String,
    pub path: PathBuf,
    pub sha256: String,
}

/// Replaces `dest` with a snapshot of `profile`: context file, skills, prompts, a valid
/// `models.json`, and only those extensions `approved(name, sha256)` accepts whose copy still
/// hashes to the approved value. Returns the approved extensions as loaded by the worker.
pub fn materialize(
    profile: &Path,
    dest: &Path,
    approved: impl Fn(&str, &str) -> bool,
) -> io::Result<Vec<Materialized>> {
    if dest.exists() {
        fs::remove_dir_all(dest)?;
    }
    private_dir(dest)?;
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
        private_dir(&dest.join("extensions"))?;
        let target = dest.join("extensions").join(&entry.name);
        copy_entry(
            &profile.join("extensions").join(&entry.name),
            &target,
            &mut budget,
        )?;
        // The snapshot itself must match the approval (the source may change while copying).
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

/// What a profile import copied and skipped (names and reasons only).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub copied: Vec<String>,
    pub skipped: Vec<String>,
}

/// Copy-only import from a pi agent directory (`~/.pi/agent` layout): `AGENTS.md`, `skills/`,
/// `prompts/`, `extensions/` and a `models.json` that passes LADE's rules. The source is only read;
/// nothing existing in the LADE profile is overwritten; imported extensions still need approval.
pub fn import(source: &Path, profile: &Path) -> io::Result<ImportReport> {
    let meta = fs::metadata(source)?;
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a directory",
        ));
    }
    private_dir(profile)?;
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
        private_dir(&profile.join(dir))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let mut bytes = [0u8; 8];
        crate::service::convert::random_fill(&mut bytes).unwrap();
        let dir = std::env::temp_dir().join(format!("lade-profile-{tag}-{}", hex(&bytes)));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn extension_hashes_match_the_worker_algorithm() {
        let dir = temp("hash");
        let ext = dir.join("ext");
        fs::create_dir_all(ext.join("lib")).unwrap();
        fs::write(ext.join("index.js"), "export default function () {}\n").unwrap();
        fs::write(ext.join("lib/util.js"), "export const x = 1;\n").unwrap();
        // The same constants are asserted by packages/pi-worker/test/worker-compat.test.ts.
        assert_eq!(
            extension_hash(&ext.join("index.js")).unwrap(),
            "4554c00dbb28cad89d67afb301d893073eefca19bee50292764c7b8c3dc8bf03"
        );
        assert_eq!(
            extension_hash(&ext).unwrap(),
            "3649d03b8d53b790fab2b926092c3e3ff4dfac57fbf968ab8c3e8e27b767d73a"
        );
        std::os::unix::fs::symlink("index.js", ext.join("link.js")).unwrap();
        assert!(extension_hash(&ext).unwrap_err().contains("symbolic link"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn snapshots_contain_only_approved_unchanged_extensions() {
        let dir = temp("snapshot");
        let profile = dir.join("profile");
        fs::create_dir_all(profile.join("extensions")).unwrap();
        fs::create_dir_all(profile.join("skills/demo")).unwrap();
        fs::write(profile.join("AGENTS.md"), "context").unwrap();
        fs::write(profile.join("skills/demo/SKILL.md"), "skill").unwrap();
        fs::write(profile.join("extensions/a.js"), "a").unwrap();
        fs::write(profile.join("extensions/b.js"), "b").unwrap();
        fs::write(profile.join("extensions/notes.txt"), "ignored").unwrap();
        let a = sha256(b"a");
        let entries = scan(&profile);
        assert_eq!(
            entries
                .iter()
                .filter(|e| e.kind == Kind::Extension)
                .count(),
            2
        );
        let dest = dir.join("session/profile");
        let loaded = materialize(&profile, &dest, |name, sha| name == "a.js" && sha == a).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "a.js");
        assert!(dest.join("AGENTS.md").is_file());
        assert!(dest.join("skills/demo/SKILL.md").is_file());
        assert!(!dest.join("extensions/b.js").exists(), "unapproved code is never copied");
        // A changed file no longer matches its approval.
        fs::write(profile.join("extensions/a.js"), "changed").unwrap();
        let loaded = materialize(&profile, &dest, |name, sha| name == "a.js" && sha == a).unwrap();
        assert!(loaded.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn variables_need_the_custom_prefix() {
        assert_eq!(variable("$LADE_PROVIDER_KEY"), Some("LADE_PROVIDER_KEY"));
        assert_eq!(variable("${LADE_PROVIDER_KEY}"), Some("LADE_PROVIDER_KEY"));
        assert_eq!(variable("$OPENAI_API_KEY"), None);
        assert_eq!(variable("$LADE_PROVIDER_key"), None);
    }

    #[test]
    fn endpoints_parse_scheme_host_and_port() {
        assert_eq!(
            endpoint("https://api.example.com/v1").unwrap(),
            (true, "api.example.com".into(), 443)
        );
        assert_eq!(
            endpoint("http://localhost:11434/v1").unwrap(),
            (false, "localhost".into(), 11434)
        );
        assert_eq!(
            endpoint("http://[::1]:8080").unwrap(),
            (false, "::1".into(), 8080)
        );
        assert!(endpoint("ftp://x").is_err());
        assert!(endpoint("https://user@host").is_err());
        assert!(endpoint("https://host:0").is_err());
    }

    #[test]
    fn custom_providers_follow_the_trust_rules() {
        let ok = br#"{"providers":{"local":{"baseUrl":"http://127.0.0.1:11434/v1","apiKey":"ollama","api":"openai-completions","models":[{"id":"m"}]},
            "proxy":{"baseUrl":"https://llm.example.com/v1","apiKey":"$LADE_PROVIDER_PROXY","headers":{"x-team":"lade"}}}}"#;
        let providers = custom_providers(ok).unwrap();
        assert_eq!(providers.len(), 2);
        assert!(providers[0].local);
        assert_eq!(providers[1].vars, ["LADE_PROVIDER_PROXY"]);
        assert_eq!(providers[1].port, 443);
        for (bad, why) in [
            (
                r#"{"providers":{"p":{"baseUrl":"https://h","apiKey":"!op read x"}}}"#,
                "shell",
            ),
            (
                r#"{"providers":{"p":{"baseUrl":"https://h","apiKey":"sk-literal"}}}"#,
                "credentials.env",
            ),
            (
                r#"{"providers":{"p":{"baseUrl":"https://h","apiKey":"$OPENAI_API_KEY"}}}"#,
                "variable",
            ),
            (
                r#"{"providers":{"p":{"baseUrl":"http://public.example"}}}"#,
                "plain HTTP",
            ),
            (
                r#"{"providers":{"anthropic":{"baseUrl":"https://proxy"}}}"#,
                "built-in",
            ),
            (
                r#"{"providers":{"p":{"baseUrl":"https://h","oauth":"radius"}}}"#,
                "OAuth",
            ),
            (
                r#"{"providers":{"p":{"baseUrl":"https://h","headers":{"k":"!cmd"}}}}"#,
                "shell",
            ),
        ] {
            let error = custom_providers(bad.as_bytes()).unwrap_err();
            assert!(error.contains(why), "{bad}: {error}");
        }
    }
}
