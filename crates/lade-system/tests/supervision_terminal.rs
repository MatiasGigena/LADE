//! Contained terminal (IMP-012): a real PTY owned by the test (as by the service) with the shell
//! in the real bubblewrap sandbox. Requires bubblewrap/prlimit and a working sandbox; each test
//! prints a skip notice otherwise.

mod adapters_support;

use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use adapters_support::TempDir;
use lade_system::adapters::sandbox::{SandboxTools, sandbox_available};
use lade_system::supervision::terminal::{RING_BYTES, TERMINAL_LIMITS, Terminal, TerminalRequest};

const TIMEOUT: Duration = Duration::from_secs(30);

struct Fixture {
    root: TempDir,
    tools: SandboxTools,
}

impl Fixture {
    fn new(tag: &str) -> Option<Self> {
        let tools = SandboxTools::locate().ok()?;
        if let Err(e) = sandbox_available(&tools) {
            eprintln!("skipping terminal test: sandbox unavailable ({e})");
            return None;
        }
        let root = TempDir::new(tag);
        for dir in ["worktree", "common", "run"] {
            fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        Some(Self { root, tools })
    }

    fn worktree(&self) -> PathBuf {
        self.root.path().join("worktree")
    }

    fn open(&self, writable: bool) -> Terminal {
        Terminal::open(
            &self.tools,
            &TerminalRequest {
                objective_id: "0b8a4c1e-7a1f-4f7e-9d7a-0d6c8f2a9b11".into(),
                terminal_id: format!("t{}", std::process::id()),
                worktree: self.worktree(),
                repo_common_dir: self.root.path().join("common"),
                writable,
                cols: 80,
                rows: 24,
                run_dir: self.root.path().join("run"),
                limits: TERMINAL_LIMITS,
                cgroups: None,
            },
        )
        .expect("terminal opens")
    }
}

/// Drains output until it contains `needle` (the echoed command line never does: tests print
/// computed markers such as `M_$((40+2))`).
fn read_until(terminal: &Terminal, seen: &mut String, needle: &str) {
    let deadline = Instant::now() + TIMEOUT;
    while !seen.contains(needle) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {needle:?}; output so far: {seen:?}"
        );
        let drained = terminal.drain(64 << 10);
        seen.push_str(&String::from_utf8_lossy(&drained.bytes));
        thread::sleep(Duration::from_millis(20));
    }
}

fn run(terminal: &Terminal, line: &str, needle: &str) -> String {
    let mut seen = String::new();
    terminal.write(format!("{line}\n").as_bytes()).unwrap();
    read_until(terminal, &mut seen, needle);
    seen
}

/// Live, non-zombie processes running exactly `sleep <marker>`.
fn sleepers(marker: &str) -> usize {
    let expected = format!("sleep\0{marker}\0").into_bytes();
    fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            let live = fs::read_to_string(format!("/proc/{pid}/stat"))
                .is_ok_and(|s| s.rsplit(") ").next().is_some_and(|r| !r.starts_with('Z')));
            live && fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c == "sleep\n")
                && fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|c| c.ends_with(&expected))
        })
        .count()
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    condition()
}

#[test]
fn shell_runs_on_a_pty_inside_the_sandbox_with_worker_quotas() {
    let Some(fx) = Fixture::new("term-pty") else {
        return;
    };
    let terminal = fx.open(true);
    run(
        &terminal,
        "[ -t 0 ] && [ -t 1 ] && echo TTY_$((40+2))",
        "TTY_42",
    );
    // Quotas: the prlimit values of the worker limits apply to the shell and its children.
    run(
        &terminal,
        "echo L_$(ulimit -n)_$(ulimit -u)_$(( $(ulimit -v) / 1048576 ))G",
        "L_1024_512_4G",
    );
    // Grants: no credentials, no network interfaces besides loopback, the Objective worktree as
    // the working directory, LADE's terminal environment only.
    let out = run(
        &terminal,
        "echo C_${ANTHROPIC_API_KEY:-none}_$(ls /sys/class/net 2>/dev/null | tr -d '\\n')_${LADE_TERMINAL}_${HOME}_$(pwd | sed 's,.*/,,')",
        "C_none_",
    );
    assert!(
        out.contains("C_none_lo_read_write_/tmp_worktree")
            || out.contains("C_none__read_write_/tmp_worktree"),
        "{out:?}"
    );
    // Job control: the PTY is the shell's controlling terminal.
    run(
        &terminal,
        "sleep 30 & kill %1 && echo JOB_$((1+1))",
        "JOB_2",
    );
    // Writes land in the worktree.
    run(&terminal, "echo hello > made.txt && echo W_$((2+3))", "W_5");
    assert_eq!(
        fs::read_to_string(fx.worktree().join("made.txt")).unwrap(),
        "hello\n"
    );
    // Outside the binds nothing is visible.
    let host = fx.root.path().display().to_string();
    run(
        &terminal,
        &format!("ls {host}/run >/dev/null 2>&1 || echo HIDDEN_$((3+4))"),
        "HIDDEN_7",
    );
}

#[test]
fn seccomp_refuses_terminal_input_injection_and_pwd_is_the_shells_own() {
    let Some(fx) = Fixture::new("term-seccomp") else {
        return;
    };
    if !std::path::Path::new("/usr/bin/python3").is_file() {
        eprintln!("skipping TIOCSTI test: /usr/bin/python3 not installed");
        return;
    }
    let terminal = fx.open(true);
    // TIOCSTI on the shell's own PTY: EPERM (1) from the filter; without it the kernel either
    // injects (success) or, with dev.tty.legacy_tiocsti=0, refuses with EIO (5).
    let out = run(
        &terminal,
        "python3 -I -c 'import fcntl, termios\ntry:\n    fcntl.ioctl(0, termios.TIOCSTI, b\" \"); print(\"TI_\" + \"ok\")\nexcept OSError as e:\n    print(\"TI_%d_\" % e.errno)'",
        "TI_",
    );
    let deadline = Instant::now() + TIMEOUT;
    let mut seen = out;
    while !seen.contains("TI_1_") && !seen.contains("TI_ok") && !seen.contains("TI_5_") {
        assert!(Instant::now() < deadline, "{seen:?}");
        seen.push_str(&String::from_utf8_lossy(&terminal.drain(64 << 10).bytes));
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        seen.contains("TI_1_"),
        "TIOCSTI was not refused with EPERM: {seen:?}"
    );
    // The shell sets its own PWD; bwrap's export was removed before it started.
    run(
        &terminal,
        "echo P_$((6*7))_$(pwd | sed 's,.*/,,')",
        "P_42_worktree",
    );
}

/// The prompt names LADE and the terminal's mode. `PS1` must reach the interactive shell even
/// though the sandbox's launch gate is a non-interactive bash, which drops an inherited `PS1`.
#[test]
fn prompt_names_lade_and_the_terminal_mode() {
    for (tag, writable, prompt) in [
        ("term-prompt-rw", true, "[lade] worktree$ "),
        ("term-prompt-ro", false, "[lade read-only] worktree$ "),
    ] {
        let Some(fx) = Fixture::new(tag) else {
            return;
        };
        let terminal = fx.open(writable);
        let mut seen = String::new();
        read_until(&terminal, &mut seen, prompt);
        assert!(!seen.contains("bash-"), "{seen:?}");
    }
}

#[test]
fn read_only_terminal_cannot_write_the_worktree() {
    let Some(fx) = Fixture::new("term-ro") else {
        return;
    };
    fs::write(fx.worktree().join("frozen.txt"), "reviewed\n").unwrap();
    let terminal = fx.open(false);
    let out = run(
        &terminal,
        "echo changed > frozen.txt 2>/dev/null; touch new.txt 2>/dev/null; echo RO_${LADE_TERMINAL}_$((5+5))",
        "RO_read_only_10",
    );
    assert!(!out.contains("RO_read_write"), "{out:?}");
    assert_eq!(
        fs::read_to_string(fx.worktree().join("frozen.txt")).unwrap(),
        "reviewed\n"
    );
    assert!(!fx.worktree().join("new.txt").exists());
}

#[test]
fn kill_ends_detached_descendants_and_removes_the_pidfile() {
    let Some(fx) = Fixture::new("term-kill") else {
        return;
    };
    let mut terminal = fx.open(true);
    let marker = format!("{}.{}", 7000 + std::process::id() % 1000, 31);
    // A setsid'ed, double-forked daemon that would outlive a plain process-group kill.
    run(
        &terminal,
        &format!("(setsid nohup sleep {marker} >/dev/null 2>&1 &) ; echo D_$((6+6))"),
        "D_12",
    );
    assert!(
        wait_until(Duration::from_secs(5), || sleepers(&marker) == 1),
        "daemon started"
    );
    let pidfile = terminal.pidfile().to_path_buf();
    assert!(pidfile.exists(), "pidfile recorded for the startup sweep");
    assert!(terminal.namespace_init().is_some());
    terminal.kill();
    assert!(
        wait_until(Duration::from_secs(5), || sleepers(&marker) == 0),
        "detached descendant survived the terminal"
    );
    assert!(!pidfile.exists());
    assert!(terminal.exit().is_some());
    assert!(
        terminal.write(b"echo\n").is_err(),
        "a killed terminal takes no input"
    );
}

#[test]
fn shell_exit_is_observed_with_its_status() {
    let Some(fx) = Fixture::new("term-exit") else {
        return;
    };
    let terminal = fx.open(true);
    run(&terminal, "echo READY_$((1+2))", "READY_3");
    terminal.write(b"exit 3\n").unwrap();
    let exit = (0..1500)
        .find_map(|_| {
            let _ = terminal.drain(64 << 10);
            let finished = terminal.finished();
            if finished.is_none() {
                thread::sleep(Duration::from_millis(20));
            }
            finished
        })
        .expect("terminal finished");
    assert_eq!(exit.code, Some(3));
}

#[test]
fn resize_reaches_the_shell() {
    let Some(fx) = Fixture::new("term-resize") else {
        return;
    };
    let terminal = fx.open(true);
    run(&terminal, "echo S_$(stty size | tr ' ' x)", "S_24x80");
    terminal.resize(132, 40).unwrap();
    run(&terminal, "echo S_$(stty size | tr ' ' x)", "S_40x132");
    assert!(terminal.resize(0, 40).is_err());
    assert!(terminal.resize(80, 10_000).is_err());
}

#[test]
fn output_flood_stays_bounded_and_counts_dropped_bytes() {
    let Some(fx) = Fixture::new("term-flood") else {
        return;
    };
    let terminal = fx.open(true);
    run(&terminal, "echo GO_$((8+8))", "GO_16");
    terminal
        .write(b"stty -echo; tr '\\0' x < /dev/zero\n")
        .unwrap();
    // Nobody drains for a while: the ring fills, then discards the oldest bytes.
    let mut total = 0usize;
    let mut dropped = 0u64;
    let deadline = Instant::now() + TIMEOUT;
    while dropped == 0 {
        assert!(Instant::now() < deadline, "flood never overflowed the ring");
        thread::sleep(Duration::from_millis(500));
        let drained = terminal.drain(usize::MAX);
        assert!(drained.bytes.len() <= RING_BYTES, "ring is bounded");
        total += drained.bytes.len();
        dropped += drained.dropped;
    }
    assert!(total > 0);
    // ^C reaches the foreground job through the controlling terminal; the shell survives.
    terminal.write(b"\x03").unwrap();
    let mut seen = String::new();
    terminal.write(b"stty echo; echo AFTER_$((9+9))\n").unwrap();
    read_until(&terminal, &mut seen, "AFTER_18");
}
