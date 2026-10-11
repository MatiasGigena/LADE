# The `laded` service (WP-F1)

`laded` is the authoritative System Layer process: it owns product identities, the product store, Objective
lifecycles, Git mutations, worker supervision and client admission. It instantiates [delivery-plan §1–§4](delivery-plan.md)
over the domain rules in `lade-core`, the wire contract in [protocol-v1.md](protocol-v1.md), the adapters in
`lade-system::adapters` and the launcher contract in `lade-system::supervision` (see [execution-safety.md](execution-safety.md)).
Source: `crates/lade-system/src/{bin/laded.rs,service,storage,transport}`.

```sh
laded --version
laded serve [--state-dir DIR] [--runtime-dir DIR]
```

Defaults: state `$XDG_STATE_HOME/lade` (`~/.local/state/lade`), runtime `$XDG_RUNTIME_DIR/lade` (fallback
`$STATE/run`). Both must be absolute. Exit statuses: `0` version, `64` usage, `78` refused to start (unsafe directory,
unopenable/corrupt/newer store, another instance holds the state directory), `70` internal. The process sets umask
`077`. It runs until the controlling client asks it to stop (`service.stop`, explicit Exit or logout: exit status
`0`, see [Lifetime](#lifetime-close-exit-logout)) or until killed; `SIGTERM` is not trapped (no `unsafe` signal
handlers) — workers and terminals die with their sandbox (`--die-with-parent`) and the next start recovers.

## State layout

| Path | Content |
| --- | --- |
| `$STATE/lade.db` (+`-wal`, `-shm`) | SQLite product store (0600) |
| `$STATE/lade.lock` | `flock` held for the service lifetime: one instance per state directory |
| `$STATE/artifacts/sha256/xx/…` | immutable content artifacts (file bytes served by `file.read`/`diff.get`, candidate manifests, verification output) |
| `$STATE/worktrees/<objective>/` | Objective worktrees (lazily provisioned on first execution) |
| `$STATE/sessions/<objective>/` | pi agent dir and session files of the Objective |
| `$STATE/verify/<verification>/` | disposable verification copies (removed after each run and swept on start) |
| `$STATE/run/` | launcher and terminal pidfiles (`worker-*.pid`, `terminal-*.pid`; swept on start) |
| `$STATE/tmp/` | temporary capture indexes and artifact staging (cleared on start) |
| `$RUNTIME/control.cap`, `control.sock` | admission capability and control socket (0600, directory 0700) |
| `$RUNTIME/g/<12 hex>-<gen>.sock` | per-launch gateway sockets (0600, directory 0700; swept on start) |

Unix socket paths hold at most 107 bytes. Gateway sockets therefore live under the runtime directory with short
names, so any state directory length works; at startup `laded` refuses (exit 78, before creating anything) a runtime
directory whose `control.sock` or widest gateway socket path (generation up to 9,999,999) would not fit, naming the
path and asking for a shorter `--runtime-dir`.

Git-side state lives in the Workspace repository: Objective branches `lade/<short-id>` (suffix `-2`… on collision),
retention refs `refs/lade/retain/{capture-<id>,candidate-<id>,candidate-<id>-tree}` that pin every inspected capture
and candidate against garbage collection (never removed by Cleanup), and temporary `lade/verify/<id>` branches for
verification copies.

## Storage

One connection, opened and owned by the service thread: `journal_mode=WAL`, `synchronous=FULL`, `foreign_keys=ON`,
`busy_timeout` 5 s. Opening runs `PRAGMA quick_check`; a failure, a non-WAL journal or a schema version newer than this
build refuses to start — the database is never reset or rewritten. Migrations are an append-only ordered list recorded
in `schema_migrations(version, applied_at)`, each applied in its own transaction.

**Dependency:** `rusqlite =0.40.2` with `bundled` (SQLite compiled in, so behaviour does not depend on the host
library version). It is allowed for `lade-system` only; `scripts/check-architecture.py` still rejects any path from
`lade-desktop` to SQLite. `rustix =1.1.4` (`net`, `process`, `fs`, `pty`, `termios`) provides `SO_PEERCRED`, `getuid`, `umask`, `flock`,
`kill` and the terminal PTY (`posix_openpt`, `TIOCGPTPEER`, window size) without `unsafe`; `serde_json` (already reviewed for the protocol) encodes stored outcomes and references.

Schema v1 tables:

| Table | Holds |
| --- | --- |
| `settings` | the wire `Settings` JSON (default model, verification commands) |
| `workspaces` | id, canonical top-level path (bytes), name, default branch |
| `objectives` | every `ObjectiveRecord` field (intent, revisions, target, start commit, lifecycle, execution state/generation/session association/model, cleaned_up, timestamps) plus the real Git branch, worktree path and the integration-conflict fact |
| `sessions` | each session association (session id, association revision, model, last generation, pi session id/file) |
| `captures` | capture id → tree id, target commit, change counts, retention ref |
| `drafts`, `draft_revisions` | current draft (reference JSON, body, CAS revision, state, submitted input) and every saved revision |
| `accepted_inputs` | immutable accepted input (items with frozen references, note, rendered text); `UPDATE`/`DELETE` triggers abort |
| `input_delivery` | delivery state per input, separate from acceptance |
| `operations` | operation id → canonical request digest, name, state (`pending`/`completed`/`uncertain`), recorded outcome JSON |
| `candidates`, `verifications`, `approvals` | review records (verification output tail kept for the summary; full output is an artifact) |
| `activity` | activity items per Objective `seq`, upserted by `item_id`, text ≤ 192 KiB |
| `integration_journal` | intent (expected target, candidate tree, message, commit time) before mutation, outcome after |

Never stored: the capability, provider credentials, environment values. Diagnostics go to stderr with codes and ids
only, never prompt, source or secret bodies.

### What is durable

Durable before the reply: every successful mutating command's state change **and** its outcome in the operations
journal in the same transaction (accepted input, submitted draft revisions and the outcome are one transaction);
recorded rejections; drafts (CAS); captures; review records; delivery transitions (`dispatched` is recorded before
the frame is written to the worker); integration intent before the Git mutation.

Not durable: streamed activity deltas between flushes (items are upserted when the worker sends them and streamed
text is flushed every second, so a crash can lose up to ~1 s of streamed text), the event sequence (per service epoch),
the controlling lease, draft staleness flags (recomputed by the next capture), running background jobs.

## Admission (transport)

Bootstrap order: validate or create the runtime directory (real directory, owned by the service uid, no group/other
bits — otherwise refuse), write 32 random bytes from `/dev/urandom` as 64 hex to `control.cap.tmp` (0600, fsync),
rename to `control.cap`, fsync the directory, then bind `control.sock` and chmod it 0600. An existing socket is
removed only after a connect probe fails (a live service makes the new one refuse). Capability and `service_epoch`
rotate on every start.

Per connection: the peer uid from `SO_PEERCRED` must equal the service uid; at most 8 handshakes run at once with a
10 s timeout. The first frame must be `hello` (answered with `hello`), the second `auth`; the capability is compared in
constant time. Anything else, or a wrong capability, closes the link without a reply. The authenticated link is
handed to the service, which grants the single controlling lease (`welcome {service_epoch, lease_id}`) or — when a
lease is held — sends one `service.notice {level: error, code: "unauthorized"}` event (sequence `0`) and closes the
link. The lease frees when its link closes; the next client re-queries `snapshot`.

After admission, frames are decoded with `decode_client_frame`: a schema-invalid request with a readable
`request_id` is answered `invalid_request` and the link stays; any other decode failure, a second `hello`, `auth`
or `chunk.*` closes it. Each link has a reader thread and a writer thread fed by a bounded queue (256 frames /
8 MiB). The service never blocks on a client: on overflow the queue is discarded, a `service.notice
{code: "link_overflow"}` is sent and the link closes; the client resyncs from `snapshot`.

## Threading model

```text
accept thread ── per-link reader ─┐
tick thread (100 ms) ─────────────┤   (the tick also drains terminal output, see below)
supervisor forwarder (launcher) ──┼─► service queue ─► service thread (state + store) ─► per-link writer
pool (3 threads) job results ─────┘                         │
                                                            └─► pool: Git capture/diff/read/search, freeze,
                                                                refresh, verification, integration,
                                                                provisioning + launch, cleanup
```

The service thread owns all in-memory state and the only store connection. Mutations follow
clone → domain transition → commit (short transaction) → swap, so a failed write leaves memory unchanged. Long work
runs on the pool with owned inputs and posts its result back; on completion the service revalidates (lifecycle,
execution liveness, candidate status, generation) before committing. A long mutating command holds a per-Objective
`busy` marker: other mutating commands on that Objective are rejected `conflict` until it completes (queries
continue). Short Git reads (target resolution for details, `workspace.open`, branch lists) run inline.

## Requests

Every request name in protocol-v1 is implemented.

Queries: `snapshot` (≤ 64 workspaces with ≤ 64 branches each, ≤ 400 most recent objective summaries with the outcome
clipped to 512 chars, settings, current sequence); `objective.get` (detail with a freshly resolved target commit,
the latest ≤ 64 candidates/verifications and `available_actions`); `activity.page` (items before `seq`, newest last,
≤ 768 KiB of text per page, live streamed text included); `changes.list` (working: a fresh capture; candidate:
candidate tree vs its base target commit; since: tree vs tree); `file.read` and `diff.get` (working uses the latest
capture, or captures first; whole file bytes ≤ 64 MiB are retained in the artifact store and their digest returned;
UTF-8 pages are cut at character boundaries and kept under the frame budget, binary content is reported as binary;
diffs are bounded to 1,800 lines / 640 KiB with `truncated`); `search` (objectives by title/outcome; file paths via
`ls_tree` and content via `git grep` over the basis tree, or the workspace default branch without an objective, with a
`scope_label` naming the version); `drafts.list`, `inputs.list`; `operation.get` (journal state: `unknown`,
`pending`, `completed`, `uncertain`, with the recorded outcome).

Commands (all journaled; a duplicate `operation_id` with the same canonical digest returns the recorded reply without
re-executing, a different digest is `duplicate_conflict`, a duplicate while pending is `conflict`/`query_operation`;
a reused `request_id` with different content is `duplicate_conflict`):

- `workspace.open`: `Repository::open` (top level, non-bare, supported Git); idempotent per canonical path.
- `objective.create`: target branch must exist; start = target tip or an explicit commit of the repository.
- `objective.update_intent`: core CAS on `intent_revision`.
- `execution.start` / `execution.resume`: after the core transition is checked, the launcher's `preflight(provider)`
  (sandbox probe, egress policy, credentials) runs; a failure rejects synchronously with `execution_unavailable`
  (detail names missing credential variables, never values) and nothing is recorded. Otherwise the core transition
  (new session / new generation) is committed first; then the
  pool provisions the worktree (`worktree_add` of the Objective branch at the start commit under
  `$STATE/worktrees/<id>`; never the shared checkout) and calls the launcher with `worker.start`. Launch failure →
  execution `failed` with the reason, notice and a system activity item. Resume re-queues input blocked only by the
  stopped execution (`blocked: execution_unavailable`) of the same association — never uncertain input.
- `execution.cancel`: core `request_stop` (Stopping), `session.abort` to the worker, then `terminate` on the pool; the
  observed exit moves the execution to `cancelled`.
- `message.send`: durable acceptance (`accept_message`) then ordered dispatch.
- `draft.save` / `draft.delete`: core CAS; the reference's capture/candidate must belong to the Objective and its
  artifact must exist with the selected bytes at the selected range.
- `feedback.submit`: association/lifecycle/revisions checked, then a fresh capture recomputes the stale rule for
  every item against current content (new side: working tree; old side: target commit). Any stale item rejects the
  whole batch `stale_reference` listing the stale draft ids; otherwise acceptance, submitted drafts and outcome are
  one transaction, then dispatch.
- `review.freeze`: stops a live execution first (abort + terminate, waits up to 30 s for the observed exit, else
  `interrupted`), captures the worktree, `commit_tree` with parent = start commit, merges onto a moved target
  (`merge_tree`; conflict refuses), refuses an empty Change Set (`invalid_lifecycle`, "No changes to freeze —
  complete without changes instead"; checked against the latest capture before stopping the execution, and again
  on the exact content), stores a manifest artifact, CAS-moves the Objective
  branch, re-points the worktree's `HEAD` at it and refreshes the worktree's own index (`read-tree` +
  `update-index --refresh`, best effort, so `git status` inside the worktree is clean), adds retention refs, re-captures and **invalidates** the candidate if the content changed meanwhile.
- `review.verify`: requires a working sandbox (`execution_unavailable` otherwise); records `running`, then on the pool
  creates a verification commit of the candidate tree, `worktree add` on a temporary branch into
  `$STATE/verify/<id>`, runs the argv (`argv[0]` resolved in `/usr/bin`, `/bin`) through `verification_spec` +
  `run_bounded` (900 s, 4 MiB tail), stores the output as an artifact, removes the copy and branch, and records the
  result (`verification.changed`).
- `review.approve`: core rules against the target commit resolved fresh.
- `review.return`: core return for revision (approval invalidated, candidate historical).
- `review.refresh`: `merge_tree(current target, candidate commit)` → new candidate (re-verify, re-approve) or conflict
  (returned for revision, `stale_target` rejection, `integration_conflict` badge).
- `review.integrate`: core `check_integration` against the fresh target (a moved target invalidates the approval
  durably and rejects `stale_target`); the intent (expected target, tree, message, commit time) is journaled; then
  `integrate_squash` (CAS ref update + `read-tree -m -u` in a clean checked-out target; dirty/busy targets refuse with
  `conflict` and stay untouched); success → Integrated, approval consumed, journal `done`.
- `objective.complete`: requires an empty Change Set (captured on the pool); `objective.discard`: core discard.
- `objective.cleanup`: core cleanup rules; removes the worktree (without confirmation only when its content equals
  the branch commit) and CAS-deletes the branch; records, retention refs, artifacts and session files stay.
- `settings.update`: validated and persisted.
- `service.stop {reason: exit|logout}`: journaled and answered, then the service thread leaves its loop: every
  terminal is killed, every worker is terminated (sandbox killed), the store is closed, the socket and capability
  are removed and `laded` exits `0`. Durable records stay as they are; the next start's recovery marks the live
  executions `cancelled` + interrupted and nothing resumes.

Terminal requests (`terminal.open/input/resize/close`, IMP-012) are interaction, not operations: they carry no
`operation_id`, are never journaled or replayed, and create nothing durable. See
[Contained terminals](#contained-terminals-imp-012).

`available_actions` is computed for every `ActionName` from the same domain rules (lifecycle, execution state,
current candidate, evidence, target movement, busy marker, latest capture, verification sandbox) with a reason when
disabled; approval with missing or failed evidence is enabled with a reason stating that an override reason is
required, and input (`message.send`, `feedback.submit`) to a stopped execution is enabled with a reason saying it waits
for Resume (it is accepted with delivery `blocked: execution_unavailable`). `tests/service_actions.rs` checks, for ten
Objective states, that every enabled action's request is accepted and every disabled action's request is rejected.

Events carry a monotonically increasing `sequence` per service epoch: `objective.changed` + `objective.detail` after
every Objective change, `activity.appended` per item, `activity.delta` coalesced per 100 ms tick, `draft.changed`,
`input.changed`, `worktree.changed` per new capture, `verification.changed`, `service.notice`.

## Supervision, dispatch and attention

Supervisor events are accepted only for the Objective's current generation (stale generations are ignored).
`worker.ready` → Idle and the pi session id is recorded; `input.received` → delivery `received`; `activity.item` /
`activity.delta` → persisted/broadcast; `turn.state` → running/idle, and idle triggers a working capture (deduplicated
by tree), `worktree.changed`, draft staleness and attention; `worker.error` → notice + system activity item; a
protocol violation → notice; `Exited` → `cancelled` after a stop request, otherwise `failed` — and every
`dispatched` input of that worker becomes `uncertain`.

Dispatch is strictly in acceptance order and revalidated at dispatch time (association, lifecycle, execution
authority; feedback items are re-checked against a fresh capture unless they were validated in the same turn). The
`dispatched` state is committed before the frame is written; a write failure marks the input `uncertain`. Uncertain
input is never resent automatically.

Attention uses `lade_core::derive_attention` with facts the service tracks (uncertain delivery, stale unsent drafts,
failed checks, integration conflict, cached target movement, non-empty latest capture); wire badges add
`verification_missing`, `unsent_drafts` and `fixture_model`.

## Contained terminals (IMP-012)

Source: `service/terminal.rs` (bookkeeping) over `supervision/terminal.rs` (PTY and sandbox; see
[execution-safety.md](execution-safety.md#contained-terminal-imp-012)). The client cannot start a process: it can
only ask the service, which allocates the PTY itself and starts the shell in the Objective's bubblewrap sandbox with
the worker's `prlimit` quotas.

- **Scope.** `terminal.open {objective_id, cols, rows}` (sizes 1..=500 × 1..=200) is accepted only for an Objective
  whose worktree exists (`invalid_lifecycle` "start the agent once" otherwise, and after Cleanup) and whose lifecycle
  is `defined`/`active` (read-write) or `frozen`/`approved` (**read-only**: the worktree is bound read-only). Any
  other lifecycle is `invalid_lifecycle`. Without a working sandbox: `execution_unavailable` — there is no
  unsandboxed terminal. At most 4 terminals per Objective and 16 in total (`limit_reached`). The sandbox starts on
  the pool; on completion the service revalidates the lease, the lifecycle and writer exclusion and kills a terminal
  whose scope changed meanwhile (`conflict`).
- **Review writer exclusion.** `review.freeze` kills every writable terminal of the Objective — waiting for its
  sandbox to end — before the execution is stopped and the worktree captured (`terminal.closed {reason:
  review_freeze}`); while the freeze runs and while a candidate is frozen or approved only read-only terminals open.
  After every Objective change the service closes terminals the Objective no longer admits (`reason: lifecycle`).
- **Lease.** Terminals belong to the controlling lease that opened them. When that link closes (window closed,
  client crashed, link overflow) every terminal of it is killed; a later client never sees them. `service.stop` and
  service shutdown kill all of them. They are not stored and **never resumed**; the startup sweep kills any recorded
  terminal sandbox that survived a crash (`terminal-*.pid`).
- **I/O.** `terminal.input {terminal_id, data ≤ 16,384 code points}` queues bytes on a bounded per-terminal queue
  (256 KiB; `limit_reached` when the shell is not reading); `terminal.resize` sets the PTY window size (the kernel
  sends `SIGWINCH`); `terminal.close` kills the terminal (`terminal.closed {reason: closed}`).
- **Output.** A reader thread copies PTY output into a 1 MiB ring per terminal. Every 100 ms tick the service sends
  at most 2 × 32 KiB per terminal as `terminal.output {seq, data, dropped_bytes}`: `seq` increases by one per event,
  `data` is UTF-8 (incomplete sequences carried to the next chunk, invalid bytes replaced, NUL dropped). When the ring
  is full the reader waits up to 100 ms for the tick (backpressure: a flooding command is slowed to ≈ 640 KiB/s),
  then discards the oldest bytes and reports their count in `dropped_bytes`. Output is never written anywhere else
  (no diagnostic copy, nothing on disk).
- **End.** When the shell (and with it the sandbox) ends, the remaining output is sent, then
  `terminal.closed {reason: exited, exit_code | signal}`.

Evidence: `tests/supervision_terminal.rs` (real PTY + sandbox: quotas, grants, job control, read-only bind,
detached-descendant teardown, exit status, resize, flood ring), `tests/service_terminal.rs` (real service: scope,
freeze exclusion, read-only review terminal, lease loss, bounded flood rate, `service.stop` and restart),
`scripts/e2e-terminal` (native client under Xvfb).

## Lifetime: close, Exit, logout

| Event | Service | Agents (workers) | Terminals |
| --- | --- | --- | --- |
| Window closed / client ends | keeps running; the lease is freed | keep running (authorized background work) | killed with the lease |
| Explicit Exit in the window (`service.stop {reason: exit}`) | answers, stops, exits `0` (`Restart=on-failure` does not restart it) | sandboxes killed; recorded as interrupted on the next start, never resumed | killed |
| Session logout, client running (`query-end` → `service.stop {reason: logout}`) | as Exit | as Exit | as Exit |
| Session logout, no client / under systemd | the user manager stops `lade.service` (`SIGTERM`, then `KillMode=control-group` ends every descendant) | killed with the control group; interrupted on the next start | killed |
| `laded` started by hand (no systemd) and the session ends without a client | keeps running until killed (not trapped) | keep running | none (no lease) |

A second client never gets authority: the window application is single-instance on the session bus (a second
launch activates the existing window), and the service refuses any second link while the lease is held (one
`service.notice {code: unauthorized}`, then the link closes).

## Restart recovery

Before the socket exists: (1) operations left `pending` are recorded `uncertain` with an `interrupted` outcome
(never re-executed); (2) integration intents are reconciled by recomputing the exact squash commit (same tree,
parent, message, identity and time) and comparing it with the actual target ref — present → Integrated
(`reconciled_done`), otherwise `reconciled_aborted`; (3) per Objective `lade_core::application::recover_after_restart`:
live executions → `cancelled` + `interrupted`, unconsumed approvals invalidated (`service_restart`), running
verifications interrupted, `dispatched` inputs → `uncertain`; nothing resumes; (4) best-effort sweep: the launcher's
`sweep_stale($STATE/run)` (pidfile-identified sandboxes, gateway sockets), any remaining `bwrap`/`prlimit` process of
this uid whose arguments reference the state directory, leftover verification copies and `lade/verify/*` branches,
and `$STATE/tmp`.

## Tests

`crates/lade-system/tests/service_{admission,operations,flow,actions,terminal}.rs` run the real socket, real SQLite and disposable Git
repositories: handshake/file modes, wrong capability and pre-auth frames, second client refused and reconnect,
invalid request keeps the link, admission rotates on restart; duplicate operations and `operation.get`; objective and
journal survive restart; restart interrupts a live execution and makes unacknowledged dispatch uncertain; corrupt
store refuses to start; draft CAS and whole-batch stale blocking; the full loop with the real worker (the real
bubblewrap launcher when it works, else the unsandboxed test launcher; `lade-fixture` model): message → working changes → draft → feedback → revision → freeze → sandboxed
verification (or override without bubblewrap) → approve → integrate into a clean checked-out `main` → cleanup; dirty
target refused and untouched; moved target → `stale_target`, approval invalidated, refresh, re-approve, integrate;
and the real sandboxed launcher when bubblewrap works.

## Known gaps

- The working-basis Change Set compares against the current target commit; if the target moved after the start
  commit, target-side changes appear as reverted until a candidate is frozen (candidates merge onto the target).
- The lease refusal is a `service.notice` event (the protocol has no request to reply to).
- `SIGTERM` is not handled gracefully; recovery on the next start is the contract. Graceful stops go through
  `service.stop` (Exit, logout with a running client).
- A terminal needs an existing worktree: an Objective that never ran its agent has none (no provisioning from
  `terminal.open`).
- Search content results carry line numbers, not byte offsets.
- Reading files larger than 64 MiB is refused (`unsupported_content`) rather than streamed.
