//! Startup sweep: kills recorded sandboxes whose identity still matches and leaves recycled
//! pids alone. Requires bubblewrap/prlimit (skips otherwise).

mod adapters_support;

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use adapters_support::TempDir;
use lade_system::adapters::sandbox::{Limits, SandboxTools, VerificationLaunch, verification_spec};
use lade_system::supervision::sweep::{PidRecord, read_pidfile, sweep_stale, write_pidfile};

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

/// Live, non-zombie `sleep` processes running exactly `sleep <marker>`. The comm check excludes
/// prlimit/bwrap, whose own command lines also end with `sleep <marker>`.
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
    let mut sandbox = spec.command(&tools).stdout(Stdio::null()).spawn().unwrap();
    let outer = sandbox.id();
    assert!(wait_until(Duration::from_secs(10), || sleepers(&marker) == 1));
    let init = child_of(outer).expect("namespace init");
    let records = [PidRecord::of(outer).unwrap(), PidRecord::of(init).unwrap()];
    let pidfile = run.join("worker-a-1.pid");
    write_pidfile(&pidfile, &records).unwrap();
    assert_eq!(read_pidfile(&pidfile).unwrap(), records);

    // A pidfile naming a live process with a different start time (pid reuse) is ignored.
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
    assert!(report.killed.contains(&outer), "{report:?}");
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
