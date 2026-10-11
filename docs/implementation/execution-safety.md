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
5. Spawns `prlimit <limits> -- bwrap <isolation> -- node --disable-wasm-trap-handler <worker_dir>/src/worker.ts`
   in its own process group with piped stdin/stdout/stderr, and records
   `STATE/run/worker-<objective>-<generation>.pid` (outer bwrap, then the PID-namespace init once bwrap forked it;
   each line `pid starttime`, starttime from `/proc/<pid>/stat` field 22).
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
`extra_ro`.

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
| `/tmp`, `/proc`, `/dev` | private |

Namespaces: all unshared (`--unshare-all --unshare-user --disable-userns --unshare-net`), `--cap-drop ALL`,
`--new-session`, `--die-with-parent`, read-only root remount. Environment: exactly `HOME`, `PATH`, `LANG=C.UTF-8`,
`TERM=dumb`, `LADE_GATEWAY_SOCKET`, `PI_CODING_AGENT_DIR`, `PI_CODING_AGENT_SESSION_DIR`, `PI_OFFLINE=1`,
`PI_SKIP_VERSION_CHECK=1`, `PI_TELEMETRY=0`, the selected provider's credential variables, plus `PWD` exported
by bwrap. No control socket, capability file, database or other Objective is reachable. File descriptors: only
the three pipes (std opens everything else `O_CLOEXEC`).

Two minimal changes to the WP-C sandbox adapter were needed:

- `SandboxSpec::secret_env`: credential variables reach bwrap through the (cleared, then explicitly populated)
  process environment of `prlimit`/`bwrap` instead of `--setenv` arguments, because `/proc/<pid>/cmdline` is
  readable by every local user while `/proc/<pid>/environ` is not. With secrets present `--clearenv` is omitted
  (the outer environment holds only the secrets); `Debug` redacts the values. `worker_spec` uses it for
  `credentials`.
- `WorkerLaunch::node_args`: Node options before the entry script (see limits below).

## Limits (§7)

| Limit | Worker (launcher) | §7 default | Note |
| --- | --- | --- | --- |
| address space (`RLIMIT_AS`) | **8 GiB** | 4 GiB | Node 22 cannot start under 4 GiB: V8 reserves heap and code ranges up front. With the default trap-handler WebAssembly bounds checking, every WebAssembly instance (Node's type stripper, undici's HTTP parser) reserves ~10 GiB of guard regions, which fails even at 16 GiB; `--disable-wasm-trap-handler` (explicit bounds checks) avoids this. `RLIMIT_AS` bounds virtual memory, not resident memory. |
| processes (`RLIMIT_NPROC`) | 512 | 512 | per real UID, not enforced for root |
| open files | 1024 | 1024 | |
| file size | 512 MiB | 512 MiB | |
| CPU time | none | none (verification: 600 s) | |

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

`sweep_stale(run_dir) -> SweepReport` must run at service start before any launch. For every
`worker-*.pid` it sends `SIGKILL` (via `kill`, no `unsafe`) to each recorded process whose pid **and** start time
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
3. Pidfile `$STATE/run/terminal-<objective>-<terminal>.pid` (outer bwrap, then the PID-namespace init), read by
   the startup sweep exactly like worker pidfiles.

| Path / property | Terminal |
| --- | --- |
| system dirs, `/etc` files, `/proc`, `/dev`, private `/tmp` | as for the worker |
| repository Git common dir | read-only (no commits, no ref moves) |
| Objective worktree | read-write while `defined`/`active`; **read-only** while a freeze runs and for `frozen`/`approved` candidates |
| session dir, worker package, Node, gateway socket, credentials | **not bound / not set** |
| network | none (`--unshare-net`, no gateway) |
| environment | `HOME=/tmp`, `PATH=/usr/bin:/bin`, `LANG=C.UTF-8`, `TERM=vt100`, `PS1`, `LADE_TERMINAL=read_write\|read_only` (+ `PWD` from bwrap) |
| limits | `Limits::WORKER` through `prlimit`: 4 GiB address space, 512 processes, 1024 files, 512 MiB file size |

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

- **Disk usage**: no quota on the worktree, session dir or `/tmp` tmpfs (tmpfs is memory-backed). The §7 2 GiB
  monitor belongs to the service; true quotas are an installation gate.
- **Memory**: `RLIMIT_AS` (8 GiB, not 4 GiB) limits virtual address space only; no cgroup memory limit.
- **Process count**: `RLIMIT_NPROC` is per real UID (shared with the user's other processes) and not enforced for
  root.
- **Supervisor-loss race**: if `laded` dies in the first milliseconds of a launch, before bwrap armed its
  parent-death signal, or before the pidfile (or its namespace-init line) was written, the sandbox can survive
  unrecorded; the startup sweep covers only recorded processes.
- **Network scope**: the allowlist is per provider host, not per request; any code in the sandbox (tools,
  extensions) can use the provider tunnel with the released key. Destination filtering is not data-loss
  prevention: content sent to an allowed host is not inspected. Through an upstream proxy, DNS-based
  private-address checks are delegated to the proxy.
- **Per-extension secrecy**: none; every process in the sandbox can read the credential variables.
- **Tool network**: only undici/`fetch` traffic is tunnelled; other clients have no network at all (not a
  bypass, a functional limitation).
- **Worker output**: frames are schema-validated; content of valid frames (activity text) is untrusted and not
  otherwise filtered.
- **Terminal quotas** are the worker's per-process `prlimit` values, not an aggregate per-Objective budget: a terminal
  and the agent of the same Objective each get them, and `RLIMIT_NPROC` is still per real UID (root: not enforced).
  No CPU limit, no disk quota (as for workers).
- **Terminal and agent write concurrently** while the Objective is active: the terminal is a second writer of the
  worktree until a freeze excludes both. Changes a terminal makes appear in the Change Set like the agent's.
- **Terminal output** is untrusted bytes shown as text; escape sequences are interpreted by the client's minimal
  VT parser only (no clipboard, title or hyperlink effects).
