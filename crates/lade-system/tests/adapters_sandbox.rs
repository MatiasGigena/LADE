//! Sandbox integration tests. They require bubblewrap and prlimit; when `bwrap` is missing each
//! test prints a skip notice and returns.

mod adapters_support;

use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use adapters_support::TempDir;
use lade_system::adapters::sandbox::{
    Limits, RunOutcome, SandboxSpec, SandboxTools, VerificationLaunch, WorkerLaunch, run_bounded,
    sandbox_available, verification_spec, worker_spec,
};

fn tools() -> Option<SandboxTools> {
    match SandboxTools::locate() {
        Ok(tools) => Some(tools),
        Err(e) => {
            eprintln!("skipping sandbox test: {e} (bubblewrap/prlimit not installed)");
            None
        }
    }
}

fn sh_spec(script: &str, rw: &Path) -> SandboxSpec {
    let mut spec = verification_spec(&VerificationLaunch {
        work_dir: rw.to_path_buf(),
        argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
        extra_ro: Vec::new(),
        env: Vec::new(),
        limits: Limits::VERIFICATION,
    })
    .unwrap();
    spec.env("RW", rw).unwrap();
    spec
}

fn run(tools: &SandboxTools, spec: &SandboxSpec) -> RunOutcome {
    run_bounded(spec.command(tools), Duration::from_secs(30), 64 << 10).unwrap()
}

/// Live host processes running exactly `sleep <marker>` (not shells or bwrap whose argv merely
/// mentions it). Zombies are excluded: they have exited and hold no resources but a pid.
fn processes_with(marker: &str) -> Vec<u32> {
    let expected = format!("sleep\0{marker}\0").into_bytes();
    fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            let running = fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
                s.rsplit(") ")
                    .next()
                    .is_some_and(|rest| !rest.starts_with('Z'))
            });
            running
                && fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|c| c.ends_with(&expected))
        })
        .collect()
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    condition()
}

#[test]
fn probe_succeeds() {
    let Some(tools) = tools() else { return };
    sandbox_available(&tools).expect("sandbox available");
}

#[test]
fn host_files_outside_binds_are_invisible() {
    let Some(tools) = tools() else { return };
    let tmp = TempDir::new("sbx-visibility");
    let rw = tmp.path().join("rw");
    fs::create_dir(&rw).unwrap();
    let secret = tmp.path().join("secret.txt");
    fs::write(&secret, "host secret").unwrap();
    let script = format!(
        "test -e /root && echo ROOT_VISIBLE; test -e {} && echo SECRET_VISIBLE; \
         test -e /home && echo HOME_VISIBLE; ls /usr/bin/sh >/dev/null && echo USR_OK",
        secret.display()
    );
    let outcome = run(&tools, &sh_spec(&script, &rw));
    let out = String::from_utf8_lossy(&outcome.output_tail);
    assert!(out.contains("USR_OK"), "{out}");
    assert!(!out.contains("VISIBLE"), "{out}");
}

#[test]
fn environment_is_cleared_to_the_allowlist() {
    let Some(tools) = tools() else { return };
    let tmp = TempDir::new("sbx-env");
    let mut spec =
        SandboxSpec::new(vec!["/usr/bin/env".into()], tmp.path(), Limits::WORKER).unwrap();
    spec.rw_bind(tmp.path()).unwrap();
    spec.env("ONLY_THIS", "1").unwrap();
    assert!(spec.env("ONLY_THIS", "2").is_err());
    assert!(spec.env("BAD-NAME", "x").is_err());
    let outcome = run(&tools, &spec);
    assert_eq!(outcome.exit_code, Some(0));
    // bwrap itself exports PWD for --chdir; nothing else is inherited.
    assert_eq!(
        String::from_utf8_lossy(&outcome.output_tail),
        format!("ONLY_THIS=1\nPWD={}\n", tmp.path().display())
    );
}

#[test]
fn network_is_unreachable() {
    let Some(tools) = tools() else { return };
    let tmp = TempDir::new("sbx-net");
    let outcome = run(
        &tools,
        &sh_spec(
            "if command -v bash >/dev/null; then \
               bash -c 'exec 3<>/dev/tcp/1.1.1.1/80' && echo CONNECTED; \
             fi; cat /proc/net/route | wc -l",
            tmp.path(),
        ),
    );
    let out = String::from_utf8_lossy(&outcome.output_tail);
    assert!(!outcome.timed_out);
    assert!(!out.contains("CONNECTED"), "{out}");
    // Only the header line: no routes in the private network namespace.
    assert!(out.trim_end().ends_with('1'), "{out}");
}

#[test]
fn writes_outside_the_writable_bind_fail() {
    let Some(tools) = tools() else { return };
    let tmp = TempDir::new("sbx-write");
    let rw = tmp.path().join("rw");
    let ro = tmp.path().join("ro");
    fs::create_dir(&rw).unwrap();
    fs::create_dir(&ro).unwrap();
    let unbound = tmp.path().join("unbound");
    fs::create_dir(&unbound).unwrap();
    let mut spec = sh_spec(
        &format!(
            "echo ok > \"$RW/inside\" && echo RW_OK; \
             echo x > {ro}/f && echo RO_WRITTEN; \
             echo x > {unbound}/f && echo UNBOUND_WRITTEN; \
             echo x > /usr/lade-test && echo USR_WRITTEN; \
             echo x > /lade-test && echo ROOT_WRITTEN; \
             echo x > /tmp/private && echo TMP_OK",
            ro = ro.display(),
            unbound = unbound.display()
        ),
        &rw,
    );
    spec.ro_bind(&ro).unwrap();
    let outcome = run(&tools, &spec);
    let out = String::from_utf8_lossy(&outcome.output_tail);
    assert!(out.contains("RW_OK") && out.contains("TMP_OK"), "{out}");
    assert!(!out.contains("WRITTEN"), "{out}");
    assert_eq!(fs::read(rw.join("inside")).unwrap(), b"ok\n");
    assert!(!ro.join("f").exists() && !unbound.join("f").exists());
    assert!(!Path::new("/usr/lade-test").exists() && !Path::new("/lade-test").exists());
}

#[test]
fn file_size_limit_is_enforced() {
    let Some(tools) = tools() else { return };
    let tmp = TempDir::new("sbx-fsize");
    let mut spec = SandboxSpec::new(
        vec![
            "/bin/sh".into(),
            "-c".into(),
            "head -c 3000000 /dev/zero > big; echo status=$?".into(),
        ],
        tmp.path(),
        Limits {
            file_size_bytes: 1 << 20,
            ..Limits::VERIFICATION
        },
    )
    .unwrap();
    spec.rw_bind(tmp.path()).unwrap();
    let outcome = run(&tools, &spec);
    assert!(!String::from_utf8_lossy(&outcome.output_tail).contains("status=0"));
    assert!(fs::metadata(tmp.path().join("big")).unwrap().len() <= 1 << 20);
}

#[test]
fn timeout_kills_the_sandbox_and_detached_descendants() {
    let Some(tools) = tools() else { return };
    let tmp = TempDir::new("sbx-timeout");
    let marker = "4242.0517";
    let spec = sh_spec(
        &format!(
            "setsid nohup sleep {marker} >/dev/null 2>&1 & \
             (sleep {marker} &) ; echo started; sleep 1000"
        ),
        tmp.path(),
    );
    let started = Instant::now();
    let outcome = run_bounded(spec.command(&tools), Duration::from_secs(2), 4096).unwrap();
    assert!(outcome.timed_out);
    assert!(started.elapsed() < Duration::from_secs(15));
    assert!(String::from_utf8_lossy(&outcome.output_tail).contains("started"));
    assert_eq!(outcome.signal, Some(9));
    assert!(
        wait_until(Duration::from_secs(5), || processes_with(marker).is_empty()),
        "descendants survived: {:?}",
        processes_with(marker)
    );
}

#[test]
fn killing_the_sandbox_or_its_supervisor_kills_descendants() {
    let Some(tools) = tools() else { return };
    let tmp = TempDir::new("sbx-kill");
    let marker = "4343.0611";
    let script = format!("setsid nohup sleep {marker} >/dev/null 2>&1 & sleep 1000");
    let mut child = sh_spec(&script, tmp.path())
        .command(&tools)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert!(wait_until(Duration::from_secs(10), || !processes_with(
        marker
    )
    .is_empty()));
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        wait_until(Duration::from_secs(5), || processes_with(marker).is_empty()),
        "descendants survived bwrap kill"
    );

    // Supervisor loss: the sandbox's parent dies without cleaning up (--die-with-parent).
    let marker = "4444.0723";
    let script = format!("setsid nohup sleep {marker} >/dev/null 2>&1 & sleep 1000");
    let cmd = sh_spec(&script, tmp.path()).command(&tools);
    let mut line = shell_quote(cmd.get_program().as_encoded_bytes());
    for arg in cmd.get_args() {
        line.push(' ');
        line.push_str(&shell_quote(arg.as_encoded_bytes()));
    }
    let mut supervisor = Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("{line} & wait"))
        .env_clear()
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert!(wait_until(Duration::from_secs(10), || !processes_with(
        marker
    )
    .is_empty()));
    supervisor.kill().unwrap();
    supervisor.wait().unwrap();
    assert!(
        wait_until(Duration::from_secs(5), || processes_with(marker).is_empty()),
        "descendants survived supervisor loss"
    );
}

fn shell_quote(bytes: &[u8]) -> String {
    let text = String::from_utf8(bytes.to_vec()).expect("utf-8 argument");
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[test]
fn output_is_bounded_to_the_tail() {
    let Some(tools) = tools() else { return };
    let tmp = TempDir::new("sbx-output");
    let spec = sh_spec(
        "head -c 100000 /dev/zero | tr '\\0' a; printf END",
        tmp.path(),
    );
    let outcome = run_bounded(spec.command(&tools), Duration::from_secs(30), 1000).unwrap();
    assert_eq!(outcome.exit_code, Some(0));
    assert!(!outcome.timed_out);
    assert!(outcome.truncated);
    assert_eq!(outcome.total_bytes, 100_003);
    assert_eq!(outcome.output_tail.len(), 1000);
    assert!(outcome.output_tail.ends_with(b"aaEND"));
}

#[test]
fn run_bounded_reports_exit_status_without_sandbox() {
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "echo out; echo err >&2; exit 3"]);
    let outcome = run_bounded(cmd, Duration::from_secs(10), 1024).unwrap();
    assert_eq!(outcome.exit_code, Some(3));
    assert_eq!(outcome.signal, None);
    assert!(!outcome.truncated);
    assert_eq!(outcome.output_tail, b"out\nerr\n");
}

fn node() -> Option<PathBuf> {
    [
        "/opt/node22/bin/node",
        "/usr/bin/node",
        "/usr/local/bin/node",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.exists())
}

#[test]
fn worker_sandbox_runs_node_with_gateway_socket_only() {
    let Some(tools) = tools() else { return };
    let Some(node) = node() else {
        eprintln!("skipping worker sandbox test: node not installed");
        return;
    };
    let tmp = TempDir::new("sbx-worker");
    let dirs: Vec<PathBuf> = ["worker", "worktree", "repo-git", "session", "run"]
        .iter()
        .map(|d| tmp.path().join(d))
        .collect();
    for dir in &dirs {
        fs::create_dir(dir).unwrap();
    }
    let [worker, worktree, repo_git, session, run_dir] = &dirs[..] else {
        unreachable!()
    };
    fs::write(repo_git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    let socket = run_dir.join("gateway.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut received = String::new();
        stream.read_to_string(&mut received).unwrap();
        received
    });
    let entry = worker.join("worker.js");
    fs::write(
        &entry,
        r#"
const fs = require('node:fs');
const net = require('node:net');
const report = {
  home: process.env.HOME,
  term: process.env.TERM,
  pi: process.env.PI_CODING_AGENT_DIR,
  key: process.env.FIXTURE_API_KEY,
  envKeys: Object.keys(process.env).sort(),
  root: fs.existsSync('/root'),
  headReadable: fs.readFileSync(process.argv[2] + '/HEAD', 'utf8').startsWith('ref:'),
};
try { fs.writeFileSync(process.argv[2] + '/HEAD', 'x'); report.repoWritable = true; } catch { report.repoWritable = false; }
try { fs.writeFileSync('./edited.txt', 'worker edit'); report.worktreeWritable = true; } catch { report.worktreeWritable = false; }
const sock = net.connect(process.env.LADE_GATEWAY_SOCKET, () => sock.end('hello gateway'));
sock.on('close', () => console.log(JSON.stringify(report)));
"#,
    )
    .unwrap();
    let spec = worker_spec(&WorkerLaunch {
        node,
        worker_dir: worker.clone(),
        entry,
        node_args: Vec::new(),
        args: vec![OsString::from(repo_git)],
        worktree: worktree.clone(),
        repo_common_dir: repo_git.clone(),
        session_dir: session.clone(),
        gateway_socket: socket.clone(),
        pi_env: vec![("PI_CODING_AGENT_DIR".into(), session.clone().into())],
        credentials: vec![("FIXTURE_API_KEY".into(), "not-a-real-key".into())],
        extra_ro: Vec::new(),
        limits: Limits::WORKER,
    })
    .unwrap();
    assert!(
        worker_spec(&WorkerLaunch {
            node: PathBuf::from("/opt/node22/bin/node"),
            worker_dir: worker.clone(),
            entry: worker.join("worker.js"),
            node_args: Vec::new(),
            args: Vec::new(),
            worktree: worktree.clone(),
            repo_common_dir: repo_git.clone(),
            session_dir: session.clone(),
            gateway_socket: socket.clone(),
            pi_env: vec![("NOT_PI".into(), "x".into())],
            credentials: Vec::new(),
            extra_ro: Vec::new(),
            limits: Limits::WORKER,
        })
        .is_err()
    );
    let outcome = run_bounded(spec.command(&tools), Duration::from_secs(60), 64 << 10).unwrap();
    let out = String::from_utf8_lossy(&outcome.output_tail).into_owned();
    assert_eq!(outcome.exit_code, Some(0), "{out}");
    assert_eq!(server.join().unwrap(), "hello gateway");
    let session_text = session.display().to_string();
    assert!(
        out.contains(&format!("\"home\":\"{session_text}\"")),
        "{out}"
    );
    assert!(out.contains("\"term\":\"dumb\""), "{out}");
    assert!(out.contains("\"key\":\"not-a-real-key\""), "{out}");
    assert!(out.contains("\"root\":false"), "{out}");
    assert!(out.contains("\"headReadable\":true"), "{out}");
    assert!(out.contains("\"repoWritable\":false"), "{out}");
    assert!(out.contains("\"worktreeWritable\":true"), "{out}");
    assert!(
        out.contains(
            "\"envKeys\":[\"FIXTURE_API_KEY\",\"HOME\",\"LADE_GATEWAY_SOCKET\",\"LANG\",\"PATH\",\"PI_CODING_AGENT_DIR\",\"PWD\",\"TERM\"]"
        ),
        "{out}"
    );
    assert_eq!(
        fs::read(worktree.join("edited.txt")).unwrap(),
        b"worker edit"
    );
    assert_eq!(
        fs::read(repo_git.join("HEAD")).unwrap(),
        b"ref: refs/heads/main\n"
    );
}
