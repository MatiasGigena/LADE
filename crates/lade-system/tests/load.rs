//! Canonical load, latency and soak procedures of the performance budgets (IMP-013, A19).
//!
//! Ignored by default (long-running, host-dependent); run through `scripts/load`, which builds in
//! release mode and records the results:
//!
//! ```text
//! scripts/load [OUT_DIR]                       # nominal 1/4 workers, launches, client phase
//! LADE_LOAD_SOAK_SECS=1800 scripts/load --soak [OUT_DIR]
//! ```
//!
//! Everything runs against the real `laded` binary with real sandboxed workers and the offline
//! `lade-fixture` model; nothing is mocked. The driver is a test client holding the controlling
//! lease, so the GTK client is measured in separate phases (idle with the reference workload and
//! while four agents stream queued work), when `LADE_LOAD_CLIENT` names `lade-desktop` and an X
//! display is available. Memory is PSS summed per owned process group (service: `laded` and its
//! Git helpers; worker: each Node/SDK sandbox; jobs: processes below a worker's Node and terminal
//! or verification sandboxes; client), CPU is percent of one logical core from `/proc/<pid>/stat`
//! ticks between samples (short-lived processes between two samples are not counted).

mod service_support;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use lade_protocol::client::{
    Basis, DiffGetQuery, DraftSaveCommand, Empty, Event, ExecutionState, FileReadQuery,
    MessageSendCommand, ObjectiveSelector, Reference, ReferenceVersion, ReplyData, Request,
    ServiceStopCommand, Side, StopReason,
};
use lade_protocol::types::{Counter, DeliveryMode, Id, ReadLimit, WirePath};
use lade_system::supervision::credentials::FIXTURE_SWITCH;
use service_support::*;

// ---------------------------------------------------------------------------------------------
// Configuration.

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn out_dir() -> PathBuf {
    let dir = std::env::var_os("LADE_LOAD_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("lade-load"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

const OBJECTIVES: usize = 32;
const REF_LINES: usize = 20_000;
const REF_BYTES: usize = 1_280_000;
const DIFF_LINES: usize = 2_000;

/// Deterministic UTF-8 reference file: 20,000 lines, 1,280,000 bytes.
fn reference_file() -> String {
    let mut out = String::with_capacity(REF_BYTES);
    let width = REF_BYTES / REF_LINES - 1;
    for n in 0..REF_LINES {
        let mut line = format!("{n:06} reference line — größe ");
        while line.len() < width {
            line.push(char::from(b'a' + (line.len() % 26) as u8));
        }
        line.truncate(width);
        out.push_str(&line);
        out.push('\n');
    }
    assert_eq!(out.len(), REF_BYTES);
    out
}

/// 2,000 lines; the worktree version changes every line, a 4,000-line signed diff.
fn diff_file(changed: bool) -> String {
    (0..DIFF_LINES)
        .map(|n| {
            if changed {
                format!("{n:05} changed line of the reference diff, revised\n")
            } else {
                format!("{n:05} original line of the reference diff file\n")
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Process accounting.

#[derive(Clone, Copy, Debug, Default)]
struct Groups {
    client: u64,
    service: u64,
    worker: u64,
    jobs: u64,
    workers: u32,
}

impl Groups {
    fn total(&self) -> u64 {
        self.client + self.service + self.worker + self.jobs
    }
}

#[derive(Clone, Debug)]
struct Sample {
    /// Seconds since the phase started.
    t: f64,
    phase: String,
    pss_kib: Groups,
    /// Percent of one logical core since the previous sample.
    cpu: [f64; 4],
}

struct Proc {
    ppid: u32,
    comm: String,
    ticks: u64,
    start: u64,
}

fn read_proc(pid: u32) -> Option<Proc> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let comm = stat[open + 1..close].to_string();
    let fields: Vec<&str> = stat[close + 2..].split(' ').collect();
    if fields.first() == Some(&"Z") {
        return None;
    }
    let num = |i: usize| fields.get(i).and_then(|v| v.parse::<u64>().ok());
    Some(Proc {
        ppid: u32::try_from(num(1)?).ok()?,
        comm,
        ticks: num(11)? + num(12)?,
        start: num(19)?,
    })
}

fn pss_kib(pid: u32) -> u64 {
    fs::read_to_string(format!("/proc/{pid}/smaps_rollup"))
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|l| l.strip_prefix("Pss:"))
                .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        })
        .unwrap_or(0)
}

/// Group of every owned process: 0 client, 1 service, 2 worker, 3 jobs.
fn classify(service: u32, client: Option<u32>) -> Vec<(u32, usize, Proc)> {
    let mut procs: HashMap<u32, Proc> = HashMap::new();
    for entry in fs::read_dir("/proc").unwrap().flatten() {
        if let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse().ok())
            && let Some(p) = read_proc(pid)
        {
            procs.insert(pid, p);
        }
    }
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (pid, p) in &procs {
        children.entry(p.ppid).or_default().push(*pid);
    }
    let mut out = Vec::new();
    // Descendants of `root` with the group decided per subtree.
    fn walk(
        pid: u32,
        group: usize,
        procs: &HashMap<u32, Proc>,
        children: &HashMap<u32, Vec<u32>>,
        out: &mut Vec<(u32, usize)>,
    ) {
        out.push((pid, group));
        for child in children.get(&pid).into_iter().flatten() {
            let below_node = group == 2 && procs.get(&pid).is_some_and(|p| p.comm == "node");
            walk(
                child.to_owned(),
                if below_node { 3 } else { group },
                procs,
                children,
                out,
            );
        }
    }
    fn has_node(pid: u32, procs: &HashMap<u32, Proc>, children: &HashMap<u32, Vec<u32>>) -> bool {
        procs.get(&pid).is_some_and(|p| p.comm == "node")
            || children
                .get(&pid)
                .into_iter()
                .flatten()
                .any(|c| has_node(*c, procs, children))
    }
    let mut groups = Vec::new();
    if procs.contains_key(&service) {
        groups.push((service, 1));
        for child in children.get(&service).into_iter().flatten() {
            let group = match procs.get(child).map(|p| p.comm.as_str()) {
                Some("git") => 1,
                _ if has_node(*child, &procs, &children) => 2,
                _ => 3,
            };
            walk(*child, group, &procs, &children, &mut groups);
        }
    }
    if let Some(client) = client.filter(|c| procs.contains_key(c)) {
        walk(client, 0, &procs, &children, &mut groups);
    }
    for (pid, group) in groups {
        if let Some(p) = procs.remove(&pid) {
            out.push((pid, group, p));
        }
    }
    out
}

/// Samples the owned process groups every `interval` until stopped.
struct Sampler {
    samples: Arc<Mutex<Vec<Sample>>>,
    phase: Arc<Mutex<String>>,
    client: Arc<Mutex<Option<u32>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

const CLK_TCK: f64 = 100.0;

impl Sampler {
    fn start(service: u32, interval: Duration) -> Self {
        let samples = Arc::new(Mutex::new(Vec::new()));
        let phase = Arc::new(Mutex::new(String::from("start")));
        let client = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (s, p, c, st) = (
            Arc::clone(&samples),
            Arc::clone(&phase),
            Arc::clone(&client),
            Arc::clone(&stop),
        );
        let thread = thread::spawn(move || {
            let begin = Instant::now();
            let mut last: HashMap<(u32, u64), u64> = HashMap::new();
            let mut last_at = Instant::now();
            while !st.load(Ordering::SeqCst) {
                let client = *c.lock().unwrap();
                let procs = classify(service, client);
                let elapsed = last_at.elapsed().as_secs_f64().max(0.001);
                last_at = Instant::now();
                let mut pss = Groups::default();
                let mut cpu = [0.0f64; 4];
                let mut seen = HashMap::new();
                for (pid, group, proc_) in &procs {
                    let kib = pss_kib(*pid);
                    match group {
                        0 => pss.client += kib,
                        1 => pss.service += kib,
                        2 => {
                            pss.worker += kib;
                            if proc_.comm == "node" {
                                pss.workers += 1;
                            }
                        }
                        _ => pss.jobs += kib,
                    }
                    let key = (*pid, proc_.start);
                    let before = last.get(&key).copied().unwrap_or(proc_.ticks);
                    cpu[*group] +=
                        (proc_.ticks.saturating_sub(before)) as f64 / CLK_TCK / elapsed * 100.0;
                    seen.insert(key, proc_.ticks);
                }
                last = seen;
                s.lock().unwrap().push(Sample {
                    t: begin.elapsed().as_secs_f64(),
                    phase: p.lock().unwrap().clone(),
                    pss_kib: pss,
                    cpu,
                });
                thread::sleep(interval);
            }
        });
        Self {
            samples,
            phase,
            client,
            stop,
            thread: Some(thread),
        }
    }

    fn phase(&self, name: &str) {
        *self.phase.lock().unwrap() = name.to_string();
    }

    fn set_client(&self, pid: Option<u32>) {
        *self.client.lock().unwrap() = pid;
    }

    fn finish(mut self) -> Vec<Sample> {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        std::mem::take(&mut *self.samples.lock().unwrap())
    }
}

// ---------------------------------------------------------------------------------------------
// Statistics.

#[derive(Clone, Debug, Default)]
struct Dist {
    ms: Vec<f64>,
}

impl Dist {
    fn push(&mut self, d: Duration) {
        self.ms.push(d.as_secs_f64() * 1000.0);
    }

    /// Nearest-rank percentile.
    fn pct(&self, p: f64) -> f64 {
        if self.ms.is_empty() {
            return f64::NAN;
        }
        let mut sorted = self.ms.clone();
        sorted.sort_by(f64::total_cmp);
        let rank = ((p / 100.0) * sorted.len() as f64).ceil().max(1.0) as usize;
        sorted[rank.min(sorted.len()) - 1]
    }

    fn max(&self) -> f64 {
        self.ms.iter().copied().fold(f64::NAN, f64::max)
    }

    fn row(&self, name: &str) -> String {
        format!(
            "| {name} | {} | {:.1} | {:.1} | {:.1} |\n",
            self.ms.len(),
            self.pct(50.0),
            self.pct(95.0),
            self.max()
        )
    }
}

fn mib(kib: u64) -> f64 {
    kib as f64 / 1024.0
}

/// Max and median PSS (MiB) and mean CPU (%) of one phase.
fn phase_stats(samples: &[Sample], phase: &str) -> String {
    let rows: Vec<&Sample> = samples.iter().filter(|s| s.phase == phase).collect();
    if rows.is_empty() {
        return format!("| {phase} | – | – | – | – | – | – |\n");
    }
    let col = |f: &dyn Fn(&Groups) -> u64| {
        let mut v: Vec<u64> = rows.iter().map(|s| f(&s.pss_kib)).collect();
        v.sort_unstable();
        (mib(v[v.len() / 2]), mib(*v.last().unwrap()))
    };
    let cpu = |i: usize| rows.iter().map(|s| s.cpu[i]).sum::<f64>() / rows.len() as f64;
    let (cm, cx) = col(&|g| g.client);
    let (sm, sx) = col(&|g| g.service);
    let (wm, wx) = col(&|g| g.worker);
    let (jm, jx) = col(&|g| g.jobs);
    let (tm, tx) = col(&|g| g.total());
    let workers = rows.iter().map(|s| s.pss_kib.workers).max().unwrap_or(0);
    format!(
        "| {phase} | {cm:.1} / {cx:.1} · {:.1}% | {sm:.1} / {sx:.1} · {:.1}% | {wm:.1} / {wx:.1} ({workers}) · {:.1}% | {jm:.1} / {jx:.1} · {:.1}% | {tm:.1} / {tx:.1} | {} |\n",
        cpu(0),
        cpu(1),
        cpu(2),
        cpu(3),
        rows.len()
    )
}

const PHASE_HEADER: &str = "| Phase | Client PSS median / max MiB · CPU | Service | Workers (count) | Jobs | All-owned total | Samples |\n| --- | --- | --- | --- | --- | --- | --- |\n";

fn write_timeline(path: &Path, samples: &[Sample]) {
    let mut csv = String::from(
        "t_s,phase,client_kib,service_kib,worker_kib,jobs_kib,total_kib,workers,client_cpu,service_cpu,worker_cpu,jobs_cpu\n",
    );
    for s in samples {
        let g = &s.pss_kib;
        let _ = writeln!(
            csv,
            "{:.1},{},{},{},{},{},{},{},{:.2},{:.2},{:.2},{:.2}",
            s.t,
            s.phase,
            g.client,
            g.service,
            g.worker,
            g.jobs,
            g.total(),
            g.workers,
            s.cpu[0],
            s.cpu[1],
            s.cpu[2],
            s.cpu[3]
        );
    }
    fs::write(path, csv).unwrap();
}

// ---------------------------------------------------------------------------------------------
// The service under test.

/// `laded` from `LADE_LOAD_LADED` (the release binary from `scripts/load`), else this build's.
fn laded_binary() -> PathBuf {
    std::env::var_os("LADE_LOAD_LADED")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_laded")))
}

struct Laded {
    child: Child,
    socket: PathBuf,
    capability: PathBuf,
}

impl Laded {
    /// Starts the service (stopped before) and returns the launch-to-ready time: process start
    /// until an admitted client received its snapshot.
    fn start(env: &Env) -> (Self, Duration) {
        let worker_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../packages/pi-worker")
            .canonicalize()
            .unwrap();
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(env.root.path().join("laded.log"))
            .unwrap();
        let started = Instant::now();
        let child = Command::new(laded_binary())
            .args(["serve", "--state-dir"])
            .arg(env.root.path().join("state"))
            .arg("--runtime-dir")
            .arg(env.root.path().join("run"))
            .args(["--max-workers", "32"])
            .env("LADE_WORKER_DIR", worker_dir)
            .env(FIXTURE_SWITCH, "1")
            .env_remove("LADE_FAULT_POINT")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        let laded = Self {
            child,
            socket: env.socket(),
            capability: env.root.path().join("run/control.cap"),
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(mut client) = laded.connect()
                && client.call(Request::Snapshot(Empty {})).is_ok()
            {
                let ready = started.elapsed();
                drop(client);
                // Let the lease free before the driver connects.
                thread::sleep(Duration::from_millis(50));
                return (laded, ready);
            }
            assert!(Instant::now() < deadline, "laded did not start");
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn connect(&self) -> Option<Client> {
        // Probe first: the test client helper panics on a missing or dead socket.
        drop(std::os::unix::net::UnixStream::connect(&self.socket).ok()?);
        let capability = fs::read_to_string(&self.capability).ok()?;
        Client::connect(&self.socket, &capability).ok()
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Explicit stop (`service.stop`), as the window's Exit does.
    fn stop(mut self, client: Option<Client>) {
        let client = client.or_else(|| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(client) = self.connect() {
                    break Some(client);
                }
                if Instant::now() > deadline {
                    break None;
                }
                thread::sleep(Duration::from_millis(20));
            }
        });
        if let Some(mut client) = client {
            let _ = client.call(Request::ServiceStop(ServiceStopCommand {
                reason: StopReason::Exit,
            }));
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Laded {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn stop_service(laded: Laded, client: Client) {
    laded.stop(Some(client));
}

// ---------------------------------------------------------------------------------------------
// Workload.

struct World {
    env: Env,
    objectives: Vec<Id>,
    probe: Id,
}

fn message_text(turn: u64, delay_ms: u64) -> String {
    if turn.is_multiple_of(4) {
        format!(
            "lade-fixture: delay {delay_ms}\nlade-fixture: write notes/turn-{}.txt turn {turn}\\n",
            turn % 16
        )
    } else {
        let mut text = format!("lade-fixture: echo\nlade-fixture: delay {delay_ms}\nturn {turn}: ");
        while text.len() < 1536 {
            text.push_str("streamed reference text for the load trace; ");
        }
        text
    }
}

/// Creates the 32-Objective reference state with the probe Objective's worktree holding the
/// reference file and diff.
fn build_world(tag: &str) -> World {
    let env = Env::new(tag);
    fs::write(env.repo.join("ref.txt"), reference_file()).unwrap();
    fs::write(env.repo.join("diff.txt"), diff_file(false)).unwrap();
    commit_all(&env.repo, "reference workload");
    let (laded, _) = Laded::start(&env);
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objectives: Vec<Id> = (0..OBJECTIVES)
        .map(|n| {
            create_objective(
                &mut client,
                &ws.workspace_id,
                &format!("Load objective {n:02}"),
            )
            .objective_id
        })
        .collect();
    let probe = objectives[OBJECTIVES - 1].clone();
    start_execution(&mut client, &probe);
    wait_state(&mut client, &probe, ExecutionState::Idle);
    fs::write(worktree(&env, &probe).join("diff.txt"), diff_file(true)).unwrap();
    client
        .call(Request::ExecutionCancel(ObjectiveSelector {
            objective_id: probe.clone(),
        }))
        .unwrap();
    stop_service(laded, client);
    World {
        env,
        objectives,
        probe,
    }
}

fn file_read(
    client: &mut Client,
    objective: &Id,
    path: &str,
) -> lade_protocol::client::FileContent {
    match client.call(Request::FileRead(FileReadQuery {
        objective_id: objective.clone(),
        basis: Basis::Working,
        path: WirePath::from_bytes(path.as_bytes().to_vec()).unwrap(),
        side: Side::New,
        offset: Counter::new(0),
        limit: ReadLimit::parse("262144").unwrap(),
    })) {
        Ok(ReplyData::FileRead(content)) => content,
        other => panic!("file.read: {other:?}"),
    }
}

fn diff_get(client: &mut Client, objective: &Id) -> bool {
    match client.call(Request::DiffGet(DiffGetQuery {
        objective_id: objective.clone(),
        basis: Basis::Working,
        path: WirePath::from_bytes(b"diff.txt".to_vec()).unwrap(),
    })) {
        Ok(ReplyData::DiffGet(diff)) => diff.truncated,
        other => panic!("diff.get: {other:?}"),
    }
}

/// Keeps `streams` Objectives busy: a new message whenever one goes idle.
struct Streams {
    objectives: Vec<Id>,
    turns: u64,
    delay_ms: u64,
    accept: Dist,
}

impl Streams {
    fn kick(&mut self, client: &mut Client, objective: &Id) {
        self.turns += 1;
        let text = message_text(self.turns, self.delay_ms);
        let started = Instant::now();
        let reply = client.call(Request::MessageSend(MessageSendCommand {
            objective_id: objective.clone(),
            text,
            mode: DeliveryMode::FollowUp,
        }));
        if reply.is_ok() {
            self.accept.push(started.elapsed());
        }
    }

    /// Reacts to buffered events: an Objective of ours that became idle gets its next turn.
    fn pump(&mut self, client: &mut Client) {
        let idle: Vec<Id> = client
            .events
            .drain(..)
            .filter_map(|frame| match frame.event {
                Event::ObjectiveChanged(s)
                    if s.execution.state == lade_protocol::client::ExecutionState::Idle =>
                {
                    Some(s.objective_id)
                }
                _ => None,
            })
            .filter(|id| self.objectives.contains(id))
            .collect();
        for objective in idle {
            self.kick(client, &objective);
        }
    }
}

fn ensure_running(client: &mut Client, objective: &Id) {
    let state = detail(client, objective).execution.state;
    match state {
        ExecutionState::None => {
            start_execution(client, objective);
        }
        ExecutionState::Cancelled | ExecutionState::Failed => {
            client
                .call(Request::ExecutionResume(ObjectiveSelector {
                    objective_id: objective.clone(),
                }))
                .unwrap();
        }
        _ => {}
    }
}

struct RunResult {
    input: Dist,
    open_file: Dist,
    open_diff: Dist,
    accept: Dist,
    diff_truncated: bool,
}

/// One nominal run: fresh service, 60 s idle, then `workers` streaming Objectives for the load
/// duration with 200 draft saves and 40 file/diff opens spread over it.
fn nominal_run(
    world: &World,
    workers: usize,
    run: usize,
    sampler_out: &mut Vec<Sample>,
    launch: &mut Dist,
) -> RunResult {
    let idle_secs = env_u64("LADE_LOAD_IDLE_SECS", 60);
    let load_secs = env_u64("LADE_LOAD_SECS", 300);
    let inputs = env_u64("LADE_LOAD_INPUTS", 200);
    let opens = env_u64("LADE_LOAD_OPENS", 40);
    let (laded, ready) = Laded::start(&world.env);
    launch.push(ready);
    let sampler = Sampler::start(laded.pid(), Duration::from_secs(1));
    let mut client = world.env.connect();
    sampler.phase(&format!("idle-{workers}w-run{run}"));
    thread::sleep(Duration::from_secs(idle_secs));

    // Reference for the draft saves: exact bytes of the reference file.
    let content = file_read(&mut client, &world.probe, "ref.txt");
    let capture = match client.call(Request::ChangesList(
        lade_protocol::client::ChangesListQuery {
            objective_id: world.probe.clone(),
            basis: Basis::Working,
        },
    )) {
        Ok(ReplyData::ChangesList(list)) => list.capture_id.unwrap(),
        other => panic!("changes.list: {other:?}"),
    };
    let text = content.text.clone().unwrap();
    let selected: String = text.chars().take(6).collect();
    let reference = Reference {
        artifact_digest: content.artifact_digest.clone(),
        path: WirePath::from_bytes(b"ref.txt".to_vec()).unwrap(),
        version: ReferenceVersion::Working {
            capture_id: capture,
        },
        side: Side::New,
        byte_start: Counter::new(0),
        byte_end: Counter::new(selected.len() as u64),
        selected_text: selected,
        line_hint: Some(Counter::new(1)),
    };

    let streaming: Vec<Id> = world.objectives[..workers].to_vec();
    sampler.phase("starting");
    for objective in &streaming {
        ensure_running(&mut client, objective);
    }
    for objective in &streaming {
        wait_state(&mut client, objective, ExecutionState::Idle);
    }
    // Already-open idle workers, measured separately (lazy lifecycle unchanged).
    thread::sleep(Duration::from_secs(5));
    sampler.phase(&format!("idleworkers-{workers}w-run{run}"));
    thread::sleep(Duration::from_secs(idle_secs / 2));
    sampler.phase(&format!("load-{workers}w-run{run}"));
    let mut streams = Streams {
        objectives: streaming.clone(),
        turns: 0,
        // Total routine stream updates ≈ 20/s across the active workers (4 deltas per turn).
        delay_ms: 50 * workers as u64,
        accept: Dist::default(),
    };
    for objective in &streaming {
        streams.kick(&mut client, objective);
    }
    let mut result = RunResult {
        input: Dist::default(),
        open_file: Dist::default(),
        open_diff: Dist::default(),
        accept: Dist::default(),
        diff_truncated: false,
    };
    let draft = new_id();
    let mut revision = 0u64;
    let begin = Instant::now();
    let total = Duration::from_secs(load_secs);
    let actions = inputs + opens;
    let mut done = 0u64;
    while begin.elapsed() < total {
        streams.pump(&mut client);
        let due = total.mul_f64(done as f64 / actions as f64);
        if done < actions && begin.elapsed() >= due {
            // Interleave: one open after every five inputs.
            if done % 6 == 5
                && result.open_file.ms.len() + result.open_diff.ms.len() < opens as usize
            {
                let started = Instant::now();
                if result.open_file.ms.len() <= result.open_diff.ms.len() {
                    file_read(&mut client, &world.probe, "ref.txt");
                    result.open_file.push(started.elapsed());
                } else {
                    result.diff_truncated |= diff_get(&mut client, &world.probe);
                    result.open_diff.push(started.elapsed());
                }
            } else if result.input.ms.len() < inputs as usize {
                let started = Instant::now();
                let saved = client.call(Request::DraftSave(DraftSaveCommand {
                    draft_id: draft.clone(),
                    objective_id: world.probe.clone(),
                    expected_revision: Counter::new(revision),
                    reference: reference.clone(),
                    body: format!("load input action {revision}"),
                }));
                assert!(saved.is_ok(), "draft.save: {saved:?}");
                revision += 1;
                result.input.push(started.elapsed());
            }
            done += 1;
        }
        thread::sleep(Duration::from_millis(20));
    }
    result.accept = streams.accept.clone();
    sampler.phase("stopping");
    for objective in &streaming {
        let _ = client.call(Request::ExecutionCancel(ObjectiveSelector {
            objective_id: objective.clone(),
        }));
    }
    let _ = client.call(Request::DraftDelete(
        lade_protocol::client::DraftDeleteCommand {
            draft_id: draft,
            expected_revision: Counter::new(revision),
        },
    ));
    stop_service(laded, client);
    sampler_out.extend(sampler.finish());
    result
}

fn versions() -> String {
    let run = |cmd: &str, args: &[&str]| {
        Command::new(cmd)
            .args(args)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let cpu = fs::read_to_string("/proc/cpuinfo")
        .unwrap_or_default()
        .lines()
        .find_map(|l| {
            l.strip_prefix("model name")
                .map(|v| v.trim_start_matches([' ', '\t', ':']).to_string())
        })
        .unwrap_or_default();
    let mem = fs::read_to_string("/proc/meminfo")
        .unwrap_or_default()
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    format!(
        "- laded: `{}` ({})\n- kernel: `{}`\n- CPU: {cpu}, {} logical cores\n- memory: {mem}\n- node: `{}`, git: `{}`, bubblewrap: `{}`\n",
        run(laded_binary().to_str().unwrap(), &["--version"]),
        laded_binary().display(),
        run("uname", &["-srm"]),
        thread::available_parallelism().map_or(0, |n| n.get()),
        run("node", &["--version"]),
        run("git", &["--version"]),
        run("bwrap", &["--version"]),
    )
}

// ---------------------------------------------------------------------------------------------
// GTK client phases (optional).

fn client_binary() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("LADE_LOAD_CLIENT")?);
    (path.is_file() && std::env::var_os("DISPLAY").is_some()).then_some(path)
}

fn spawn_client(binary: &Path, env: &Env) -> Child {
    Command::new(binary)
        .args(["--no-spawn", "--runtime-dir"])
        .arg(env.root.path().join("run"))
        .arg("--open")
        .arg(&env.repo)
        .env("XDG_CONFIG_HOME", env.root.path().join("config"))
        .env("GTK_A11Y", "none")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

/// A mapped window of exactly this client process (other clients may share the display).
fn window_visible(pid: u32) -> bool {
    Command::new("xdotool")
        .args(["search", "--onlyvisible", "--pid", &pid.to_string()])
        .output()
        .is_ok_and(|o| o.status.success() && !o.stdout.is_empty())
}

/// Time until the client's window is mapped, or `None` after 30 s.
fn wait_mapped(child: &Child, started: Instant) -> Option<Duration> {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if window_visible(child.id()) {
            return Some(started.elapsed());
        }
        thread::sleep(Duration::from_millis(5));
    }
    None
}

fn kill_client(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Client warm launches (window mapped, a proxy), then idle and streaming client phases.
fn client_phases(world: &World, report: &mut String, samples_out: &mut Vec<Sample>) {
    let Some(binary) = client_binary() else {
        report.push_str(
            "\n## GTK client\n\nNot measured in this run (`LADE_LOAD_CLIENT`/`DISPLAY` unset).\n",
        );
        return;
    };
    let launches = env_u64("LADE_LOAD_LAUNCHES", 20);
    let (laded, _) = Laded::start(&world.env);
    let mut mapped = Dist::default();
    // One start to warm the caches, not counted.
    let warm = spawn_client(&binary, &world.env);
    let _ = wait_mapped(&warm, Instant::now());
    kill_client(warm);
    thread::sleep(Duration::from_millis(300));
    let mut unmapped = 0;
    for _ in 0..launches {
        let started = Instant::now();
        let child = spawn_client(&binary, &world.env);
        match wait_mapped(&child, started) {
            Some(elapsed) => mapped.push(elapsed),
            None => unmapped += 1,
        }
        kill_client(child);
        thread::sleep(Duration::from_millis(300));
    }
    // Idle reference workload: 32 Objective records, no executing workers.
    let sampler = Sampler::start(laded.pid(), Duration::from_secs(1));
    let child = spawn_client(&binary, &world.env);
    sampler.set_client(Some(child.id()));
    sampler.phase("client-settle");
    thread::sleep(Duration::from_secs(env_u64(
        "LADE_LOAD_CLIENT_SETTLE_SECS",
        30,
    )));
    sampler.phase("client-idle");
    thread::sleep(Duration::from_secs(env_u64("LADE_LOAD_IDLE_SECS", 60)));
    sampler.set_client(None);
    sampler.phase("client-prepare");
    kill_client(child);
    // Four agents stream queued work while the window is attached (the driver queued it and
    // left; the window holds the lease and shows the Objectives list and its default view).
    let mut client = world.env.connect();
    let streaming: Vec<Id> = world.objectives[..4].to_vec();
    for objective in &streaming {
        ensure_running(&mut client, objective);
    }
    for objective in &streaming {
        wait_state(&mut client, objective, ExecutionState::Idle);
    }
    for (n, objective) in streaming.iter().enumerate() {
        for turn in 0..40u64 {
            let mut text =
                format!("lade-fixture: echo\nlade-fixture: delay 300\nqueued {n}/{turn}: ");
            while text.len() < 1536 {
                text.push_str("streamed while the window is attached; ");
            }
            client
                .call(Request::MessageSend(MessageSendCommand {
                    objective_id: objective.clone(),
                    text,
                    mode: DeliveryMode::FollowUp,
                }))
                .unwrap();
        }
    }
    drop(client);
    thread::sleep(Duration::from_millis(200));
    let child = spawn_client(&binary, &world.env);
    sampler.set_client(Some(child.id()));
    sampler.phase("client-stream-settle");
    thread::sleep(Duration::from_secs(10));
    sampler.phase("client-stream-4w");
    thread::sleep(Duration::from_secs(env_u64("LADE_LOAD_IDLE_SECS", 60)));
    sampler.set_client(None);
    kill_client(child);
    let samples = sampler.finish();
    let _ = write!(
        report,
        "\n## GTK client (Xvfb, development host)\n\nWarm launches: process start → main window mapped (`xdotool search --onlyvisible`). This is a **proxy**: it does not prove a usable, presented workbench.\n\n| Measure | n | p50 ms | p95 ms | max ms |\n| --- | --- | --- | --- | --- |\n{}\n{PHASE_HEADER}{}{}",
        mapped.row(&format!(
            "client window mapped ({unmapped} launches not mapped within 30 s)"
        )),
        phase_stats(&samples, "client-idle"),
        phase_stats(&samples, "client-stream-4w"),
    );
    samples_out.extend(samples);
    laded.stop(None);
}

// ---------------------------------------------------------------------------------------------
// Procedures.

#[test]
#[ignore = "long-running load procedure; run scripts/load"]
fn canonical_load() {
    let out = out_dir();
    let runs = env_u64("LADE_LOAD_RUNS", 5) as usize;
    let world = build_world("load");
    let mut report = format!(
        "# IMP-013 load procedure (development host, not Mint)\n\n{}\n- workload: {OBJECTIVES} Objective records; reference file {REF_LINES} lines / {REF_BYTES} bytes; diff {} lines; streams: fixture `echo` turns (4 deltas each) and `write` turns, ≈20 stream updates/s in total\n- per run: {} s idle, {} s load, {} draft saves (input → durable ack), {} opens (file.read first 256 KiB page / diff.get), fresh `laded` per run\n\n",
        versions(),
        DIFF_LINES * 2,
        env_u64("LADE_LOAD_IDLE_SECS", 60),
        env_u64("LADE_LOAD_SECS", 300),
        env_u64("LADE_LOAD_INPUTS", 200),
        env_u64("LADE_LOAD_OPENS", 40),
    );
    let mut samples = Vec::new();
    let mut launch = Dist::default();
    for workers in [1usize, 4] {
        let _ = writeln!(report, "## {workers} active worker(s)\n");
        report.push_str("| Run | Measure | n | p50 ms | p95 ms | max ms |\n| --- | --- | --- | --- | --- | --- |\n");
        let mut all = RunResult {
            input: Dist::default(),
            open_file: Dist::default(),
            open_diff: Dist::default(),
            accept: Dist::default(),
            diff_truncated: false,
        };
        for run in 1..=runs {
            eprintln!("load: {workers} worker(s), run {run}/{runs}");
            let result = nominal_run(&world, workers, run, &mut samples, &mut launch);
            for (name, dist) in [
                ("input: draft.save → durable ack", &result.input),
                ("open: file.read first page", &result.open_file),
                ("open: diff.get", &result.open_diff),
                ("message.send → accepted", &result.accept),
            ] {
                report.push_str(&format!("| {run} {}", &dist.row(name)[2..]));
            }
            all.input.ms.extend(&result.input.ms);
            all.open_file.ms.extend(&result.open_file.ms);
            all.open_diff.ms.extend(&result.open_diff.ms);
            all.accept.ms.extend(&result.accept.ms);
            all.diff_truncated |= result.diff_truncated;
        }
        for (name, dist) in [
            ("input: draft.save → durable ack", &all.input),
            ("open: file.read first page", &all.open_file),
            ("open: diff.get", &all.open_diff),
            ("message.send → accepted", &all.accept),
        ] {
            report.push_str(&format!("| all {}", &dist.row(name)[2..]));
        }
        let _ = write!(
            report,
            "\ndiff.get truncated the 4,000-line diff: {} (the service bound is 1,800 lines / 640 KiB, labelled `truncated`).\n\n{PHASE_HEADER}",
            all.diff_truncated
        );
        for run in 1..=runs {
            for phase in ["idle", "idleworkers", "load"] {
                report.push_str(&phase_stats(
                    &samples,
                    &format!("{phase}-{workers}w-run{run}"),
                ));
            }
        }
        report.push('\n');
    }
    // Warm launches of the service alone (service initially stopped, existing state).
    let launches = env_u64("LADE_LOAD_LAUNCHES", 20);
    for _ in launch.ms.len()..(launch.ms.len() + launches as usize) {
        let (laded, ready) = Laded::start(&world.env);
        launch.push(ready);
        laded.stop(None);
    }
    let _ = writeln!(
        report,
        "## Service launch\n\nProcess start → admitted client received its snapshot (warm OS cache, {OBJECTIVES} Objectives; includes restart recovery and the sandbox sweep).\n\n| Measure | n | p50 ms | p95 ms | max ms |\n| --- | --- | --- | --- | --- |\n{}",
        launch.row("laded start → snapshot")
    );
    // Saved before the optional client phases, which cannot lose the nominal results.
    write_timeline(&out.join("load-timeline.csv"), &samples);
    fs::write(out.join("load.md"), &report).unwrap();
    client_phases(&world, &mut report, &mut samples);
    write_timeline(&out.join("load-timeline.csv"), &samples);
    fs::write(out.join("load.md"), &report).unwrap();
    eprintln!("{report}");
}

#[test]
#[ignore = "30-minute soak; run scripts/load --soak"]
fn soak() {
    let out = out_dir();
    let warmup = env_u64("LADE_SOAK_WARMUP_SECS", 300);
    let soak = env_u64("LADE_SOAK_SECS", 1800);
    let cycle = env_u64("LADE_SOAK_CYCLE_SECS", 60);
    let world = build_world("soak");
    let (laded, _) = Laded::start(&world.env);
    let sampler = Sampler::start(laded.pid(), Duration::from_secs(5));
    let mut client = world.env.connect();
    let streaming: Vec<Id> = world.objectives[..4].to_vec();
    for objective in &streaming {
        ensure_running(&mut client, objective);
    }
    for objective in &streaming {
        wait_state(&mut client, objective, ExecutionState::Idle);
    }
    let mut streams = Streams {
        objectives: streaming.clone(),
        turns: 0,
        delay_ms: 200,
        accept: Dist::default(),
    };
    let begin = Instant::now();
    let mut quiescent: Vec<(f64, Groups)> = Vec::new();
    let mut views = 0usize;
    let mut cycle_no = 0u64;
    while begin.elapsed() < Duration::from_secs(warmup + soak) {
        cycle_no += 1;
        let soaking = begin.elapsed() >= Duration::from_secs(warmup);
        // Streaming half: four agents.
        sampler.phase(if soaking {
            "soak-stream"
        } else {
            "warmup-stream"
        });
        for objective in &streaming {
            streams.kick(&mut client, objective);
        }
        let half = Instant::now();
        while half.elapsed() < Duration::from_secs(cycle * 2 / 3) {
            streams.pump(&mut client);
            thread::sleep(Duration::from_millis(50));
        }
        // Views half: no new turns; switch across Objectives and views, then settle.
        sampler.phase(if soaking {
            "soak-views"
        } else {
            "warmup-views"
        });
        streams.objectives.clear();
        let views_begin = Instant::now();
        while views_begin.elapsed() < Duration::from_secs(cycle / 3) {
            let objective = &world.objectives[views % OBJECTIVES];
            views += 1;
            let _ = detail(&mut client, objective);
            let _ = client.call(Request::ActivityPage(
                lade_protocol::client::ActivityPageQuery {
                    objective_id: objective.clone(),
                    limit: lade_protocol::types::PageLimit::parse("50").unwrap(),
                    before: None,
                },
            ));
            if views.is_multiple_of(5) {
                file_read(&mut client, &world.probe, "ref.txt");
                diff_get(&mut client, &world.probe);
            }
            client.events.clear();
            thread::sleep(Duration::from_millis(200));
        }
        streams.objectives = streaming.clone();
        // Quiescent point: the last sample of the views half (agents idle, caches filled).
        thread::sleep(Duration::from_secs(6));
        if let Some(last) = sampler.samples.lock().unwrap().last() {
            quiescent.push((begin.elapsed().as_secs_f64(), last.pss_kib));
        }
        eprintln!(
            "soak: cycle {cycle_no}, {:.0} s",
            begin.elapsed().as_secs_f64()
        );
    }
    for objective in &streaming {
        let _ = client.call(Request::ExecutionCancel(ObjectiveSelector {
            objective_id: objective.clone(),
        }));
    }
    stop_service(laded, client);
    let samples = sampler.finish();
    write_timeline(&out.join("soak-timeline.csv"), &samples);
    let after: Vec<&(f64, Groups)> = quiescent
        .iter()
        .filter(|(t, _)| *t >= warmup as f64)
        .collect();
    let mut report = format!(
        "# IMP-013 soak (development host, not Mint)\n\n{}\n- warmup {warmup} s, soak {soak} s, cycles of {cycle} s: 2/3 four agents streaming (fixture echo/write turns), 1/3 view switching across the 32 Objectives (detail, activity page, file/diff opens), then a quiescent sample\n- turns sent: {}\n\n| Quiescent point | t s | Service MiB | Workers MiB | Jobs MiB | All-owned MiB |\n| --- | --- | --- | --- | --- | --- |\n",
        versions(),
        streams.turns
    );
    for (n, (t, g)) in after.iter().enumerate() {
        let _ = writeln!(
            report,
            "| {n} | {t:.0} | {:.1} | {:.1} | {:.1} | {:.1} |",
            mib(g.service),
            mib(g.worker),
            mib(g.jobs),
            mib(g.total())
        );
    }
    if let (Some(first), Some(last)) = (after.first(), after.last()) {
        let change = |a: u64, b: u64| (b as f64 - a as f64) / a.max(1) as f64 * 100.0;
        // Least-squares slope of the all-owned quiescent PSS, MiB per 10 minutes.
        let n = after.len() as f64;
        let mean_t = after.iter().map(|(t, _)| *t).sum::<f64>() / n;
        let mean_m = after.iter().map(|(_, g)| mib(g.total())).sum::<f64>() / n;
        let cov: f64 = after
            .iter()
            .map(|(t, g)| (t - mean_t) * (mib(g.total()) - mean_m))
            .sum();
        let var: f64 = after.iter().map(|(t, _)| (t - mean_t).powi(2)).sum();
        let slope = if var > 0.0 { cov / var * 600.0 } else { 0.0 };
        let within = change(first.1.total(), last.1.total()).abs() <= 10.0;
        let _ = writeln!(
            report,
            "\nFinal vs post-warmup quiescent PSS: service {:+.1}%, workers {:+.1}%, all-owned {:+.1}% (criterion: within 10%: **{}**). Trend of all-owned quiescent PSS: {slope:+.2} MiB per 10 min over {} points.\n",
            change(first.1.service, last.1.service),
            change(first.1.worker, last.1.worker),
            change(first.1.total(), last.1.total()),
            if within { "met" } else { "not met" },
            after.len()
        );
    }
    report.push_str(PHASE_HEADER);
    report.push_str(&phase_stats(&samples, "soak-stream"));
    report.push_str(&phase_stats(&samples, "soak-views"));
    fs::write(out.join("soak.md"), &report).unwrap();
    eprintln!("{report}");
}
