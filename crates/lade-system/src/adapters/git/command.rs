use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::thread::{self, JoinHandle};

use super::GitError;

pub const STDERR_LIMIT: usize = 2048;
const DEFAULT_STDOUT_LIMIT: usize = 64 << 20;
const CONFIG_SCAN_LIMIT: usize = 4 << 20;
const GIT_PROGRAM: &str = "git";
const FALLBACK_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

pub const LADE_IDENTITY_NAME: &str = "LADE";
pub const LADE_IDENTITY_EMAIL: &str = "lade@localhost";

const HARDENED_CONFIG: &[(&str, &str)] = &[
    ("core.hooksPath", "/dev/null"),
    ("core.fsmonitor", "false"),
    ("core.untrackedCache", "false"),
    ("core.pager", "cat"),
    ("core.editor", "false"),
    ("core.askPass", ""),
    ("core.sshCommand", "false"),
    ("core.attributesFile", "/dev/null"),
    ("core.alternateRefsCommand", ""),
    ("protocol.allow", "never"),
    ("credential.helper", ""),
    ("diff.external", ""),
    ("diff.suppressBlankEmpty", "false"),
    ("commit.gpgSign", "false"),
    ("tag.gpgSign", "false"),
    ("gpg.program", "false"),
    ("log.showSignature", "false"),
    ("gc.auto", "0"),
    ("maintenance.auto", "false"),
    ("submodule.recurse", "false"),
    ("merge.renormalize", "false"),
    ("color.ui", "false"),
    ("uploadpack.packObjectsHook", ""),
    ("sequence.editor", "false"),
];

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Diagnostics {
    bytes: Vec<u8>,
    truncated: bool,
}

impl Diagnostics {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    pub(crate) fn contains(&self, needle: &str) -> bool {
        self.bytes
            .windows(needle.len())
            .any(|w| w == needle.as_bytes())
    }
}

impl fmt::Debug for Diagnostics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", String::from_utf8_lossy(&self.bytes))?;
        if self.truncated {
            f.write_str("…")?;
        }
        Ok(())
    }
}

pub(crate) struct GitOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Diagnostics,
}

pub(crate) struct GitCommand {
    op: &'static str,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    config: Vec<(OsString, OsString)>,
    git_dir: Option<PathBuf>,
    work_tree: Option<PathBuf>,
    cwd: Option<PathBuf>,
    read_only: bool,
    stdin: Option<Vec<u8>>,
    stdout_limit: usize,
    scan_config: bool,
}

impl GitCommand {
    pub(crate) fn new(op: &'static str) -> Self {
        Self {
            op,
            args: Vec::new(),
            env: Vec::new(),
            config: Vec::new(),
            git_dir: None,
            work_tree: None,
            cwd: None,
            read_only: false,
            stdin: None,
            stdout_limit: DEFAULT_STDOUT_LIMIT,
            scan_config: true,
        }
    }

    pub(crate) fn without_config_scan(mut self) -> Self {
        self.scan_config = false;
        self
    }

    pub(crate) fn git_dir(mut self, path: &Path) -> Self {
        self.git_dir = Some(path.to_path_buf());
        self
    }

    pub(crate) fn work_tree(mut self, path: &Path) -> Self {
        self.work_tree = Some(path.to_path_buf());
        self
    }

    pub(crate) fn cwd(mut self, path: &Path) -> Self {
        self.cwd = Some(path.to_path_buf());
        self
    }

    pub(crate) fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    pub(crate) fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|a| a.as_ref().to_os_string()));
        self
    }

    pub(crate) fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.env
            .push((key.as_ref().to_os_string(), value.as_ref().to_os_string()));
        self
    }

    pub(crate) fn config(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.config
            .push((key.as_ref().to_os_string(), value.as_ref().to_os_string()));
        self
    }

    pub(crate) fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    pub(crate) fn stdin(mut self, bytes: Vec<u8>) -> Self {
        self.stdin = Some(bytes);
        self
    }

    pub(crate) fn stdout_limit(mut self, limit: usize) -> Self {
        self.stdout_limit = limit;
        self
    }

    fn build(&self, scanned: &[(OsString, OsString)]) -> Command {
        let mut cmd = Command::new(GIT_PROGRAM);
        cmd.env_clear();
        cmd.env(
            "PATH",
            std::env::var_os("PATH").unwrap_or_else(|| FALLBACK_PATH.into()),
        );
        cmd.env("HOME", "/nonexistent");
        cmd.env("LC_ALL", "C");
        cmd.env("LANG", "C");
        cmd.env("GIT_CONFIG_NOSYSTEM", "1");
        cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
        cmd.env("GIT_ATTR_NOSYSTEM", "1");
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        cmd.env("GIT_NO_REPLACE_OBJECTS", "1");
        cmd.env("GIT_LITERAL_PATHSPECS", "1");
        cmd.env("GIT_PROTOCOL_FROM_USER", "0");
        cmd.env("GIT_NO_LAZY_FETCH", "1");
        cmd.env("GIT_PAGER", "cat");
        cmd.env("GIT_EDITOR", "false");
        cmd.env("GIT_ADVICE", "0");
        if self.read_only {
            cmd.env("GIT_OPTIONAL_LOCKS", "0");
        }
        let total = HARDENED_CONFIG.len() + scanned.len() + self.config.len();
        cmd.env("GIT_CONFIG_COUNT", total.to_string());
        let pairs = HARDENED_CONFIG
            .iter()
            .map(|(k, v)| (OsStr::new(k), OsStr::new(v)))
            .chain(
                scanned
                    .iter()
                    .chain(&self.config)
                    .map(|(k, v)| (k.as_os_str(), v.as_os_str())),
            );
        for (i, (key, value)) in pairs.enumerate() {
            cmd.env(format!("GIT_CONFIG_KEY_{i}"), key);
            cmd.env(format!("GIT_CONFIG_VALUE_{i}"), value);
        }
        for (key, value) in &self.env {
            cmd.env(key, value);
        }
        cmd.arg("--no-pager");
        if let Some(dir) = &self.git_dir {
            cmd.arg("--git-dir").arg(dir);
        }
        if let Some(dir) = &self.work_tree {
            cmd.arg("--work-tree").arg(dir);
        }
        cmd.args(&self.args);
        let cwd = self
            .cwd
            .as_deref()
            .or(self.work_tree.as_deref())
            .or(self.git_dir.as_deref())
            .unwrap_or(Path::new("/"));
        cmd.current_dir(cwd);
        cmd
    }

    fn scan(&self) -> Result<Vec<(OsString, OsString)>, GitError> {
        if !self.scan_config {
            return Ok(Vec::new());
        }
        let mut scan = GitCommand::new("config-scan")
            .without_config_scan()
            .read_only()
            .args(["config", "--list", "-z", "--name-only"])
            .stdout_limit(CONFIG_SCAN_LIMIT);
        scan.git_dir.clone_from(&self.git_dir);
        scan.work_tree.clone_from(&self.work_tree);
        scan.cwd.clone_from(&self.cwd);
        Ok(untrusted_overrides(&scan.run()?))
    }

    fn spawn(self) -> Result<Running, GitError> {
        let op = self.op;
        let scanned = self.scan()?;
        let mut cmd = self.build(&scanned);
        cmd.stdin(if self.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| GitError::Spawn { op, kind: e.kind() })?;
        let stdin_writer = match (self.stdin, child.stdin.take()) {
            (Some(bytes), Some(mut pipe)) => Some(thread::spawn(move || {
                let _ = pipe.write_all(&bytes);
            })),
            _ => None,
        };
        let stderr = child.stderr.take().map(|mut pipe| {
            thread::spawn(move || {
                let mut kept = Vec::new();
                let mut truncated = false;
                let mut buf = [0u8; 4096];
                loop {
                    match pipe.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let room = STDERR_LIMIT.saturating_sub(kept.len());
                            kept.extend_from_slice(&buf[..n.min(room)]);
                            truncated |= n > room;
                        }
                    }
                }
                Diagnostics {
                    bytes: kept,
                    truncated,
                }
            })
        });
        let stdout = child.stdout.take().ok_or(GitError::Io {
            op,
            kind: io::ErrorKind::BrokenPipe,
        })?;
        Ok(Running {
            op,
            child,
            stdout,
            stderr,
            stdin_writer,
            stdout_limit: self.stdout_limit,
        })
    }

    pub(crate) fn output(self) -> Result<GitOutput, GitError> {
        let mut running = self.spawn()?;
        let mut stdout = Vec::new();
        let limit = running.stdout_limit;
        let read = (&mut running.stdout)
            .take(limit as u64 + 1)
            .read_to_end(&mut stdout);
        if let Err(e) = read {
            let op = running.op;
            running.abort();
            return Err(GitError::Io { op, kind: e.kind() });
        }
        if stdout.len() > limit {
            let op = running.op;
            running.abort();
            return Err(GitError::OutputLimit { op, limit });
        }
        let (status, stderr) = running.finish()?;
        Ok(GitOutput {
            status,
            stdout,
            stderr,
        })
    }

    pub(crate) fn run(self) -> Result<Vec<u8>, GitError> {
        let op = self.op;
        let out = self.output()?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(GitError::Failed {
                op,
                exit_code: out.status.code(),
                stderr: out.stderr,
            })
        }
    }

    pub(crate) fn stream(self) -> Result<GitStream, GitError> {
        Ok(GitStream {
            running: self.spawn()?,
        })
    }
}

pub(crate) fn untrusted_overrides(names: &[u8]) -> Vec<(OsString, OsString)> {
    let mut filters = BTreeSet::new();
    let mut merges = BTreeSet::new();
    let mut hooks = BTreeSet::new();
    for name in names.split(|b| *b == 0).filter(|n| !n.is_empty()) {
        let split = |rest: &[u8]| {
            rest.iter()
                .rposition(|b| *b == b'.')
                .map(|dot| (rest[..dot].to_vec(), rest[dot + 1..].to_ascii_lowercase()))
        };
        let section = |prefix: &[u8]| {
            (name.len() > prefix.len() && name[..prefix.len()].eq_ignore_ascii_case(prefix))
                .then(|| &name[prefix.len()..])
                .and_then(split)
        };
        if let Some((sub, key)) = section(b"filter.")
            && matches!(&key[..], b"clean" | b"smudge" | b"process" | b"required")
        {
            filters.insert(sub);
        } else if let Some((sub, key)) = section(b"merge.")
            && matches!(&key[..], b"driver" | b"recursive")
        {
            merges.insert(sub);
        } else if let Some((sub, _)) = section(b"hook.") {
            hooks.insert(sub);
        }
    }
    let key = |section: &[u8], sub: &[u8], name: &[u8]| {
        OsString::from_vec([section, b".", sub, b".", name].concat())
    };
    let mut pairs = Vec::new();
    for sub in &filters {
        for (name, value) in [
            (&b"clean"[..], ""),
            (b"smudge", ""),
            (b"process", ""),
            (b"required", "false"),
        ] {
            pairs.push((key(b"filter", sub, name), OsString::from(value)));
        }
    }
    for sub in &merges {
        pairs.push((key(b"merge", sub, b"driver"), OsString::from("false")));
        pairs.push((key(b"merge", sub, b"recursive"), OsString::from("binary")));
    }
    for sub in &hooks {
        for (name, value) in [(&b"command"[..], ""), (b"event", ""), (b"enabled", "false")] {
            pairs.push((key(b"hook", sub, name), OsString::from(value)));
        }
    }
    pairs
}

struct Running {
    op: &'static str,
    child: Child,
    stdout: ChildStdout,
    stderr: Option<JoinHandle<Diagnostics>>,
    stdin_writer: Option<JoinHandle<()>>,
    stdout_limit: usize,
}

impl Running {
    fn finish(mut self) -> Result<(ExitStatus, Diagnostics), GitError> {
        let status = self.child.wait().map_err(|e| GitError::Io {
            op: self.op,
            kind: e.kind(),
        })?;
        Ok((status, self.join()))
    }

    fn join(&mut self) -> Diagnostics {
        if let Some(writer) = self.stdin_writer.take() {
            let _ = writer.join();
        }
        self.stderr
            .take()
            .and_then(|h| h.join().ok())
            .unwrap_or_default()
    }

    fn abort(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.join();
    }
}

pub(crate) struct GitStream {
    running: Running,
}

impl GitStream {
    pub(crate) fn stdout(&mut self) -> &mut ChildStdout {
        &mut self.running.stdout
    }

    pub(crate) fn finish(mut self) -> Result<(ExitStatus, Diagnostics), GitError> {
        let _ = io::copy(&mut self.running.stdout, &mut io::sink());
        self.running.finish()
    }

    pub(crate) fn abort(self) {
        self.running.abort();
    }

    pub(crate) fn op(&self) -> &'static str {
        self.running.op
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(names: &[u8]) -> Vec<(String, String)> {
        untrusted_overrides(names)
            .into_iter()
            .map(|(k, v)| {
                (
                    String::from_utf8_lossy(k.as_encoded_bytes()).into_owned(),
                    v.into_string().unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn configuration_hooks_filters_and_merge_drivers_are_overridden() {
        let names = b"core.bare\0hook.evil.command\0hook.evil.event\0hook.a.b.enabled\0\
            filter.lfs.clean\0filter.lfs.smudge\0merge.ours.driver\0hookx.y.command\0hook.command\0";
        let got = pairs(names);
        let has = |k: &str, v: &str| got.iter().any(|(gk, gv)| gk == k && gv == v);
        for name in ["evil", "a.b"] {
            assert!(has(&format!("hook.{name}.command"), ""), "{got:?}");
            assert!(has(&format!("hook.{name}.event"), ""), "{got:?}");
            assert!(has(&format!("hook.{name}.enabled"), "false"), "{got:?}");
        }
        assert!(has("filter.lfs.clean", "") && has("filter.lfs.required", "false"));
        assert!(has("merge.ours.driver", "false"));
        assert!(
            !got.iter()
                .any(|(k, _)| k.starts_with("hookx.") || k == "hook.command")
        );
        assert_eq!(got.len(), 12, "{got:?}");
    }

    #[test]
    fn subsection_bytes_are_kept_exactly() {
        let got = untrusted_overrides(b"hook.caf\xe9 X.command\0");
        assert_eq!(got[0].0.as_encoded_bytes(), b"hook.caf\xe9 X.command");
    }

    #[test]
    fn invocations_emit_the_scanned_overrides() {
        let dir = std::env::temp_dir().join(format!(
            "lade-command-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let setup = |args: &[&str]| {
            let status = GitCommand::new("test-setup")
                .without_config_scan()
                .cwd(&dir)
                .args(args)
                .output()
                .unwrap()
                .status;
            assert!(status.success(), "{args:?}");
        };
        setup(&["init", "-q"]);
        setup(&["config", "hook.marker.command", "touch /nonexistent/marker"]);
        setup(&["config", "hook.marker.event", "pre-commit"]);
        let cmd = GitCommand::new("rev-parse")
            .git_dir(&dir.join(".git"))
            .work_tree(&dir)
            .args(["rev-parse", "--git-dir"]);
        let scanned = cmd.scan().unwrap();
        let built = cmd.build(&scanned);
        let env: Vec<(String, String)> = built
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_str()?.to_owned(), v?.to_str()?.to_owned())))
            .collect();
        let value_of = |key: &str| {
            env.iter()
                .find(|(k, v)| k.starts_with("GIT_CONFIG_KEY_") && v == key)
                .and_then(|(k, _)| {
                    let index = k.trim_start_matches("GIT_CONFIG_KEY_");
                    env.iter()
                        .find(|(vk, _)| *vk == format!("GIT_CONFIG_VALUE_{index}"))
                        .map(|(_, v)| v.clone())
                })
        };
        assert_eq!(value_of("hook.marker.command").as_deref(), Some(""));
        assert_eq!(value_of("hook.marker.event").as_deref(), Some(""));
        assert_eq!(value_of("hook.marker.enabled").as_deref(), Some("false"));
        assert_eq!(value_of("core.hooksPath").as_deref(), Some("/dev/null"));
        let count: usize = env
            .iter()
            .find(|(k, _)| k == "GIT_CONFIG_COUNT")
            .map(|(_, v)| v.parse().unwrap())
            .unwrap();
        assert_eq!(count, HARDENED_CONFIG.len() + scanned.len());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
