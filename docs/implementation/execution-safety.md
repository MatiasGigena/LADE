# Execution safety: worker sandbox, egress gateway, credentials, sweep (WP-F2)

Status: implementation record for `crates/lade-system/src/supervision/{launcher,gateway,credentials,sweep,terminal}.rs`.
It instantiates [delivery plan §1, §5, §7](delivery-plan.md) and [architecture §5](../../architecture.md#5-host-authority-and-protected-git).
The service (`laded`) consumes it through the `supervision` contract (`WorkerLauncher`, `WorkerControl`,
`SupervisorEvent`); it never runs a worker outside this path. The "Not enforced" list at the end feeds
`limitations.md`.

## Launch sequence

`supervision::default_launcher(LauncherConfig) -> Box<dyn WorkerLauncher>` locates `bwrap` and `prlimit`,
canonicalizes the Node runtime and worker package, and creates the run directory (0700). Each `launch`:

1. Checks that the start body is `worker.start` whose `cwd`/`session_dir` equal the launch's worktree/session
   dir, and that the Objective id is file-name safe (`[A-Za-z0-9-]{1,64}`).
2. Runs `sandbox_available()` (a full sandbox with worker limits running `true`). Failure →
   `LaunchError::SandboxUnavailable`. **There is no unsandboxed fallback.**
3. Looks up the provider's egress policy (none → `ProviderUnavailable`) and releases the provider's
   credentials (missing → `ProviderUnavailable` naming the variables, never values).
4. Starts the per-launch gateway on `<socket_dir>/<first 12 hex of the objective id>-<generation>.sock`
   (`socket_dir` = `$RUNTIME/g` in `laded`; socket 0600, directory 0700; socket paths over 107 bytes are refused,
   and `laded` checks the widest one at startup). Pidfiles stay in `run_dir` (`$STATE/run`), whose length is free.
5. Spawns `prlimit <limits> -- bwrap <isolation> -- sh -c <gate> lade-gate rm env -u PWD -- node
   --disable-wasm-trap-handler <worker_dir>/src/worker.ts` in its own process group with piped stdin/stdout/stderr,
   with the [launch gate](#launch-gate-and-supervisor-loss) closed. While bwrap waits on the gate the launcher
   records `STATE/run/worker-<objective>-<generation>.pid` (outer bwrap, then the PID-namespace init; each line `pid
   starttime`, starttime from `/proc/<pid>/stat` field 22), moves both into the Objective's cgroup leaf when cgroups
   are delegated, and only then opens the gate. A failure of either kills the sandbox and fails the launch.
6. Starts three threads — writer, monitor, stderr forwarder — and queues `worker.start`.

Threads and bounds:

| Thread | Behaviour | Bound |
| --- | --- | --- |
| writer | writes queued service→worker JSONL frames to stdin, in order | 256 frames **and** 8 MiB queued; a full queue makes `send` fail (`WouldBlock`, delivery uncertain); a failed write marks the link gone (`BrokenPipe`) |
| monitor | `FrameReader` (worker framing, 1 MiB frames) → `decode_worker` → only worker→service bodies → `WorkerEvent::Body` on the service's bounded channel | any framing/schema failure or a service→worker body (`service_body_from_worker`) → kill sandbox, then `ProtocolViolation(code)`; EOF → 3 s grace then kill; then `Exited{code, signal}` after the process is reaped, the gateway is stopped and the pidfile removed |
| stderr | forwards worker stderr to `laded`'s stderr as `lade-worker[<objective>/<generation>]: <line>` | 512 bytes per line (cut marked `…`), control characters replaced, 100 lines/s (excess counted and reported as a number) |

`WorkerControl::terminate` (also on `Drop`): queue `worker.shutdown`, close stdin, wait 3 s; then `SIGKILL` the
namespace init (which ends every process in the PID namespace) and bwrap. Killing happens while holding the
`Child`, so an already-reaped pid is never signalled; the init pid is verified by start time. After the exit the
gateway is stopped (all tunnels closed) and the pidfile removed. `terminate` never waits for the service to drain
events, so it is safe to call from the event-draining thread.

`LauncherConfig` fields: `node` (`LADE_NODE`, an absolute path, else `node` on `PATH`), `worker_dir`
(`LADE_WORKER_DIR`, else the bundle layout `<executable dir>/../lib/lade/pi-worker`, else — debug builds only —
the repository's `packages/pi-worker`; release builds never use the compiled-in source tree),
`entry` (default `<worker_dir>/src/worker.ts`; tests point it at probe scripts), `node_args` (default
`NODE_ARGS`), `run_dir`, `socket_dir`, `limits` (default `WORKER_LIMITS`), `policies` (default `default_policies()`),
`credentials` (default `CredentialSource::from_process()`), `upstream_proxy` (default `UpstreamProxy::from_env()`),
`extra_ro`, `cgroups` (default none; `laded` passes the delegated subtree, see [cgroups](#cgroup-v2-placement)).

## Sandbox layout

From `adapters::sandbox::worker_spec` (WP-C), per launch:

| Path | Access |
| --- | --- |
| `/usr`, `/bin`, `/sbin`, `/lib*` (binds or symlinks), `/etc/{ssl,ca-certificates,resolv.conf,hosts,passwd,group,alternatives,nsswitch.conf,ld.so.cache}` | read-only |
| Node installation prefix (`<prefix>` of the canonical `node`) | read-only |
| worker package (`packages/pi-worker`, including `node_modules`) | read-only |
| repository Git common dir (`.git`) | read-only — the worker cannot commit; Rust captures |
| Objective worktree | read-write, working directory |
| Objective session dir | read-write, `HOME`, pi agent/session dirs |
| gateway socket | the only network path (`--unshare-net`; `LADE_GATEWAY_SOCKET`) |
| `/tmp` | private tmpfs of `SANDBOX_TMP_BYTES` (1 GiB, `bwrap --size`); writes beyond it fail with `ENOSPC` |
| `/proc`, `/dev` | private |

Namespaces: all unshared (`--unshare-all --unshare-user --disable-userns --unshare-net`), `--cap-drop ALL`,
`--new-session`, `--die-with-parent`, read-only root remount, and the [seccomp filter](#seccomp-filter).
Environment: exactly `HOME`, `PATH`, `LANG=C.UTF-8`, `TERM=dumb`, `LADE_GATEWAY_SOCKET`, `PI_CODING_AGENT_DIR`,
`PI_CODING_AGENT_SESSION_DIR`, `PI_OFFLINE=1`, `PI_SKIP_VERSION_CHECK=1`, `PI_TELEMETRY=0` and the selected
provider's credential variables; the `PWD` bwrap exports for `--chdir` is removed (`env -u PWD`) before the program
starts. No control socket, capability file, database or other Objective is reachable. File descriptors: only the
three pipes. std opens everything else `O_CLOEXEC`; the two pipe ends bwrap reads (the seccomp program and the
gate) are made inheritable only while one sandbox spawns (sandbox spawns are serialized by one lock), and bwrap and
the gate close both before the program starts (`adapters_sandbox.rs::only_stdio_reaches_the_sandboxed_program`). A
host helper (Git, `kill`) that `laded` starts in that instant may inherit a copy of those read ends; they hold only
the public filter program and the gate byte.

Two minimal changes to the WP-C sandbox adapter were needed:

- `SandboxSpec::secret_env`: credential variables reach bwrap through the (cleared, then explicitly populated)
  process environment of `prlimit`/`bwrap` instead of `--setenv` arguments, because `/proc/<pid>/cmdline` is
  readable by every local user while `/proc/<pid>/environ` is not. With secrets present `--clearenv` is omitted
  (the outer environment holds only the secrets); `Debug` redacts the values. `worker_spec` uses it for
  `credentials`.
- `WorkerLaunch::node_args`: Node options before the entry script (see limits below).

## Limits (§7)

All values are constants in `crates/lade-system/src/adapters/sandbox/limits.rs`, the single place to change them;
they are the delivery-plan proposals and their approval is Matias's decision (#14).

| Limit | Worker (launcher) | §7 default | Note |
| --- | --- | --- | --- |
| address space (`RLIMIT_AS`) | **8 GiB** | 4 GiB | Node 22 cannot start under 4 GiB: V8 reserves heap and code ranges up front. With the default trap-handler WebAssembly bounds checking, every WebAssembly instance (Node's type stripper, undici's HTTP parser) reserves ~10 GiB of guard regions, which fails even at 16 GiB; `--disable-wasm-trap-handler` (explicit bounds checks) avoids this. `RLIMIT_AS` bounds virtual memory, not resident memory. |
| processes (`RLIMIT_NPROC`) | 512 | 512 | per real UID, not enforced for root |
| open files | 1024 | 1024 | |
| file size | 512 MiB | 512 MiB | |
| CPU time | none | none (verification: 600 s) | |
| private `/tmp` | 1 GiB tmpfs | — | memory-backed; counts against `memory.max` under cgroups |
| writable storage (worktree + session dir) | 2 GiB monitor | 2 GiB monitor | measured every 5 s by the service; above it the agent is stopped with `limit_reached` ([service.md](service.md#resource-limits-imp-006)); not a quota |
| memory, processes/threads, CPU per Objective (cgroup v2) | 4 GiB `memory.max`, 512 `pids.max`, `cpu.max` unlimited | — | only with a delegated cgroup; shared by the Objective's worker, terminals and verification runs |

### cgroup v2 placement

`adapters::sandbox::cgroup`. When `laded` runs from the shipped unit (`Delegate=memory pids cpu`), its cgroup is
delegated: systemd sets the `user.delegate` (or `trusted.delegate`) extended attribute and chowns it to the service
user. `detect` checks, in order: a cgroup v2 membership line (`0::<path>` in `/proc/self/cgroup`), cgroup2 mounted at
`/sys/fs/cgroup`, the delegation attribute, ownership by the service's uid, and the `memory pids cpu` controllers.
Then it moves `laded` into `<unit>/service` (cgroup v2 forbids processes in inner nodes), enables the controllers on
`<unit>` and `<unit>/sandboxes`, and each Objective gets `<unit>/sandboxes/<objective>` with the limits above and one
leaf per sandbox. The launcher, terminals and verification move the outer bwrap and the PID-namespace init into the
leaf while the gate is closed (nothing else exists yet), verify membership through `/proc/<pid>/cgroup`, then open
the gate, so every descendant is accounted. The leaf is killed (`cgroup.kill`) and removed after the sandbox ends;
leftovers are removed at the next start. Any failed check gives the explicit diagnostic `cgroup limits unavailable
(<reason>); sandboxes use the prlimit limits only` and the prlimit-only behaviour; LADE never writes into a cgroup it
was not delegated and never refuses to start for lack of delegation.

Evidence: `tests/adapters_cgroup.rs` — the fallback reasons (v1/hybrid, not mounted, not delegated: nothing
written), the exact files written into a delegated subtree (on a directory standing in for cgroupfs), missing
controllers. `delegated_cgroup_contains_and_limits_a_sandbox` exercises real placement and `pids.max` only with
`LADE_TEST_DELEGATED_CGROUP=1` inside a delegated scope (`systemd-run --user --scope -p Delegate=yes …`); it has
**not been executed** (neither CI nor the development host has a delegated cgroup v2; the host is cgroup v1/hybrid).

### Seccomp filter

`adapters::sandbox::seccomp` compiles a classic-BPF program in Rust (no libseccomp, no `unsafe`); every sandbox
(worker, terminal, verification, probe) passes it to `bwrap --seccomp FD` on a pipe, and bwrap installs it (with
`PR_SET_NO_NEW_PRIVS`) as the last step before executing the program, after its own namespace and mount setup.
Architectures: x86_64 and aarch64; any other has no program, so building the command fails and the sandbox is
unavailable (fail closed).

| Rule | Result |
| --- | --- |
| foreign architecture (i386 `int 0x80` on x86_64) | process killed |
| x32 system calls (`__X32_SYSCALL_BIT`) | `EPERM` |
| `kexec_load`, `kexec_file_load`, `init_module`, `finit_module`, `delete_module` | `EPERM` |
| `mount`, `umount2`, `pivot_root`, `open_tree`, `move_mount`, `fsopen`, `fsconfig`, `fsmount`, `fspick`, `mount_setattr` | `EPERM` |
| `keyctl`, `add_key`, `request_key`, `bpf`, `perf_event_open`, `userfaultfd`, `open_by_handle_at` | `EPERM` |
| `swapon`, `swapoff`, `reboot`, `setns` | `EPERM` |
| `clone`/`unshare` with any namespace flag (`CLONE_NEWNS/CGROUP/UTS/IPC/USER/PID/NET`, `unshare` also `NEWTIME`) | `EPERM` |
| `clone3` | `ENOSYS` (its flags are in memory a filter cannot read; libc falls back to `clone`) |
| `personality` other than `PER_LINUX` (0) or the query `0xffffffff` | `EPERM` (no ASLR disabling) |
| `ioctl` `TIOCSTI`, `TIOCLINUX` (request compared as the kernel's 32-bit value) | `EPERM` |
| everything else | allowed |

`ptrace` is not filtered: a filter cannot tell a child from another process; the PID namespace already limits
targets to the sandbox's own processes. Tests: the filter's unit test runs every rule through a small BPF
interpreter; `adapters_sandbox.rs::seccomp_denies_the_reviewed_system_calls_with_eperm` (`Seccomp: 2`;
`personality(ADDR_NO_RANDOMIZE)` allowed on the host but `EPERM` inside; keyctl, unshare(CLONE_NEWUSER), mount, bpf,
setns, perf_event_open, userfaultfd, init_module, reboot `EPERM`; `unshare -U` refused);
`supervision_terminal.rs::seccomp_refuses_terminal_input_injection_and_pwd_is_the_shells_own` (`TIOCSTI` on the
terminal's own PTY gives `EPERM`); the real worker still runs end to end with the fixture model
(`supervision_launcher.rs::real_worker_writes_file_with_fixture_model_and_terminates_cleanly`, `service_flow.rs`), and
every terminal test passes under the filter.

### Launch gate and supervisor loss

Every sandbox command line contains bwrap's `--file` operation copying a gate pipe to `/tmp/.lade-gate`, and the
program is started by a small `sh` gate script. bwrap copies the pipe into the file while it sets up the sandbox, so
it blocks until the supervisor either writes `go` and closes the pipe (`Gate::open`) or loses it (the pipe closes
empty: the supervisor died or abandoned the launch). Meanwhile the sandbox is only the outer bwrap and its fork (the
future PID-namespace init); the program has not started. The gate script reads the file, removes it, and executes the
program only after `go`; an empty gate ends the sandbox with status 125. Supervisors write the pidfile and place the
cgroup before opening the gate, so:

- **the program never runs unrecorded or outside its cgroup while the supervisor lives;**
- **a supervisor that dies before opening the gate leaves nothing running** (the sandbox ends by itself);
- a supervisor that dies after opening the gate but before bwrap armed its parent-death signals (bwrap arms them
  after its clone and again just before the program, a window of microseconds in `laded`) leaves a sandbox the
  startup sweep finds by its **marker**: every sandbox command line carries `--unsetenv LADE_SANDBOX_<16 hex of
  SHA-256 of the canonical run directory>` (a no-op for the sandbox, which can neither see nor change its outer
  processes), and `sweep_stale` kills every live process of the service user carrying that argument — recorded or
  not — before the pidfile pass, repeating until none is left.

`supervision_sweep.rs::forced_supervisor_loss_races_leave_no_survivors` forces 40 races: a stand-in supervisor
(`sh`) starts the sandbox and is killed in the same instant, before any pidfile exists and before bwrap armed its
signals; half the runs never open the gate, half do. Result on the development host: none of the never-opened
sandboxes ran its program; all 20 opened ones kept running unsupervised (the worst case); after the sweep **0 of 40
survive**. `adapters_sandbox.rs::the_program_waits_for_the_gate_and_a_lost_gate_runs_nothing` checks the gate
itself. The earlier WP-C observation (about 9 of 40 forced races surviving unrecorded) is closed by these two
mechanisms; such a survivor still runs until the next `laded` start (outside systemd) or until the unit stops
(`KillMode=control-group`).

## Egress gateway

HTTP `CONNECT` proxy on the per-launch Unix socket; the worker's undici dispatcher sends
`CONNECT host:port HTTP/1.1` and runs TLS end-to-end with the origin over the tunnel.

- Request head bounded to 8 KiB (`431` beyond), 10 s handshake timeout; only `CONNECT` (`405` otherwise) with
  an explicit port and a syntactically valid DNS name or IP literal (`400` otherwise). No plaintext forwarding.
- Allowlist of exact `(host, port)` pairs after lowercasing and trailing-dot removal; anything else `403
  destination_denied`. Defaults are port 443 only. `EgressPolicy::with_extra(rules)` adds hosts (for settings).
- The host is resolved once; if **any** resolved address is loopback, private (RFC 1918), link-local,
  shared (100.64/10), unspecified/"this network", multicast, broadcast, reserved, documentation, benchmarking,
  IPv6 unique-local/site-local/link-local, or an IPv4-mapped/compatible/NAT64 form of those, the request is
  refused (`403 private_address`) unless the matching rule sets `allow_private` (custom local providers). The
  connection goes to one of the checked addresses — never re-resolved.
- Worker side (`packages/pi-worker/src/worker/gateway.ts`): undici's global dispatcher (`fetch`) **and** the
  `node:http`/`node:https` global agents tunnel every connection through the socket; `https` runs TLS end-to-end
  over the tunnel with the request's own TLS options (custom `ca`, client certificates; a custom
  `checkServerIdentity` is replaced by Node's default check). Clients that bring their own agent or `agent: false`
  bypass the global agents and have no network. Without a socket both paths fail with `network_unavailable`.
  Evidence: `supervision_gateway.rs::worker_https_reaches_a_local_tls_origin_through_the_gateway` (the real Rust
  gateway, a TLS origin with a certificate generated in the test by the `openssl` CLI and trusted only through
  `NODE_EXTRA_CA_CERTS`: `fetch` and `https.get` complete over TLS; a port outside the policy is refused for both),
  `packages/pi-worker/test/worker-gateway.test.ts` (`node:http` through the agent, refusal, no network).
- Upstream proxy: when `laded` runs with `HTTPS_PROXY`/`https_proxy` (`http://[user:pass@]host[:port]`), tunnels
  are chained with `CONNECT` through it (`Proxy-Authorization: Basic` from the URL's userinfo), after the
  allowlist check. `NO_PROXY` minimal semantics: `*`, exact host/IP, and domain suffixes (`example.com`,
  `.example.com`, `*.example.com`); CIDR entries are not interpreted. Through a proxy, names are not resolved
  locally, so the private-address check applies only to IP literals (the proxy resolves).
- At most 32 concurrent connections per gateway (`503`). Bytes are copied by two threads with 16 KiB buffers.
- `Gateway::stop` (on worker exit, on `terminate`, on drop) removes the socket and shuts down every handshaking
  and open tunnel: revocation closes access that was already granted.
- Diagnostics: `lade-gateway[<12 hex>-<gen>]: host:port <decision>` on stderr; never headers or payload.

Default provider policies (pi provider ids from pi-ai 0.85.1; hosts are the base URLs of its built-in catalogues
in `dist/providers/data/*.json`):

| Provider | Host (443) | Credential variables (pi `env-api-keys.js`) |
| --- | --- | --- |
| `lade-fixture` | none (offline) | none |
| `anthropic` | `api.anthropic.com` | `ANTHROPIC_API_KEY`, `ANTHROPIC_OAUTH_TOKEN`, `ANTHROPIC_AUTH_TOKEN` |
| `openai` | `api.openai.com` | `OPENAI_API_KEY` |
| `google` | `generativelanguage.googleapis.com` | `GEMINI_API_KEY` |
| `openrouter` | `openrouter.ai` | `OPENROUTER_API_KEY` |
| `mistral` | `api.mistral.ai` | `MISTRAL_API_KEY` |
| `groq` | `api.groq.com` | `GROQ_API_KEY` |
| `xai` | `api.x.ai` | `XAI_API_KEY` |
| `deepseek` | `api.deepseek.com` | `DEEPSEEK_API_KEY` |
| `cerebras` | `api.cerebras.ai` | `CEREBRAS_API_KEY` |
| `together` | `api.together.ai` | `TOGETHER_API_KEY` |
| `fireworks` | `api.fireworks.ai` | `FIREWORKS_API_KEY` |

Providers whose pi authentication is not a plain variable (Bedrock profiles, Vertex ADC, OAuth logins such as
Copilot/Codex) have no entry and are unavailable.

## Credentials

`CredentialSource` holds the known credential variables of `laded`'s environment (snapshot at construction; other
variables are not retained) and the optional file `$XDG_CONFIG_HOME/lade/credentials.env`
(`~/.config/lade/credentials.env`). The file is re-read on each resolution and refused (`InsecureFile`) unless it
is a regular file (not a symlink), owned by the service's uid, without group/other permission bits (0600/0400),
and at most 64 KiB; there is no fallback location. Format: `NAME=value` lines, optional `export `, `#` comments,
optional matching quotes; unknown names are ignored. The environment wins over the file. Only the selected
provider's variables are released; values containing NUL/CR/LF are ignored. Values never appear in logs, errors
or `Debug` output (`Secret`, `ReleasedCredentials`, `CredentialSource`, `WorkerLaunch`, `SandboxSpec` redact).

The released key is readable by the worker process and every tool it runs inside the sandbox (in-process exposure
accepted by issue #8). Secret Service / keyring integration is a follow-up.

## Sweep

`sweep_stale(run_dir) -> SweepReport` must run at service start before any launch. First it sends `SIGKILL`
(rustix, no `unsafe`) to every live process of the service user whose command line carries the run directory's
[marker](#launch-gate-and-supervisor-loss) as a whole argument (outer `prlimit`/`bwrap` and bwrap's forks, the
namespace init among them), repeating for up to 3 s until none is left. Then, for every `worker-*.pid` and
`terminal-*.pid`, it sends `SIGKILL` (via `kill`, no `unsafe`) to each recorded process whose pid **and** start time
still match a live, non-zombie process — namespace init first — and skips the rest (pid reuse); then it removes
all worker pidfiles and legacy `gw-*.sock` files. The report lists killed and skipped pids and the removed counts.
`sweep_sockets(socket_dir)` then removes the gateway sockets left in `$RUNTIME/g`.

## Contained terminal (IMP-012)

`supervision::terminal::Terminal::open` runs an interactive shell for the developer under the same isolation as a
worker; the service (`service/terminal.rs`) is the only caller and the desktop client has no other way to start a
process.

1. The service allocates the PTY: `posix_openpt(O_RDWR|O_NOCTTY|O_CLOEXEC)`, `grantpt`, `unlockpt`, window size,
   then the user side through `TIOCGPTPEER` (no path lookup) — all via rustix (`pty`, `termios` features), no
   `unsafe`. The service never acquires it as a controlling terminal.
2. `adapters::sandbox::terminal_spec`: `prlimit <worker limits> -- bwrap <the worker isolation> --
   /usr/bin/setsid --ctty --wait /usr/bin/bash -i` (first of `/usr/bin/bash`, `/bin/bash`, `/usr/bin/sh`, `/bin/sh`;
   without `setsid` the shell runs without a controlling terminal). The user side is the shell's stdin/stdout/stderr;
   the service's copies are closed right after the spawn, nothing else is inherited. Inside, `setsid --ctty` makes
   the PTY the shell's controlling terminal (job control, `^C` reaches the foreground job). The PTY is private to
   this terminal, so `TIOCSTI` from inside can only inject into its own input; bwrap's `--new-session` still
   separates the sandbox from any terminal of `laded`.
3. Pidfile `$STATE/run/terminal-<objective>-<terminal>.pid` (outer bwrap, then the PID-namespace init), written
   (and the cgroup leaf placed) while the launch gate is closed and read by the startup sweep exactly like worker
   pidfiles; the command line carries the sweep marker.

| Path / property | Terminal |
| --- | --- |
| system dirs, `/etc` files, `/proc`, `/dev`, private `/tmp` | as for the worker |
| repository Git common dir | read-only (no commits, no ref moves) |
| Objective worktree | read-write while `defined`/`active`; **read-only** while a freeze runs and for `frozen`/`approved` candidates |
| session dir, worker package, Node, gateway socket, credentials | **not bound / not set** |
| network | none (`--unshare-net`, no gateway) |
| environment | `HOME=/tmp`, `PATH=/usr/bin:/bin`, `LANG=C.UTF-8`, `TERM=vt100`, `PS1`, `LADE_TERMINAL=read_write\|read_only` (bwrap's `PWD` removed; the shell sets its own) |
| limits | `TERMINAL_LIMITS` (= `Limits::WORKER`) through `prlimit`: 4 GiB address space, 512 processes, 1024 files, 512 MiB file size; the seccomp filter (no `TIOCSTI`); the Objective's cgroup when delegated |

Teardown: `Terminal::kill` (explicit close, freeze, lifecycle, lease loss, `service.stop`, shutdown, `Drop`) sends
`SIGKILL` to the PID-namespace init — ending every process of the terminal, including `setsid`/double-forked
daemons — and to bwrap, holding the `Child` so a reaped pid is never signalled, waits for the exit and removes the
pidfile. A shell that exits ends `setsid --wait`, then the namespace init, which ends the namespace the same way.

Bounds: input queue 256 frames / 256 KiB (`WouldBlock` beyond); output ring 1 MiB per terminal with up to 100 ms of
backpressure before the oldest bytes are discarded and counted; no output is logged or stored.

Tests: `tests/supervision_terminal.rs` — PTY and controlling terminal inside the sandbox, `ulimit -n/-u/-v` equal the
worker limits, no credentials/network/host paths, job control, writes land in the worktree, a read-only terminal
cannot change or create worktree files, `kill` ends a `setsid nohup` double-forked daemon and removes the pidfile,
exit status, resize (`stty size`), and a `/dev/zero` flood that overflows the ring and is counted while `^C` still
interrupts it.

## Not enforced (feeds limitations.md)

- **Disk usage**: the 2 GiB writable-storage limit is a periodic monitor (every 5 s), not a quota: a burst between two
  checks can exceed it, and nothing reserves disk for `~/.local/state/lade`. True quotas remain an installation gate.
- **Memory**: without a delegated cgroup only `RLIMIT_AS` (8 GiB virtual for the worker, 4 GiB for terminals)
  applies; resident memory and `/tmp` tmpfs pages are then not capped per Objective or in aggregate. With delegation
  the per-Objective `memory.max` applies; no aggregate limit across Objectives is set.
- **Process count**: without delegation `RLIMIT_NPROC` is per real UID (shared with the user's other processes) and
  not enforced for root; `pids.max` applies only with delegation.
- **CPU**: no CPU ceiling for workers or terminals (§7 proposes none; `cpu.max` is configurable).
- **Delegated cgroup path unexecuted**: the real placement test needs a delegated cgroup v2, which neither CI nor the
  development host provides.
- **Supervisor-loss window**: a sandbox whose supervisor died after opening the gate but before bwrap armed its
  parent-death signals keeps running until the next `laded` start (startup sweep by marker) or until the unit stops.
- **Seccomp scope**: a denylist; `ptrace` and `io_uring` are not filtered; the remaining kernel attack surface is that
  of an unprivileged user namespace.
- **Network scope**: the allowlist is per provider host, not per request; any code in the sandbox (tools,
  extensions) can use the provider tunnel with the released key. Destination filtering is not data-loss
  prevention: content sent to an allowed host is not inspected. Through an upstream proxy, DNS-based
  private-address checks are delegated to the proxy.
- **Per-extension secrecy**: none; every process in the sandbox can read the credential variables.
- **Tool network**: `fetch`/undici and `node:http(s)` clients using the global agents are tunnelled; clients with
  their own agent, raw sockets, other runtimes (Python, curl) and UDP have no network at all (not a bypass, a
  functional limitation).
- **Worker output**: frames are schema-validated; content of valid frames (activity text) is untrusted and not
  otherwise filtered.
- **Terminal quotas**: without delegation the terminal gets the per-process `prlimit` values, not an aggregate
  per-Objective budget, and `RLIMIT_NPROC` is still per real UID (root: not enforced). With delegation the terminal
  shares the Objective's cgroup limits with the agent.
- **Terminal and agent write concurrently** while the Objective is active: the terminal is a second writer of the
  worktree until a freeze excludes both. Changes a terminal makes appear in the Change Set like the agent's (and
  count toward the writable-storage budget).
- **Terminal output** is untrusted bytes shown as text; escape sequences are interpreted by the client's minimal
  VT parser only (no clipboard, title or hyperlink effects).
