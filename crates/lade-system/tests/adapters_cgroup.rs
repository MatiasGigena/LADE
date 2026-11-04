mod adapters_support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use adapters_support::TempDir;
use lade_system::adapters::sandbox::cgroup::{self, CgroupError, SandboxCgroups};
use lade_system::adapters::sandbox::{
    CgroupLimits, Limits, OBJECTIVE_CGROUP_LIMITS, SandboxSpec, SandboxTools, run_bounded_with,
};

const OBJECTIVE: &str = "0b8a4c1e-7a1f-4f7e-9d7a-0d6c8f2a9b11";

fn fake_mount_at(own: &str, controllers: &str) -> (TempDir, PathBuf) {
    let tmp = TempDir::new("cgroup");
    let dir = tmp.path().join(own);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        tmp.path().join("cgroup.controllers"),
        "cpu memory pids io\n",
    )
    .unwrap();
    fs::write(dir.join("cgroup.controllers"), controllers).unwrap();
    for file in ["cgroup.procs", "cgroup.threads", "cgroup.subtree_control"] {
        fs::write(dir.join(file), "").unwrap();
    }
    (tmp, dir)
}

fn fake_mount(controllers: &str) -> (TempDir, PathBuf) {
    fake_mount_at("lade.service", controllers)
}

fn user_unit_cgroup() -> String {
    let euid = rustix::process::geteuid().as_raw();
    format!("user.slice/user-{euid}.slice/user@{euid}.service/app.slice/lade.service")
}

fn shown(control_group: &str, delegate: &str) -> String {
    format!("ControlGroup={control_group}\nDelegate={delegate}\n")
}

fn no_manager(unit: &str) -> Result<String, String> {
    panic!("the user manager must not be asked about {unit}")
}

#[test]
fn without_delegation_detection_explains_the_fallback() {
    let (tmp, _) = fake_mount("cpu memory pids\n");
    let limits = OBJECTIVE_CGROUP_LIMITS;
    let reason = |membership: &str, mount: &Path| {
        cgroup::detect_in(membership, mount, limits, 1, no_manager).expect_err("must fall back")
    };
    let v1 = reason("4:memory:/x\n1:cpu:/\n", tmp.path());
    assert!(matches!(v1, CgroupError::NoV2Membership), "{v1}");
    assert!(v1.to_string().contains("cgroup v1 or hybrid"));
    let unmounted = reason("0::/lade.service\n", &tmp.path().join("missing"));
    assert!(
        matches!(unmounted, CgroupError::NotMounted(_)),
        "{unmounted}"
    );
    assert!(unmounted.to_string().contains("not mounted"));
    let error = reason("0::/lade.service\n", tmp.path());
    assert!(
        matches!(&error, CgroupError::NotDelegated { cgroup, .. } if cgroup == "/lade.service"),
        "{error}"
    );
    let not_delegated = error.to_string();
    assert!(not_delegated.contains("not delegated"), "{not_delegated}");
    assert!(
        not_delegated.contains("not a service or scope"),
        "{not_delegated}"
    );
    assert!(!tmp.path().join("lade.service/service").exists());
}

#[test]
fn a_user_manager_unit_is_delegated_only_when_the_manager_says_so() {
    let own = user_unit_cgroup();
    let membership = format!("0::/{own}\n");
    let detect = |mount: &Path, answer: Result<String, String>| {
        cgroup::detect_in(&membership, mount, OBJECTIVE_CGROUP_LIMITS, 4242, |unit| {
            assert_eq!(unit, "lade.service");
            answer
        })
    };
    let refusals = [
        (
            Ok(shown(&format!("/{own}"), "no")),
            "reports Delegate=no for lade.service",
        ),
        (
            Ok(shown("/user.slice/elsewhere/lade.service", "yes")),
            "does not place lade.service in this cgroup",
        ),
        (Ok(String::new()), "does not place"),
        (
            Err("systemctl did not answer in time".into()),
            "could not be asked about lade.service: systemctl did not answer in time",
        ),
    ];
    for (answer, expected) in refusals {
        let (tmp, dir) = fake_mount_at(&own, "cpu memory pids\n");
        let error = detect(tmp.path(), answer)
            .expect_err("must fall back")
            .to_string();
        assert!(error.contains("not delegated"), "{error}");
        assert!(error.contains(expected), "{error}");
        assert!(!dir.join("service").exists(), "{error}: nothing written");
        assert_eq!(
            fs::read_to_string(dir.join("cgroup.subtree_control")).unwrap(),
            ""
        );
    }

    let (tmp, dir) = fake_mount_at(&own, "cpu memory pids\n");
    let cgroups = detect(tmp.path(), Ok(shown(&format!("/{own}"), "yes"))).unwrap();
    assert_eq!(
        cgroups.delegation(),
        "systemd user manager: lade.service has Delegate=yes"
    );
    assert_eq!(
        fs::read_to_string(dir.join("service/cgroup.procs")).unwrap(),
        "4242"
    );
    assert_eq!(cgroups.root(), dir.join("sandboxes"));
}

#[test]
fn a_delegated_cgroup_without_writable_interface_files_falls_back() {
    if rustix::process::geteuid().is_root() {
        eprintln!("skipping: root may write read-only files");
        return;
    }
    let own = user_unit_cgroup();
    let (tmp, dir) = fake_mount_at(&own, "cpu memory pids\n");
    fs::set_permissions(
        dir.join("cgroup.subtree_control"),
        fs::Permissions::from_mode(0o444),
    )
    .unwrap();
    let error = cgroup::detect_in(
        &format!("0::/{own}\n"),
        tmp.path(),
        OBJECTIVE_CGROUP_LIMITS,
        1,
        |_| Ok(shown(&format!("/{own}"), "yes")),
    )
    .unwrap_err();
    assert!(
        matches!(
            error,
            CgroupError::NotWritable {
                file: "cgroup.subtree_control",
                ..
            }
        ),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .contains("cgroup.subtree_control is not writable"),
        "{error}"
    );
    assert!(!dir.join("service").exists());
}

#[test]
fn a_delegate_attribute_is_accepted_without_asking_the_manager() {
    let (tmp, dir) = fake_mount("cpu memory pids\n");
    if let Err(e) =
        rustix::fs::setxattr(&dir, "user.delegate", b"1", rustix::fs::XattrFlags::empty())
    {
        eprintln!("skipping: the temporary directory takes no user xattrs ({e})");
        return;
    }
    let cgroups = cgroup::detect_in(
        "0::/lade.service\n",
        tmp.path(),
        OBJECTIVE_CGROUP_LIMITS,
        4242,
        no_manager,
    )
    .unwrap();
    assert_eq!(
        cgroups.delegation(),
        "user.delegate attribute on /lade.service"
    );
    assert!(dir.join("service").exists());
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
    assert!(
        matches!(&error, CgroupError::MissingControllers(missing) if missing == &["memory", "pids"]),
        "{error}"
    );
    assert!(error.to_string().contains("memory pids"), "{error}");
    assert!(!own.join("service").exists());
}

#[test]
fn a_cgroup_that_cannot_be_emptied_gets_no_controllers() {
    let (_tmp, own) = fake_mount("cpu memory pids\n");
    fs::write(own.join("cgroup.procs"), "99\n").unwrap();
    let error = SandboxCgroups::prepare(&own, OBJECTIVE_CGROUP_LIMITS, 4242).unwrap_err();
    assert!(
        matches!(&error, CgroupError::Unmovable(rest) if rest == &[99]),
        "{error}"
    );
    assert!(
        error.to_string().contains("could not be moved: [99]"),
        "{error}"
    );
    assert_eq!(
        fs::read_to_string(own.join("service/cgroup.procs")).unwrap(),
        "99"
    );
    assert_eq!(
        fs::read_to_string(own.join("cgroup.subtree_control")).unwrap(),
        ""
    );
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
    eprintln!("delegation: {}", cgroups.delegation());
    assert!(!cgroups.root().join("delegation-probe").exists());
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
    let placed = format!("/sandboxes/{OBJECTIVE}/verify-test");
    assert!(
        out.lines()
            .any(|line| line.starts_with("0::/") && line.ends_with(&placed)),
        "{out}"
    );
    assert!(out.contains("forks-limited"), "{out}");
    assert!(
        fs::read_to_string(leaf.path().parent().unwrap().join("pids.events"))
            .is_ok_and(|events| !events.contains("max 0")),
        "pids.max was never hit: {out}"
    );
}
