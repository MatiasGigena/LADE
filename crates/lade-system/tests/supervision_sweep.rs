mod adapters_support;

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use adapters_support::TempDir;
use lade_system::adapters::sandbox::{Limits, SandboxTools, VerificationLaunch, verification_spec};
use lade_system::supervision::sweep::{
    PidRecord, kill_marked, read_pidfile, sandbox_marker, sweep_stale, write_pidfile,
};

fn stat_fields(pid: u32) -> Option<Vec<String>> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    Some(rest.split_whitespace().map(str::to_owned).collect())
}

fn child_of(parent: u32) -> Option<u32> {
    fs::read_dir("/proc")
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .find(|pid| stat_fields(*pid).is_some_and(|f| f[1] == parent.to_string()))
}

fn sleepers(marker: &str) -> usize {
    let expected = format!("sleep\0{marker}\0").into_bytes();
    fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            stat_fields(*pid).is_some_and(|f| f[0] != "Z")
                && fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c == "sleep\n")
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
fn sweep_kills_recorded_sandbox_and_ignores_recycled_pid() {
    let Ok(tools) = SandboxTools::locate() else {
        eprintln!("skipping sweep test: bubblewrap/prlimit not installed");
        return;
    };
    let tmp = TempDir::new("sweep");
    let run = tmp.path().join("run");
    fs::create_dir(&run).unwrap();
    let marker = format!(
        "{}.{}",
        7000 + std::process::id() % 1000,
        std::process::id()
    );
    let spec = verification_spec(&VerificationLaunch {
        work_dir: tmp.path().to_path_buf(),
        argv: vec!["/bin/sleep".into(), marker.clone().into()],
        extra_ro: Vec::new(),
        env: Vec::new(),
        limits: Limits::VERIFICATION,
    })
    .unwrap();
    let mut cmd = spec.command(&tools).unwrap();
    cmd.command_mut().stdout(Stdio::null());
    let (mut sandbox, mut gate) = cmd.spawn().unwrap();
    gate.open().unwrap();
    let outer = sandbox.id();
    assert!(wait_until(Duration::from_secs(10), || sleepers(&marker) == 1));
    let init = child_of(outer).expect("namespace init");
    let records = [PidRecord::of(outer).unwrap(), PidRecord::of(init).unwrap()];
    let pidfile = run.join("worker-a-1.pid");
    write_pidfile(&pidfile, &records).unwrap();
    assert_eq!(read_pidfile(&pidfile).unwrap(), records);

    let mut bystander = Command::new("sleep").arg("30").spawn().unwrap();
    let real = PidRecord::of(bystander.id()).unwrap();
    write_pidfile(
        &run.join("worker-b-1.pid"),
        &[PidRecord {
            pid: real.pid,
            start_time: real.start_time + 1,
        }],
    )
    .unwrap();
    fs::write(run.join("gw-a-1.sock"), "").unwrap();
    fs::write(run.join("unrelated.txt"), "").unwrap();

    let report = sweep_stale(&run).unwrap();
    assert!(
        report.killed.contains(&outer) || report.killed.contains(&init),
        "{report:?}"
    );
    assert!(report.skipped.contains(&real.pid), "{report:?}");
    assert_eq!(report.removed_pidfiles, 2);
    assert_eq!(report.removed_sockets, 1);
    let _ = sandbox.wait();
    assert!(wait_until(Duration::from_secs(5), || sleepers(&marker) == 0));
    assert!(real.is_alive(), "bystander must survive");
    assert!(run.join("unrelated.txt").exists());
    assert!(!run.join("gw-a-1.sock").exists());
    let _ = bystander.kill();
    let _ = bystander.wait();

    assert_eq!(
        sweep_stale(Path::new("/nonexistent/lade-run")).unwrap(),
        Default::default()
    );
}

fn shell_line(cmd: &Command) -> String {
    let quote = |arg: &std::ffi::OsStr| {
        format!(
            "'{}'",
            arg.to_str().expect("utf-8 argument").replace('\'', "'\\''")
        )
    };
    std::iter::once(quote(cmd.get_program()))
        .chain(cmd.get_args().map(quote))
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn forced_supervisor_loss_races_leave_no_survivors() {
    const RUNS: usize = 40;
    let Ok(tools) = SandboxTools::locate() else {
        eprintln!("skipping race test: bubblewrap/prlimit not installed");
        return;
    };
    let tmp = TempDir::new("sweep-race");
    let run = tmp.path().join("run");
    fs::create_dir(&run).unwrap();
    let base = 9000 + std::process::id() % 1000;
    let sleep_marker = |i: usize| format!("{base}.{:04}{i:02}", std::process::id() % 10_000);
    for i in 0..RUNS {
        let mut spec = verification_spec(&VerificationLaunch {
            work_dir: tmp.path().to_path_buf(),
            argv: vec!["/bin/sleep".into(), sleep_marker(i).into()],
            extra_ro: Vec::new(),
            env: Vec::new(),
            limits: Limits::VERIFICATION,
        })
        .unwrap();
        spec.marker(&sandbox_marker(&run)).unwrap();
        let cmd = spec.command(&tools).unwrap();
        let (mut supervisor, mut gate) = cmd
            .spawn_via(|sandbox| {
                let mut supervisor = Command::new("/bin/sh");
                supervisor
                    .arg("-c")
                    .arg(format!(
                        "{} </dev/null >/dev/null 2>&1 & kill -KILL $$",
                        shell_line(sandbox)
                    ))
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                supervisor
            })
            .unwrap();
        if i % 2 == 0 {
            gate.open().unwrap();
        } else {
            drop(gate);
        }
        let _ = supervisor.wait();
    }
    thread::sleep(Duration::from_millis(1500));
    let alive: Vec<usize> = (0..RUNS)
        .filter(|i| sleepers(&sleep_marker(*i)) > 0)
        .collect();
    eprintln!(
        "{} of {RUNS} forced races left a running program before the sweep: {alive:?}",
        alive.len()
    );
    assert!(
        alive.iter().all(|i| i % 2 == 0),
        "a sandbox whose gate was never opened ran its program: {alive:?}"
    );
    let report = sweep_stale(&run).unwrap();
    assert!(
        wait_until(Duration::from_secs(5), || (0..RUNS)
            .all(|i| sleepers(&sleep_marker(i)) == 0)),
        "survivors after the sweep: {report:?}"
    );
    assert!(
        kill_marked(&sandbox_marker(&run)).is_empty(),
        "marked sandbox processes are left"
    );
}
