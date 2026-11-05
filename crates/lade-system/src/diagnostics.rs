use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::storage::{SCHEMA_VERSION, backup};
use crate::support::fs::{ensure_private_dir, service_uid};
use crate::support::{now_ms, wire_ts};

pub const LOG_DIR: &str = "logs";
pub const LOG_FILE: &str = "laded.log";
pub const FILE_BYTES: u64 = 1024 * 1024;
pub const FILES: usize = 4;
pub const LINE_BYTES: usize = 2048;

const REDACTED: &str = "<redacted>";

const SECRET_NAMES: &[&str] = &[
    "KEY",
    "TOKEN",
    "SECRET",
    "PASSW",
    "AUTH",
    "CREDENTIAL",
    "COOKIE",
    "CAPABILITY",
];

const SECRET_PREFIXES: &[&str] = &[
    "sk-",
    "sk_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "AKIA",
    "ASIA",
    "AIza",
    "ya29.",
    "eyJ",
];

fn is_secret_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    SECRET_NAMES.iter().any(|s| upper.contains(s))
}

fn trim_punct(s: &str) -> (usize, &str) {
    let is_punct = |c: char| "\"'`()[]{}<>,;".contains(c);
    let start = s.len() - s.trim_start_matches(is_punct).len();
    (start, s.trim_matches(is_punct))
}

fn looks_opaque(core: &str) -> bool {
    core.len() >= 32
        && core
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+/=_-.".contains(c))
        && core.chars().any(|c| c.is_ascii_uppercase())
        && core.chars().any(|c| c.is_ascii_lowercase())
        && core.chars().any(|c| c.is_ascii_digit())
}

fn redact_url_userinfo(token: &str) -> Option<String> {
    let scheme_end = token.find("://")? + 3;
    let rest = &token[scheme_end..];
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let at = rest[..authority_end].rfind('@')?;
    Some(format!("{}{REDACTED}{}", &token[..scheme_end], &rest[at..]))
}

fn redact_token(token: &str, value_next: &mut bool) -> String {
    let (start, core) = trim_punct(token);
    if core.eq_ignore_ascii_case("bearer") || core.eq_ignore_ascii_case("basic") {
        *value_next = true;
        return token.to_owned();
    }
    if std::mem::take(value_next) {
        return REDACTED.to_owned();
    }
    if let Some(colon) = core.find(':')
        && !core[..colon].contains('/')
        && is_secret_name(core[..colon].trim_matches(|c| c == '"' || c == '\''))
    {
        let value = &core[colon + 1..];
        if value.trim_matches(|c| c == '"' || c == '\'').is_empty() {
            *value_next = true;
            return token.to_owned();
        }
        return format!("{}{}:{REDACTED}", &token[..start], &core[..colon]);
    }
    if let Some(eq) = core.find('=')
        && eq > 0
        && is_secret_name(&core[..eq])
    {
        return format!("{}{}={REDACTED}", &token[..start], &core[..eq]);
    }
    if let Some(url) = redact_url_userinfo(token) {
        return url;
    }
    let value = core.split_once('=').map_or(core, |(_, v)| v);
    if (SECRET_PREFIXES
        .iter()
        .any(|p| value.starts_with(p) && value.len() >= p.len() + 12))
        || looks_opaque(value)
    {
        let prefix_len = core.len() - value.len();
        return format!("{}{}{REDACTED}", &token[..start], &core[..prefix_len]);
    }
    token.to_owned()
}

fn replace_home(line: &str, home: &str) -> String {
    if home.len() < 2 || !home.starts_with('/') {
        return line.to_owned();
    }
    let home = home.trim_end_matches('/');
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find(home) {
        let after = &rest[at + home.len()..];
        let boundary = after
            .chars()
            .next()
            .is_none_or(|c| !(c.is_alphanumeric() || "._-".contains(c)));
        out.push_str(&rest[..at]);
        out.push_str(if boundary { "~" } else { home });
        rest = after;
    }
    out.push_str(rest);
    out
}

#[must_use]
pub fn redact(line: &str, home: Option<&str>) -> String {
    let clean: String = line
        .chars()
        .map(|c| if c.is_control() && c != '\t' { ' ' } else { c })
        .collect();
    let clean = match home {
        Some(home) => replace_home(&clean, home),
        None => clean,
    };
    let mut out = String::with_capacity(clean.len());
    let mut value_next = false;
    let mut token_start = None;
    for (i, c) in clean.char_indices() {
        if c.is_whitespace() {
            if let Some(start) = token_start.take() {
                out.push_str(&redact_token(&clean[start..i], &mut value_next));
            }
            out.push(c);
        } else if token_start.is_none() {
            token_start = Some(i);
        }
    }
    if let Some(start) = token_start {
        out.push_str(&redact_token(&clean[start..], &mut value_next));
    }
    out
}

#[must_use]
pub fn home() -> Option<String> {
    std::env::var("HOME").ok().filter(|h| h.starts_with('/'))
}

pub struct RotatingLog {
    dir: PathBuf,
    file_bytes: u64,
    files: usize,
    current: Option<(File, u64)>,
}

impl RotatingLog {
    #[must_use]
    pub fn new(dir: PathBuf, file_bytes: u64, files: usize) -> Self {
        Self {
            dir,
            file_bytes,
            files: files.max(1),
            current: None,
        }
    }

    fn path(&self, index: usize) -> PathBuf {
        if index == 0 {
            self.dir.join(LOG_FILE)
        } else {
            self.dir.join(format!("{LOG_FILE}.{index}"))
        }
    }

    fn open_current(&mut self) -> io::Result<&mut (File, u64)> {
        let current = if let Some(current) = self.current.take() {
            current
        } else {
            ensure_private_dir(&self.dir)?;
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(self.path(0))?;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            let len = file.metadata()?.len();
            (file, len)
        };
        Ok(self.current.insert(current))
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.current = None;
        let last = self.path(self.files - 1);
        match fs::remove_file(&last) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        for index in (0..self.files - 1).rev() {
            match fs::rename(self.path(index), self.path(index + 1)) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Appends one record (cut to fit a file) as a line, rotating the files first when it
    /// would not fit in the current one.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the log directory is unsafe or a file cannot be
    /// opened, rotated or written.
    pub fn append(&mut self, record: &str) -> io::Result<()> {
        let limit = usize::try_from(self.file_bytes)
            .unwrap_or(usize::MAX)
            .max(2)
            - 1;
        let mut end = record.len().min(limit);
        while !record.is_char_boundary(end) {
            end -= 1;
        }
        let bytes = format!("{}\n", &record[..end]);
        let size = bytes.len() as u64;
        let (_, len) = self.open_current()?;
        if *len > 0 && *len + size > self.file_bytes {
            self.rotate()?;
        }
        let (file, len) = self.open_current()?;
        file.write_all(bytes.as_bytes())?;
        *len += size;
        Ok(())
    }
}

fn source_of(line: &str) -> &str {
    match line.find(": ") {
        Some(at) if at <= 64 && !line[..at].contains(' ') => &line[..at],
        _ => "stderr",
    }
}

#[must_use]
pub fn record(line: &str) -> String {
    serde_json::json!({
        "ts": wire_ts(now_ms()).as_str(),
        "pid": std::process::id(),
        "source": source_of(line),
        "line": line,
    })
    .to_string()
}

/// The group and other permission bits, none of which a private directory may have.
const GROUP_OTHER_BITS: u32 = 0o077;

fn state_is_private(state: &Path) -> bool {
    fs::symlink_metadata(state)
        .is_ok_and(|m| m.is_dir() && m.uid() == service_uid() && m.mode() & GROUP_OTHER_BITS == 0)
}

pub struct StderrCapture {
    original: OwnedFd,
    done: mpsc::Receiver<()>,
}

impl StderrCapture {
    pub fn finish(self) {
        let _ = rustix::stdio::dup2_stderr(&self.original);
        let _ = self.done.recv_timeout(Duration::from_secs(2));
    }
}

fn read_bounded_line(reader: &mut impl BufRead, buf: &mut Vec<u8>, max: usize) -> bool {
    buf.clear();
    let limit = u64::try_from(max).unwrap_or(u64::MAX) + 1;
    match reader.by_ref().take(limit).read_until(b'\n', buf) {
        Ok(0) | Err(_) => return false,
        Ok(_) => {}
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        return true;
    }
    if buf.len() > max {
        buf.truncate(max);
        buf.extend_from_slice(" [cut]".as_bytes());
        let mut skip = Vec::new();
        loop {
            skip.clear();
            match reader.by_ref().take(64 * 1024).read_until(b'\n', &mut skip) {
                Ok(0) | Err(_) => break,
                Ok(_) if skip.last() == Some(&b'\n') => break,
                Ok(_) => {}
            }
        }
    }
    true
}

/// Redirects the process's stderr through a thread that redacts each line, copies it to
/// the original stderr and appends it to the rotating log under `state`.
///
/// # Errors
///
/// Returns the operating system's error when stderr cannot be duplicated or redirected, the pipe
/// cannot be created, or the thread cannot be spawned.
pub fn capture_stderr(state: PathBuf) -> io::Result<StderrCapture> {
    let original = rustix::io::fcntl_dupfd_cloexec(io::stderr(), 3)?;
    let (read, write) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)?;
    let mut out = File::from(original.try_clone()?);
    let (done_tx, done) = mpsc::channel();
    let home = home();
    thread::Builder::new()
        .name("laded-diagnostics".into())
        .spawn(move || {
            let mut reader = BufReader::new(File::from(read));
            let mut log = RotatingLog::new(state.join(LOG_DIR), FILE_BYTES, FILES);
            let mut buf = Vec::new();
            while read_bounded_line(&mut reader, &mut buf, LINE_BYTES) {
                let line = redact(&String::from_utf8_lossy(&buf), home.as_deref());
                let _ = writeln!(out, "{line}");
                if state_is_private(&state) {
                    let _ = log.append(&record(&line));
                }
            }
            let _ = done_tx.send(());
        })?;
    rustix::stdio::dup2_stderr(&write)?;
    Ok(StderrCapture { original, done })
}

#[derive(Debug)]
pub enum ExportError {
    Exists,
    Io(&'static str, io::ErrorKind),
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exists => f.write_str("the output file already exists; choose a new name"),
            Self::Io(what, kind) => write!(f, "{what}: {kind}"),
        }
    }
}

impl std::error::Error for ExportError {}

#[derive(Debug)]
pub struct ExportSummary {
    pub sections: Vec<(String, usize)>,
    pub bytes: u64,
}

fn read_lines_lossy(path: &Path) -> io::Result<Option<Vec<String>>> {
    let mut bytes = Vec::new();
    match File::open(path) {
        Ok(mut file) => {
            file.read_to_end(&mut bytes)?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    }
    Ok(Some(
        String::from_utf8_lossy(&bytes)
            .lines()
            .map(str::to_owned)
            .collect(),
    ))
}

/// Writes a redacted diagnostics bundle (logs, versions, migration journal) to the new
/// file `out`.
///
/// # Errors
///
/// Returns [`ExportError::Exists`] when `out` already exists, or [`ExportError::Io`] naming the step
/// that failed; a partly written file is removed.
pub fn export(state: &Path, out: &Path) -> Result<ExportSummary, ExportError> {
    let home = home();
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(out)
        .map_err(|e| match e.kind() {
            io::ErrorKind::AlreadyExists => ExportError::Exists,
            kind => ExportError::Io("create the export file", kind),
        })?;
    let written = write_export(state, &mut file, home.as_deref()).and_then(|summary| {
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .and_then(|()| file.sync_all())
            .map_err(|e| ExportError::Io("write the export file", e.kind()))?;
        Ok(summary)
    });
    if written.is_err() {
        let _ = fs::remove_file(out);
    }
    written
}

fn write_export(
    state: &Path,
    file: &mut File,
    home: Option<&str>,
) -> Result<ExportSummary, ExportError> {
    let io_err = |e: io::Error| ExportError::Io("write the export file", e.kind());
    let read_err = |e: io::Error| ExportError::Io("read diagnostics", e.kind());
    let database = backup::database_schema(&state.join(backup::DATABASE))
        .ok()
        .flatten()
        .map_or_else(|| "absent or unreadable".to_owned(), |v| v.to_string());
    let kernel = fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let mut text = String::new();
    text.push_str("LADE diagnostics export (redacted; review before sharing)\n");
    let _ = writeln!(text, "created={}", wire_ts(now_ms()).as_str());
    let _ = writeln!(
        text,
        "laded={} protocol={}",
        lade_protocol::BUNDLE,
        lade_protocol::VERSION
    );
    let _ = writeln!(
        text,
        "supported_schema={} database_schema={database}",
        SCHEMA_VERSION
    );
    let _ = writeln!(text, "kernel={}", kernel.trim());
    text.push_str(
        "included: service logs (already redacted, redacted again), migration journal\n\
         excluded: database contents, prompts, source, tool output, terminal output, \
         credentials, environment, artifacts, worktrees, session files\n",
    );
    let mut sections = Vec::new();
    let mut paths: Vec<(String, PathBuf)> = (0..FILES)
        .rev()
        .map(|i| {
            let name = if i == 0 {
                LOG_FILE.to_owned()
            } else {
                format!("{LOG_FILE}.{i}")
            };
            (format!("{LOG_DIR}/{name}"), state.join(LOG_DIR).join(name))
        })
        .collect();
    paths.push((
        format!("{}/{}", backup::BACKUP_DIR, backup::JOURNAL),
        state.join(backup::BACKUP_DIR).join(backup::JOURNAL),
    ));
    for (section, path) in paths {
        let Some(lines) = read_lines_lossy(&path).map_err(read_err)? else {
            continue;
        };
        let _ = writeln!(text, "\n=== {section} ({} lines) ===", lines.len());
        for line in &lines {
            text.push_str(&redact(line, home));
            text.push('\n');
        }
        sections.push((section, lines.len()));
    }
    file.write_all(text.as_bytes()).map_err(io_err)?;
    Ok(ExportSummary {
        sections,
        bytes: text.len() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_and_home_are_redacted() {
        let home = Some("/home/ana");
        let cases = [
            (
                "laded: env OPENAI_API_KEY=sk-proj-abcdefghijklmnop1234 set",
                "laded: env OPENAI_API_KEY=<redacted> set",
            ),
            (
                "Authorization: Bearer abc.def.ghi",
                "Authorization: Bearer <redacted>",
            ),
            (
                r#"{"api_key":"xyz12345","model":"m"}"#,
                r#"{"api_key":<redacted>"#,
            ),
            ("token: hunter2", "token: <redacted>"),
            (
                "fetch https://user:pa55@example.com/x failed",
                "fetch https://<redacted>@example.com/x failed",
            ),
            (
                "key ghp_0123456789abcdefABCDEF leaked",
                "key <redacted> leaked",
            ),
            (
                "opaque Zm9vYmFyYmF6cXV4MTIzNDU2Nzg5MEFCQ0RFRkdI end",
                "opaque <redacted> end",
            ),
            (
                "open /home/ana/src/repo/.git failed; /home/anabel kept",
                "open ~/src/repo/.git failed; /home/anabel kept",
            ),
            ("bell\u{7}\r", "bell  "),
        ];
        for (input, expected) in cases {
            assert_eq!(redact(input, home), expected, "input: {input}");
        }
    }

    #[test]
    fn ids_digests_and_codes_are_kept() {
        let line = "laded: launch failed objective=6f1c2b8e-1d2a-4c3b-9e8f-0a1b2c3d4e5f \
                    reason=sandbox_unavailable digest=\
                    9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08 pid=42";
        assert_eq!(redact(line, Some("/home/ana")), line);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lade-diag-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("chmod");
        dir
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).expect("metadata").permissions().mode() & 0o777
    }

    #[test]
    fn rotation_bounds_total_size_and_keeps_private_modes() {
        let dir = temp_dir("rotate").join(LOG_DIR);
        let mut log = RotatingLog::new(dir.clone(), 4096, 4);
        let line = "x".repeat(300);
        for _ in 0..1000 {
            log.append(&line).expect("append");
        }
        let mut total = 0;
        let mut count = 0;
        for entry in fs::read_dir(&dir).expect("dir") {
            let entry = entry.expect("entry");
            let len = entry.metadata().expect("meta").len();
            assert!(len <= 4096, "{len}");
            assert_eq!(mode(&entry.path()), 0o600);
            total += len;
            count += 1;
        }
        assert_eq!(count, 4);
        assert!(total <= 4 * 4096);
        assert!(total > 3 * 3500, "older files are full: {total}");
        assert_eq!(mode(&dir), 0o700);
        log.append(&"y".repeat(10_000)).expect("append");
        assert!(fs::metadata(dir.join(LOG_FILE)).expect("meta").len() <= 4096);
        let _ = fs::remove_dir_all(dir.parent().expect("parent"));
    }

    #[test]
    fn bounded_line_reader_cuts_long_lines() {
        let input = format!("short\n{}\nnext\n", "z".repeat(5000));
        let mut reader = BufReader::new(input.as_bytes());
        let mut buf = Vec::new();
        let mut lines = Vec::new();
        while read_bounded_line(&mut reader, &mut buf, 100) {
            lines.push(String::from_utf8_lossy(&buf).into_owned());
        }
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "short");
        assert!(lines[1].len() <= 100 + 6 && lines[1].ends_with("[cut]"));
        assert_eq!(lines[2], "next");
    }

    #[test]
    fn export_is_private_redacted_and_never_overwrites() {
        let state = temp_dir("export");
        let mut log = RotatingLog::new(state.join(LOG_DIR), FILE_BYTES, FILES);
        log.append(&record("laded: serving epoch=1"))
            .expect("append");
        log.append("ANTHROPIC_API_KEY=sk-ant-0123456789abcdefghij")
            .expect("append");
        let out = state.join("export.txt");
        let summary = export(&state, &out).expect("export");
        assert_eq!(mode(&out), 0o600);
        let text = fs::read_to_string(&out).expect("read");
        assert!(text.contains("serving epoch=1"));
        assert!(text.contains("ANTHROPIC_API_KEY=<redacted>"));
        assert!(!text.contains("sk-ant-"));
        assert_eq!(summary.sections, [("logs/laded.log".to_owned(), 2)]);
        assert!(matches!(export(&state, &out), Err(ExportError::Exists)));
        let _ = fs::remove_dir_all(state);
    }
}
