mod adapters_support;

use std::fs;
use std::path::Path;
use std::time::Duration;

use adapters_support::TempDir;
use lade_system::adapters::sandbox::cgroup::{self, SandboxCgroups};
use lade_system::adapters::sandbox::{
    CgroupLimits, Limits, OBJECTIVE_CGROUP_LIMITS, SandboxSpec, SandboxTools, run_bounded_with,
};

const OBJECTIVE: &str = "0b8a4c1e-7a1f-4f7e-9d7a-0d6c8f2a9b11";

fn fake_mount(controllers: &str) -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new("cgroup");
    let own = tmp.path().join("lade.service");
    fs::create_dir(&own).unwrap();
    fs::write(
        tmp.path().join("cgroup.controllers"),
        "cpu memory pids io\n",
    )
    .unwrap();
    fs::write(own.join("cgroup.controllers"), controllers).unwrap();
    (tmp, own)
}

#[test]
fn without_delegation_detection_explains_the_fallback() {
    let (tmp, _) = fake_mount("cpu memory pids\n");
    let limits = OBJECTIVE_CGROUP_LIMITS;
    let reason = |membership: &str, mount: &Path| {
        cgroup::detect_in(membership, mount, limits, 1).expect_err("must fall back")
    };
    assert!(reason("4:memory:/x\n1:cpu:/\n", tmp.path()).contains("cgroup v1 or hybrid"));
    assert!(reason("0::/lade.service\n", &tmp.path().join("missing")).contains("not mounted"));
    let not_delegated = reason("0::/lade.service\n", tmp.path());
    assert!(not_delegated.contains("not delegated"), "{not_delegated}");
    assert!(!tmp.path().join("lade.service/service").exists());
}

#[test]
fn delegated_subtree_gets_a_service_leaf_and_limited_objective_cgroups() {
    let (_tmp, own) = fake_mount("cpu memory pids\n");
    let limits = CgroupLimits {
        memory_max: Some(64 << 20),
        pids_max: Some(32),
        cpu_max: Some((50_000, 100_000)),
    };
    let cgroups = SandboxCgroups::prepare(&own, limits, 4242).unwrap();
    let read = |path: &Path| fs::read_to_string(path).unwrap();
    assert_eq!(read(&own.join("service/cgroup.procs")), "4242");
    assert_eq!(
        read(&own.join("cgroup.subtree_control")),
        "+memory +pids +cpu"
    );
    assert_eq!(cgroups.root(), own.join("sandboxes"));
    assert_eq!(
        read(&own.join("sandboxes/cgroup.subtree_control")),
        "+memory +pids +cpu"
    );
    let leaf = cgroups.leaf(OBJECTIVE, "worker-1").unwrap();
    let group = own.join("sandboxes").join(OBJECTIVE);
    assert_eq!(leaf.path(), group.join("worker-1"));
    assert_eq!(read(&group.join("memory.max")), "67108864");
    assert_eq!(read(&group.join("pids.max")), "32");
    assert_eq!(read(&group.join("cpu.max")), "50000 100000");
    leaf.add(77).unwrap();
    assert_eq!(read(&leaf.path().join("cgroup.procs")), "77");
    assert!(cgroups.leaf("../escape", "x").is_err());
    assert!(cgroups.leaf(OBJECTIVE, "a/b").is_err());

    let unlimited = SandboxCgroups::prepare(
        &own,
        CgroupLimits {
            memory_max: None,
            pids_max: None,
            cpu_max: None,
        },
        4242,
    )
    .unwrap();
    let _leaf = unlimited.leaf("other-objective", "terminal-1").unwrap();
    let other = own.join("sandboxes/other-objective");
    assert_eq!(read(&other.join("memory.max")), "max");
    assert_eq!(read(&other.join("cpu.max")), "max 100000");
}

#[test]
fn missing_controllers_fall_back_with_their_names() {
    let (_tmp, own) = fake_mount("cpu io\n");
    let error = SandboxCgroups::prepare(&own, OBJECTIVE_CGROUP_LIMITS, 1).unwrap_err();
    assert!(error.contains("memory pids"), "{error}");
    assert!(!own.join("service").exists());
}

#[test]
fn delegated_cgroup_contains_and_limits_a_sandbox() {
    if std::env::var_os("LADE_TEST_DELEGATED_CGROUP").is_none_or(|v| v != "1") {
        eprintln!(
            "skipping delegated cgroup test: set LADE_TEST_DELEGATED_CGROUP=1 in a delegated scope"
        );
        return;
    }
    let tools = SandboxTools::locate().expect("bubblewrap");
    let limits = CgroupLimits {
        memory_max: Some(256 << 20),
        pids_max: Some(16),
        cpu_max: None,
    };
    let cgroups = cgroup::detect(limits).expect("a delegated cgroup");
    let leaf = cgroups.leaf(OBJECTIVE, "verify-test").unwrap();
    let tmp = TempDir::new("cgroup-real");
    let script = "cat /proc/self/cgroup; i=0; while [ $i -lt 40 ]; do sleep 5 & i=$((i+1)); done 2>&1 | head -1; echo forks-limited";
    let mut spec = SandboxSpec::new(
        vec!["/bin/sh".into(), "-c".into(), script.into()],
        tmp.path(),
        Limits::VERIFICATION,
    )
    .unwrap();
    spec.rw_bind(tmp.path()).unwrap();
    let outcome = run_bounded_with(
        spec.command(&tools).unwrap(),
        Duration::from_secs(30),
        64 << 10,
        |pid| {
            lade_system::supervision::sweep::place_sandbox(pid, &leaf, || {
                lade_system::supervision::sweep::PidRecord::of(pid).is_none()
            })
        },
    )
    .unwrap();
    let out = String::from_utf8_lossy(&outcome.output_tail);
    let relative = leaf
        .path()
        .strip_prefix("/sys/fs/cgroup")
        .unwrap()
        .display()
        .to_string();
    assert!(out.contains(&format!("0::/{relative}")), "{out}");
    assert!(out.contains("forks-limited"), "{out}");
    assert!(
        fs::read_to_string(leaf.path().parent().unwrap().join("pids.events"))
            .is_ok_and(|events| !events.contains("max 0")),
        "pids.max was never hit: {out}"
    );
}
