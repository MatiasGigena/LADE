use std::ffi::OsString;
use std::fmt;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process_group};

use super::credentials::Secret;
use crate::adapters::sandbox::find_program;
use crate::support::process::wait_until;

pub const SERVICE: &str = "lade";
pub const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);
pub const STORE_TIMEOUT: Duration = Duration::from_secs(60);
pub const MAX_SECRET_BYTES: usize = 64 << 10;
const MAX_STDERR_BYTES: usize = 4 << 10;
const MAX_REASON_CHARS: usize = 200;
const PASSED_ENV: &[&str] = &[
    "HOME",
    "XDG_RUNTIME_DIR",
    "DBUS_SESSION_BUS_ADDRESS",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XAUTHORITY",
];

#[derive(Debug)]
pub enum Lookup {
    Found(Secret),
    NotFound,
    Unavailable(String),
}

#[derive(Clone)]
enum State {
    Disabled,
    Unavailable(String),
    Ready {
        program: PathBuf,
        env: Vec<(OsString, OsString)>,
    },
}

#[derive(Clone)]
pub struct SecretService {
    state: State,
    timeout: Duration,
}

impl fmt::Debug for SecretService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.state {
            State::Disabled => f.write_str("SecretService(disabled)"),
            State::Unavailable(reason) => write!(f, "SecretService(unavailable: {reason})"),
            State::Ready { program, env } => {
                let names: Vec<_> = env.iter().map(|(k, _)| k).collect();
                f.debug_struct("SecretService")
                    .field("program", program)
                    .field("env", &names)
                    .field("timeout", &self.timeout)
                    .finish()
            }
        }
    }
}

struct Ran {
    status: ExitStatus,
    stdout: Vec<u8>,
    stdout_overflow: bool,
    stderr: Vec<u8>,
}

#[derive(Clone, Copy)]
enum RunError {
    Spawn(io::ErrorKind),
    TimedOut,
}

impl SecretService {
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            state: State::Disabled,
            timeout: LOOKUP_TIMEOUT,
        }
    }

    #[must_use]
    pub fn new(program: PathBuf, env: Vec<(OsString, OsString)>) -> Self {
        Self {
            state: State::Ready { program, env },
            timeout: LOOKUP_TIMEOUT,
        }
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    #[must_use]
    pub fn from_env() -> Self {
        Self::detect(find_program("secret-tool"), |name| std::env::var_os(name))
    }

    pub fn detect(program: Option<PathBuf>, var: impl Fn(&str) -> Option<OsString>) -> Self {
        let unavailable = |reason: &str| Self {
            state: State::Unavailable(reason.to_owned()),
            timeout: LOOKUP_TIMEOUT,
        };
        let Some(program) = program else {
            return unavailable("secret-tool is not installed (package libsecret-tools)");
        };
        let mut env: Vec<(OsString, OsString)> = PASSED_ENV
            .iter()
            .filter_map(|name| Some((OsString::from(name), var(name)?)))
            .filter(|(_, value)| !value.is_empty())
            .collect();
        let has_bus = env.iter().any(|(k, _)| k == "DBUS_SESSION_BUS_ADDRESS");
        if !has_bus {
            let socket = var("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .filter(|dir| dir.is_absolute())
                .map(|dir| dir.join("bus"))
                .filter(|bus| bus.exists());
            let Some(socket) = socket else {
                return unavailable(
                    "no D-Bus session bus (DBUS_SESSION_BUS_ADDRESS is unset and there is no \
                     $XDG_RUNTIME_DIR/bus)",
                );
            };
            let mut address = OsString::from("unix:path=");
            address.push(socket.as_os_str());
            env.push(("DBUS_SESSION_BUS_ADDRESS".into(), address));
        }
        Self::new(program, env)
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        !matches!(self.state, State::Disabled)
    }

    #[must_use]
    pub fn unavailable_reason(&self) -> Option<&str> {
        match &self.state {
            State::Unavailable(reason) => Some(reason),
            _ => None,
        }
    }

    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    #[must_use]
    pub fn lookup(&self, provider: &str, variable: &str, deadline: Instant) -> Lookup {
        let args = [
            "lookup", "service", SERVICE, "provider", provider, "variable", variable,
        ];
        let ran = match self.run(&args, None, true, deadline) {
            Ok(ran) => ran,
            Err(e) => return Lookup::Unavailable(run_error(e, self.timeout)),
        };
        if !ran.status.success() {
            return match reason_of(&ran.stderr) {
                None => Lookup::NotFound,
                Some(reason) => Lookup::Unavailable(reason),
            };
        }
        if ran.stdout_overflow {
            return Lookup::Unavailable(format!(
                "the stored value is larger than {} KiB",
                MAX_SECRET_BYTES >> 10
            ));
        }
        let mut value = ran.stdout;
        if value.ends_with(b"\n") {
            value.pop();
        }
        if value.is_empty() {
            Lookup::NotFound
        } else {
            Lookup::Found(Secret::new(OsString::from_vec(value)))
        }
    }

    #[must_use]
    pub fn locked(&self, provider: &str, deadline: Instant) -> Option<String> {
        let args = ["search", "service", SERVICE, "provider", provider];
        let ran = self.run(&args, None, false, deadline).ok()?;
        String::from_utf8_lossy(&ran.stderr)
            .contains("locked")
            .then(|| "the keyring holding LADE's keys is locked; unlock it and try again".into())
    }

    /// Stores `secret` as `variable` of `provider` in the Secret Service.
    ///
    /// # Errors
    ///
    /// Returns a reason the developer can read when `secret-tool` cannot run, times out, or refuses
    /// the item (for example a locked keyring).
    pub fn store(&self, provider: &str, variable: &str, secret: &[u8]) -> Result<(), String> {
        let label = format!("LADE {provider} {variable}");
        let args = [
            "store", "--label", &label, "service", SERVICE, "provider", provider, "variable",
            variable,
        ];
        let deadline = Instant::now() + STORE_TIMEOUT;
        let ran = self
            .run(&args, Some(secret.to_vec()), false, deadline)
            .map_err(|e| run_error(e, STORE_TIMEOUT))?;
        if ran.status.success() {
            return Ok(());
        }
        Err(reason_of(&ran.stderr)
            .unwrap_or_else(|| format!("secret-tool store failed ({})", ran.status)))
    }

    /// Deletes the stored `variable` of `provider` (all of its variables when `None`);
    /// `false` when nothing was stored.
    ///
    /// # Errors
    ///
    /// Returns a reason the developer can read when `secret-tool` cannot run, times out, or reports an
    /// error.
    pub fn clear(&self, provider: &str, variable: Option<&str>) -> Result<bool, String> {
        let mut args = vec!["clear", "service", SERVICE, "provider", provider];
        if let Some(variable) = variable {
            args.extend(["variable", variable]);
        }
        let deadline = Instant::now() + STORE_TIMEOUT;
        let ran = self
            .run(&args, None, false, deadline)
            .map_err(|e| run_error(e, STORE_TIMEOUT))?;
        if ran.status.success() {
            return Ok(true);
        }
        match reason_of(&ran.stderr) {
            None => Ok(false),
            Some(reason) => Err(reason),
        }
    }

    fn run(
        &self,
        args: &[&str],
        input: Option<Vec<u8>>,
        capture: bool,
        deadline: Instant,
    ) -> Result<Ran, RunError> {
        let State::Ready { program, env } = &self.state else {
            return Err(RunError::Spawn(io::ErrorKind::NotFound));
        };
        let mut command = Command::new(program);
        command
            .env_clear()
            .envs(env.iter().map(|(k, v)| (k.as_os_str(), v.as_os_str())))
            .env("LC_ALL", "C")
            .args(args)
            .current_dir("/")
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(if capture {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = command.spawn().map_err(|e| RunError::Spawn(e.kind()))?;
        let writer = child.stdin.take().map(|mut stdin| {
            let input = input.unwrap_or_default();
            thread::spawn(move || {
                let _ = stdin.write_all(&input);
            })
        });
        let stdout = child
            .stdout
            .take()
            .map(|pipe| thread::spawn(move || read_capped(pipe, MAX_SECRET_BYTES)));
        let stderr = child
            .stderr
            .take()
            .map(|pipe| thread::spawn(move || read_capped(pipe, MAX_STDERR_BYTES)));
        let status = wait_or_kill(&mut child, deadline);
        if let Some(writer) = writer {
            let _ = writer.join();
        }
        let (stdout, stdout_overflow) = stdout.and_then(|t| t.join().ok()).unwrap_or_default();
        let (stderr, _) = stderr.and_then(|t| t.join().ok()).unwrap_or_default();
        let status = status.ok_or(RunError::TimedOut)?;
        Ok(Ran {
            status,
            stdout,
            stdout_overflow,
            stderr,
        })
    }
}

fn run_error(error: RunError, bound: Duration) -> String {
    match error {
        RunError::Spawn(kind) => format!("secret-tool could not be started ({kind})"),
        RunError::TimedOut => format!(
            "secret-tool did not answer within {} s (a locked keyring may be waiting for its \
             unlock prompt)",
            bound.as_secs_f32()
        ),
    }
}

/// Waits for `child` until `deadline`; a child still running then (or one whose status cannot
/// be read) is killed with its process group.
fn wait_or_kill(child: &mut Child, deadline: Instant) -> Option<ExitStatus> {
    if let Ok(Some(status)) = wait_until(child, deadline) {
        return Some(status);
    }
    if let Some(group) = i32::try_from(child.id()).ok().and_then(Pid::from_raw) {
        let _ = kill_process_group(group, Signal::KILL);
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

fn read_capped(mut from: impl Read, cap: usize) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut overflow = false;
    let mut buffer = [0u8; 4096];
    loop {
        match from.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                let room = cap.saturating_sub(kept.len());
                kept.extend_from_slice(&buffer[..n.min(room)]);
                overflow |= n > room;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    (kept, overflow)
}

fn reason_of(stderr: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stderr);
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = line.strip_prefix("secret-tool:").unwrap_or(line).trim();
    Some(
        line.chars()
            .map(|c| if c.is_control() { '?' } else { c })
            .take(MAX_REASON_CHARS)
            .collect(),
    )
}

#[must_use]
pub fn valid_provider_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_are_bounded_and_printable() {
        assert_eq!(reason_of(b""), None);
        assert_eq!(reason_of(b"\n  \n"), None);
        assert_eq!(
            reason_of(b"secret-tool: Cannot autolaunch D-Bus without X11 $DISPLAY\nmore"),
            Some("Cannot autolaunch D-Bus without X11 $DISPLAY".into())
        );
        let long = vec![b'x'; 1000];
        assert_eq!(reason_of(&long).unwrap().len(), MAX_REASON_CHARS);
        assert_eq!(reason_of(b"a\x1bb").unwrap(), "a?b");
    }

    #[test]
    fn capped_reads_drain_and_flag() {
        let (kept, overflow) = read_capped(&[7u8; 10_000][..], 100);
        assert_eq!((kept.len(), overflow), (100, true));
        let (kept, overflow) = read_capped(&b"abc"[..], 100);
        assert_eq!((kept, overflow), (b"abc".to_vec(), false));
    }

    #[test]
    fn detection_needs_the_program_and_a_bus() {
        let none = |_: &str| None::<OsString>;
        let missing = SecretService::detect(None, none);
        assert!(
            missing
                .unavailable_reason()
                .unwrap()
                .contains("libsecret-tools")
        );
        let no_bus = SecretService::detect(Some("/usr/bin/secret-tool".into()), none);
        assert!(no_bus.unavailable_reason().unwrap().contains("D-Bus"));
        let bus = SecretService::detect(Some("/usr/bin/secret-tool".into()), |name| {
            (name == "DBUS_SESSION_BUS_ADDRESS").then(|| OsString::from("unix:path=/x"))
        });
        assert!(bus.unavailable_reason().is_none() && bus.is_enabled());
        assert!(!SecretService::disabled().is_enabled());
        assert!(valid_provider_id("vercel-ai-gateway") && !valid_provider_id("a b"));
    }
}
