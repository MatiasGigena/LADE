# The `laded` service (WP-F1)

`laded` is the authoritative System Layer process: it owns product identities, the product store, Objective
lifecycles, Git mutations, worker supervision and client admission. It implements the domain rules in `lade-core`, the wire contract in [protocol-v1.md](protocol-v1.md), the adapters in
`lade-system::adapters` and the launcher contract in `lade-system::supervision` (see [execution-safety.md](execution-safety.md)).
Source: `crates/lade-system/src/{bin/laded.rs,service,storage,transport}`.

```sh
laded --version
laded serve [--state-dir DIR] [--runtime-dir DIR] [--max-workers N] [--max-terminals N]
            [--storage-warn-mib N] [--storage-reserve-mib N]
laded store info [--state-dir DIR]                 # schema this build understands, database schema, backups
laded store restore --schema N [--state-dir DIR]   # rollback restore (used by scripts/install --rollback)
laded diagnostics export OUT [--state-dir DIR]     # redacted diagnostics bundle, new 0600 file
```

The limit flags set the [aggregate accounting](#aggregate-limits-and-storage-pressure-imp-013) across
Objectives; the effective values are printed at startup (`laded: limits …`).

Defaults: state `$XDG_STATE_HOME/lade` (`~/.local/state/lade`), runtime `$XDG_RUNTIME_DIR/lade` (fallback
`$STATE/run`). Both must be absolute. Exit statuses: `0` version, `64` usage, `78` refused to start (unsafe directory,
unopenable/corrupt/newer store, pre-migration backup failed, another instance holds the state directory), `70`
internal; `store restore`: `75` while a service holds the state directory, `1` refused (no intact backup of that
schema, or the database is not newer); `diagnostics export`: `73` the output exists (never overwritten), `74` I/O. The process sets umask
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
| `$STATE/backups/` (0700) | pre-migration backups `lade.db.schema-<n>.<ms>.bak` (0600, at most 3), the append-only `migrations.journal`, and newer databases set aside by a rollback restore (`lade.db.newer-schema-<n>.<ms>`, never pruned) |
| `$STATE/logs/` (0700) | redacted diagnostics `laded.log`, `laded.log.1` … `.3` (0600, JSON lines, 4 × 1 MiB) |
| `$RUNTIME/control.cap`, `control.sock` | admission capability and control socket (0600, directory 0700) |
| `$RUNTIME/g/<12 hex>-<gen>.sock` | per-launch gateway sockets (0600, directory 0700; swept on start) |

Unix socket paths hold at most 107 bytes. Gateway sockets therefore live under the runtime directory with short
names, so any state directory length works; at startup `laded` refuses (exit 78, before creating anything) a runtime
directory whose `control.sock` or widest gateway socket path (generation up to 9,999,999) would not fit, naming the
path and asking for a shorter `--runtime-dir`.

Git-side state lives in the Workspace repository: Objective branches `lade/<short-id>` (suffix `-2`… on collision),
retention refs `refs/lade/retain/{capture-<id>,candidate-<id>,candidate-<id>-tree}` that pin every inspected capture
and candidate against garbage collection (never removed by Cleanup; only by the explicit, confirmed
`objective.delete_history`), and temporary `lade/verify/<id>` branches for verification copies.

### Git invocations

Every Git process goes through the hardened builder (`adapters/git/command.rs`): cleared environment, no
user/system configuration, fixed `GIT_CONFIG_*` overrides (hooks path, fsmonitor, pager, editor, askpass, credential
helper, external diff, signing, protocols, maintenance). Before **each** invocation the configuration that invocation
would see (repository, worktree and included files) is listed once (`git config --list -z --name-only`) and every
repository-defined filter driver (`clean`/`smudge`/`process` emptied, `required=false`), merge driver (`false`) and
configuration-defined hook (`hook.<name>.command` and `.event` emptied, `.enabled=false`) is overridden for that
call. The hook overrides are unconditional: Git versions without configuration hooks ignore the keys. This costs one
short extra `git config` process per call. Remaining race: a file *included* by the repository configuration that
changes between the listing and the call (the worker cannot write the common Git directory, but an include could
point elsewhere).

Captures (`capture_working_tree`) use a temporary index, force `core.fileMode`/`core.symlinks` on and
`core.ignoreCase` off (a candidate is an exact commit and the supported Linux filesystems represent modes and
symlinks; a repository setting meant for filesystems that cannot must not hide such a change), honour the
repository's `.gitignore`/`info/exclude` and the **user's own** global excludes file (resolved by the service from
`~/.gitconfig` / `$XDG_CONFIG_HOME/git/config` or the default `$XDG_CONFIG_HOME/git/ignore`), but never a
`core.excludesFile` from the repository configuration (it could hide new files from review). An untracked nested
repository without a checked-out commit (which Git cannot record) is excluded and reported (≤ 64 per capture) instead
of failing the capture; one with a commit is recorded as a gitlink. Branch names that are not UTF-8 are listed in a
lossless escaped form (`\xHH` per invalid byte; Git forbids `\` in ref names) and refused as integration targets
(`unsupported_content`).

## Storage

One connection, opened and owned by the service thread: `journal_mode=WAL`, `synchronous=FULL`, `foreign_keys=ON`,
`busy_timeout` 5 s. Opening runs `PRAGMA quick_check`; a failure, a non-WAL journal or a schema version newer than this
build refuses to start — the database is never reset or rewritten. Migrations are an append-only ordered list recorded
in `schema_migrations(version, applied_at)`, each applied in its own transaction. v3 adds
`objectives.history_deleted` (an older build refuses a v3 database, exit 78).

**Dependency:** `rusqlite =0.40.2` with `bundled` (SQLite compiled in, so behaviour does not depend on the host
library version). It is allowed for `lade-system` only; `scripts/check-architecture.py` still rejects any path from
`lade-desktop` to SQLite. `rustix =1.1.4` (`net`, `process`, `fs`, `pty`, `termios`) provides `SO_PEERCRED`, `getuid`, `umask`, `flock`,
`kill` and the terminal PTY (`posix_openpt`, `TIOCGPTPEER`, window size) without `unsafe`; `serde_json` (already reviewed for the protocol) encodes stored outcomes and references.

Schema v1 tables:

| Table | Holds |
| --- | --- |
| `settings` | the wire `Settings` JSON (default model, verification commands) |
| `workspaces` | id, canonical top-level path (bytes), name, default branch |
| `objectives` | every `ObjectiveRecord` field (intent, revisions, target, start commit, lifecycle, execution state/generation/session association/model, cleaned_up, timestamps) plus the real Git branch, worktree path, the integration-conflict fact and (v3) whether its Git history was deleted |
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
only, never prompt, source or secret bodies (see [Diagnostics](#diagnostics)).

### Migration safety (IMP-014)

Before migrating an **existing** database (a fresh one has nothing to lose), `Store::open` writes a consistent copy
with `VACUUM INTO` to `$STATE/backups/lade.db.schema-<from>.<unix ms>.bak` (created 0600 first; the directory is a
private 0700 directory of the service user, fsynced), records `backup <from> <to> <file> <sha256> <bundle>` in
`$STATE/backups/migrations.journal`, prunes all but the newest 3 backups (journaling `pruned`), and only then runs the
migrations; `applied <from> <to>` is journaled after they commit. If the backup cannot be written (disk full,
permissions) the service refuses to start (exit 78) **without migrating**. An interrupted migration rolls back its
own transaction; the backup stays.

`laded store restore --schema N` is the rollback half: under the service's state lock (refused with exit 75 while a
service runs) it picks the newest journaled backup of schema `N` whose digest still matches, checks it
(`quick_check`, schema `N`), folds the WAL of the current database (`wal_checkpoint(TRUNCATE)`), moves the current
database to `$STATE/backups/lade.db.newer-schema-<n>.<ms>` (kept, never pruned — it is the only copy of what happened
after the upgrade), stages the backup as a 0600 copy, renames it into place, fsyncs and journals `restored`.
`scripts/install --rollback` uses it (with the newer bundle's `laded`) when the database schema is newer than the
`schema=` recorded in the previous bundle's `VERSION`; see the [user guide](../user-guide.md#1-install). Tests:
`storage::tests` (forward migration backs up, restore, the old schema starts again, bounded backups, tampered/missing
backup and running service refused; also over the real migrations 1 → 3) and `tests/laded_cli.rs` (a schema-1 store
migrated 1 → 3 by the real `laded serve`, restored with `laded store restore --schema 1`, then opened by a
schema-1 store — `Store::open_as_schema`, a test stand-in for the previous release — and migrated forward again).

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
{code: "link_overflow"}` is sent and the link closes; the client resyncs from `snapshot`. A frame write that
makes no progress for 10 s (`WRITE_STALL_TIMEOUT`, a client that stopped reading) ends the link too, so a stalled
client never pins the writer thread or its socket. With one controlling link, the 8 MiB link bound is also the
service-wide outbound bound (≤ 32 MiB budget). `service_concurrency.rs::a_client_that_stops_reading_is_dropped_and_the_next_one_resyncs`
sends 8,000 queries without reading their replies: the link is dropped, the next client is admitted and resyncs
from `snapshot`, the test process grows by less than 64 MiB and the stalled link receives a bounded tail.

## Threading model

```text
accept thread ── per-link reader ─┐
tick thread (100 ms) ─────────────┤   (the tick also drains terminal output, see below)
supervisor forwarder (launcher) ──┼─► service queue ─► service thread (state + store) ─► per-link writer
job lanes: results ───────────────┘                         │
                                                            ├─► general lane (3 threads): Git capture/diff/
                                                            │   read/search, freeze, refresh, integration,
                                                            │   provisioning + launch, cleanup
                                                            ├─► verification lane (4 threads): checks
                                                            └─► reaper lane (4 threads): worker termination,
                                                                terminal kills
```

The service thread owns all in-memory state and the only store connection. Mutations follow
clone → domain transition → commit (short transaction) → swap, so a failed write leaves memory unchanged. Long work
runs on a job lane with owned inputs and posts its result back; on completion the service revalidates (lifecycle,
execution liveness, candidate status, generation) before committing. A long mutating command holds a per-Objective
`busy` marker: other mutating commands on that Objective are rejected `conflict` until it completes (queries
continue). Short Git reads (target resolution for details, `workspace.open`, branch lists) run inline.

The lanes keep concurrent Objectives from starving each other (IMP-013): verifications (up to 900 s each) never
occupy the threads that capture, freeze, launch or integrate, and revocation — terminating a worker, killing a
terminal after lease loss, a freeze or a lifecycle change — never waits behind other work. Jobs a request starts
are held until the request's operation is journaled `pending` and only then submitted, so a crash can never leave
effects of an operation the journal does not know (the integration intent and its pending operation are one
transaction).

## Requests

Every request name in protocol-v1 is implemented.

Queries: `snapshot` (≤ 64 workspaces with ≤ 64 branches each, ≤ 400 most recent objective summaries with the outcome
clipped to 512 chars, settings, current sequence); `objective.get` (detail with a freshly resolved target commit,
the latest ≤ 64 candidates/verifications and `available_actions`); `activity.page` (items before `seq`, newest last,
≤ 768 KiB of text per page, live streamed text included); `changes.list` (working: a fresh capture; candidate:
candidate tree vs its base target commit; since: tree vs tree; changed files whose `filter` attribute names a driver
carry `filtered`, and `notices` name nested repositories left out of the capture and filtered/LFS content);
`file.read` and `diff.get` (working uses the latest capture, or captures first; whole file bytes ≤ 64 MiB are
retained in the artifact store and their digest returned; UTF-8 pages are cut at character boundaries and kept under
the frame budget, binary content is reported as binary; a file above 64 MiB and up to the hard bound of 1 GiB is
streamed once into the artifact store — never held in memory, the last 8 large-blob digests are remembered so paging
does not re-stream — and served as `chunked` pages through `offset`/`limit`; larger files are `unsupported_content`;
feedback staleness on such files streams the blob digest, then compares the referenced range; diffs are bounded to
1,800 lines / 640 KiB with `truncated`); `search` (objectives by title/outcome; file paths via `ls_tree` and content
via `git grep` over the basis tree, or the workspace default branch without an objective, with a `scope_label`
naming the version; content results carry the line, byte column and the exact byte range of the first match in the
raw blob, computed from the blob for files ≤ 16 MiB within a 64 MiB budget per search); `drafts.list`,
`inputs.list`; `operation.get` (journal state: `unknown`, `pending`, `completed`, `uncertain`, with the recorded
outcome).

Commands (all journaled; a duplicate `operation_id` with the same canonical digest returns the recorded reply without
re-executing, a different digest is `duplicate_conflict`, a duplicate while pending is `conflict`/`query_operation`;
a reused `request_id` with different content is `duplicate_conflict`):

- `workspace.open`: `Repository::open` (top level, non-bare, supported Git); idempotent per canonical path.
- `objective.create`: target branch must exist; start = target tip or an explicit commit of the repository.
- `objective.update_intent`: core CAS on `intent_revision`.
- `execution.start` / `execution.resume`: after the core transition is checked, the launcher's `preflight(provider)`
  (sandbox probe, egress policy, credentials) runs; a failure rejects synchronously with `execution_unavailable`
  (detail names missing credential variables, never values; a failed sandbox probe reads "LADE can't run agents on
  this computer because the sandbox check failed." and its technical detail goes to the service diagnostics) and
  nothing is recorded. Otherwise the core transition
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
  `integrate_squash` (in a checked-out target first a dry run `read-tree -m -u -n` of the checkout step: untracked
  files where the candidate adds files refuse with `conflict` naming them, before any ref moves; then CAS ref update
  + `read-tree -m -u`; dirty/busy targets refuse with `conflict` and stay untouched; if the real step still fails —
  the worktree changed after the dry run — the ref is CAS-reverted); success → Integrated, approval consumed, journal
  `done`. **Same-target
  serialization (IMP-013):** one integration per (Workspace, target branch) runs at a time; a request for a target
  that is being integrated waits (its Objective is `busy`, its operation `pending`) and is then checked against the
  target the first one moved — normally `stale_target` with the approval invalidated, so a stale frozen review is
  never integrated and the developer refreshes and re-approves. Integrations into other targets or repositories do
  not wait. The Git CAS (`update-ref` with the expected old value) remains the final guard.
- `objective.complete`: requires an empty Change Set (captured on the pool); `objective.discard`: core discard.
- `objective.cleanup`: core cleanup rules; removes the worktree (without confirmation only when its content equals
  the branch commit) and CAS-deletes the branch; records, retention refs, artifacts and session files stay.
- `objective.delete_history`: only for a cleaned-up Objective whose history was not deleted yet, and only with
  `confirm_history_loss: true` (`invalid_request` otherwise; the client sends it only from a confirmation dialog with
  an explicit acknowledgement). On the pool it CAS-deletes every capture retention ref recorded for the Objective,
  `candidate-<id>` and `candidate-<id>-tree` of each candidate and the Objective branch if it still exists; then
  `history_deleted` is recorded with the outcome. Never automatic (not by Cleanup, uninstall or storage pressure);
  records, accepted inputs, evidence and artifacts stay. Git may later collect the unreferenced objects, after which
  that Objective's candidates and captures can no longer be inspected.
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
for Resume (it is accepted with delivery `blocked`, and `blocked_reason` says to resume). `tests/service_actions.rs`
checks, for eleven Objective states, that every enabled action's request is accepted and every disabled action's
request is rejected. `terminal.open` is projected from the same scope check its request uses
(`service_terminal.rs` checks that the reason equals the refusal). Reasons and refusal details are short
developer-facing sentences that name the next step (sentence case, ending with a period, no internal codes).

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
`verification_missing`, `unsent_drafts` and `limit_reached` (first, for an execution that failed
because an enforced limit was reached; its failure reason starts with `limit_reached:`).

## Resource limits (IMP-006)

Every value is a constant in `crates/lade-system/src/adapters/sandbox/limits.rs` (the §7 proposals; approving or
changing them is Matias's decision). Enforcement per sandbox is described in
[execution-safety.md](execution-safety.md#limits-7); the service adds two things:

- **Writable-storage monitor (§7, 2 GiB).** Every 5 s (`STORAGE_CHECK_INTERVAL`) one pool job measures, for each live
  execution, the allocated blocks of its worktree plus its session directory (symbolic links not followed, hard links
  counted per name). Above `ServiceConfig::storage_limit_bytes` (default `WRITABLE_STORAGE_BYTES` = 2 GiB) the
  service records the execution as `failed` with the reason `limit_reached: writable storage <used> exceeds the
  <limit> limit (worktree and session directory)`, terminates the worker, sends `service.notice {level: error, code:
  limit_reached}` and appends a system activity item; the summary carries the `limit_reached` badge. Nothing is
  restarted and the limit is never raised: the developer frees space (for example in a terminal, which stays open and
  whose writes count too) and resumes explicitly; a resume that is still over the limit is stopped again at the next
  check. This is a **monitor, not a quota**: writes between two checks succeed, and the disk itself is not reserved.
- **cgroup v2 placement.** At start `laded` (the binary, not `service::start`) calls
  `adapters::sandbox::cgroup::detect`. When the unit's cgroup is delegated (`Delegate=memory pids cpu` in
  `packaging/systemd/lade.service`; delegation is stated either by a `trusted.delegate`/`user.delegate` extended
  attribute, which only the system manager sets, or by the systemd user manager reporting `Delegate=yes` for the unit
  whose `ControlGroup=` is this cgroup; the cgroup is owned by the service user, its interface files writable, the
  three controllers available; see [execution safety](execution-safety.md#cgroup-v2-placement)), `laded` moves
  itself (and anything else in the cgroup) into `<unit cgroup>/service`, enables the controllers, proves with a probe
  cgroup that `pids.max` refuses a fork, removes sandbox cgroups a previous instance left, and passes the subtree
  to the launcher, the terminals and verification runs: each Objective gets `<unit cgroup>/sandboxes/<objective>`
  with `OBJECTIVE_CGROUP_LIMITS` (`memory.max` 4 GiB, `pids.max` 512, `cpu.max` unlimited), shared by its worker,
  terminals and verification runs in one leaf each (`worker-<generation>`, `terminal-<id>`, `verify-<id>`). A
  sandbox is moved into its leaf while its launch gate is still closed; when it ends the leaf is killed
  (`cgroup.kill`) and removed. Without delegation (cgroup v1/hybrid, not started by the unit, a non-delegated cgroup,
  missing controllers, a failed probe, `LADE_CGROUPS=off`) `laded` logs `cgroup limits unavailable (<reason>); sandboxes use the
  prlimit limits only` and continues; it never writes into a cgroup it was not delegated. A memory OOM kill inside a
  sandbox is logged (`sandbox memory limit reached`) and surfaces as the worker's unexpected exit.

## Contained terminals (IMP-012)

Source: `service/terminal.rs` (bookkeeping) over `supervision/terminal.rs` (PTY and sandbox; see
[execution-safety.md](execution-safety.md#contained-terminal-imp-012)). The client cannot start a process: it can
only ask the service, which allocates the PTY itself and starts the shell in the Objective's bubblewrap sandbox with
the worker's `prlimit` quotas.

- **Scope.** `terminal.open {objective_id, cols, rows}` (sizes 1..=500 × 1..=200) is accepted only for an Objective
  whose worktree exists (`invalid_lifecycle` with action `none`, "Start the agent once to create this Objective's worktree." otherwise; after Cleanup it says so) and whose lifecycle
  is `defined`/`active` (read-write) or `frozen`/`approved` (**read-only**: the worktree is bound read-only). Any
  other lifecycle is `invalid_lifecycle`. Without a working sandbox: `execution_unavailable` — there is no
  unsandboxed terminal. At most 4 terminals per Objective and `--max-terminals` (default 16) across all Objectives (`limit_reached`, naming both counts; before IMP-013 the total was wrongly counted per Objective). The sandbox starts on
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

## Diagnostics

`laded serve` points its own stderr at a pipe read by the `laded-diagnostics` thread (`lade_system::diagnostics`), so
every line written to stderr by the service, its libraries, a panic or the forwarded worker stderr goes through one
path: cut to 2,048 bytes, **redacted**, then written to the original stderr (the user journal under systemd) and as a
JSON object `{ts, pid, source, line}` to `$STATE/logs/laded.log`. Rotation is by size: when the next record would
pass 1 MiB the files shift (`laded.log` → `.1` … `.3`, the oldest is deleted), so the logs never exceed 4 MiB. The
log is opened lazily, only once the state directory exists as a private directory of the service user; a log
write failure never stops stderr forwarding. These numeric bounds are the implementation driver's proposal under the
#14 policy and still need Matias's approval.

Redaction (#14: credentials, prompts, source and tool-output bodies are excluded by default; redaction is tested,
not infallible): `NAME=value`, `"name": "value"` and `name: value` where the name contains `KEY`, `TOKEN`,
`SECRET`, `PASSW`, `AUTH`, `CREDENTIAL`, `COOKIE` or `CAPABILITY`; the value after `Bearer`/`Basic`; URL user-info;
well-known token prefixes (`sk-`, `ghp_`, `github_pat_`, `xox?-`, `AKIA`, `AIza`, `eyJ` …); and opaque strings of
32+ characters mixing upper case, lower case and digits become `<redacted>`; the home directory becomes `~`; control
characters become spaces. Ids, hex digests and error codes are kept. The service itself never writes prompts,
source, tool or terminal output to stderr.

`laded diagnostics export OUT` writes a new 0600 plain-text file (never overwrites; `73`): a header (bundle,
protocol, supported/database schema, kernel release, what is included and excluded), then each log file and the
migration journal, every line redacted again. It never includes the database, artifacts, worktrees, session files,
credentials or the environment. Review it before sharing it. The desktop client has no menu yet, so there is no
"Export diagnostics" action in the window; the command is the export path. The unit sets `LimitCORE=0`, so no core
dump of `laded` or a worker (whose memory can hold the released key) reaches `systemd-coredump`.

## Restart recovery

Before the socket exists: (1) operations left `pending` are recorded `uncertain` with an `interrupted` outcome
(never re-executed); (2) integration intents are reconciled by recomputing the exact squash commit (same tree,
parent, message, identity and time) and comparing it with the actual target ref — present → Integrated
(`reconciled_done`), otherwise `reconciled_aborted`; (3) per Objective `lade_core::application::recover_after_restart`:
live executions → `cancelled` + `interrupted`, unconsumed approvals invalidated (`service_restart`), running
verifications interrupted, `dispatched` inputs → `uncertain`; nothing resumes; (4) best-effort sweep: the launcher's
`sweep_stale($STATE/run)` (every `prlimit`/`bwrap` process carrying this run directory's sandbox marker, recorded or
not, then pidfile-identified sandboxes, gateway sockets), any remaining `bwrap`/`prlimit` process of this uid whose
arguments reference the state directory, leftover verification copies and `lade/verify/*` branches, and
`$STATE/tmp`. With delegated cgroups `laded` has already killed and removed leftover sandbox cgroups. IMP-013 additions: a reconciled-done integration whose crash fell between the ref move and
`read-tree` gets its checkout finished (`Repository::finish_integration_checkout`: `read-tree -m -u <expected>
<commit>` in the single idle checkout; it keeps an already-updated index and refuses to overwrite local edits —
then the first client sees a `service.notice {code: recovery}` naming the target); and a worktree that
provisioning created but did not record (crash before `launch_done`) is adopted by the next launch when it is a
registered worktree on the Objective's `lade/<id>` branch still at the start commit (no agent ran in it).

### Crash-window matrix (A09/A10)

Debug builds abort the process at a named point when `LADE_FAULT_POINT=<point>` is set (`lade_system::fault`;
release builds compile the hook to nothing). `tests/service_crash.rs` runs the real `laded` binary with real
sandboxed workers, aborts it at each point, restarts it and checks the recovered state and that the flow
continues. Every point below is **tested**:

| Point | State left by the crash | Recovery observed |
| --- | --- | --- |
| `input.accepted` | input + outcome committed, no reply, nothing dispatched | operation `completed`; input `queued`, delivered once after the explicit Resume |
| `dispatch.recorded` | `dispatched` committed, frame never written | input `uncertain` (badge), never resent, also not after Resume |
| `launch.provisioned` | worktree + branch created, not recorded | execution interrupted; Resume adopts the worktree (one `lade/*` branch) |
| `capture.retained` | retention ref, no capture row | orphan ref harmless; the next capture records normally |
| `freeze.branch_moved` | Objective branch at an unrecorded candidate commit | operation `uncertain`, Objective `active` without candidate; freezing again succeeds (CAS from the moved branch) |
| `freeze.recorded` | candidate + outcome committed, no reply | operation `completed` with the candidate; Objective `frozen` |
| `verify.copy` | verification copy and `lade/verify/*` branch, check not run | verification `interrupted`; copy and branch swept |
| `integrate.intent` | intent + pending operation journaled, target untouched | `reconciled_aborted`; operation `uncertain`; approval expired; target unchanged |
| `integrate.ref_moved` | target ref moved, checked-out files behind | `reconciled_done`; checkout finished at start (clean `git status`, files present) |
| `integrate.applied` | Git done, outcome not recorded | `reconciled_done`, Objective `integrated`; the lost reply stays `uncertain` |
| `cleanup.removed` | worktree and branch gone, record not cleaned | cleanup runs again and completes (idempotent); records and retention refs stay |

Not injected (covered by design or out of reach): a crash inside a SQLite transaction (atomic by SQLite), inside
pi's own session writes (pi does not fsync; see limitations), during a session fork/import rebinding, and the
first-message gap of pi.

`service_crash.rs::killing_the_service_ends_four_executions_and_their_terminals` kills `laded` (`SIGKILL`) while
four sandboxed workers run and each Objective has a terminal with a `setsid nohup` daemon: no sandbox process
and no daemon survives (bubblewrap `--die-with-parent`), and the restart shows all four executions interrupted,
none resumed.

## Aggregate limits and storage pressure (IMP-013)

Source: `service/limits.rs`. Accounting across Objectives lives in the service only (sandbox limits —
`prlimit`, cgroups — are the launcher's). Every limit is configurable and printed at startup with its option; the
refusal and `available_actions` state it in plain words (without the option); the defaults are **proposals
awaiting approval** (#14 policy):

| Limit | Flag | Default | At the limit |
| --- | --- | --- | --- |
| live agent executions (one sandboxed worker each) | `--max-workers` | 16 | `execution.start`/`resume` → `limit_reached` "N of N agents are already running, the most LADE runs at once. Stop one to start another."; nothing is recorded; the action shows the same reason |
| contained terminals in total | `--max-terminals` | 16 (4 per Objective, fixed) | `terminal.open` → `limit_reached` naming both counts |
| free space on the state filesystem: warning | `--storage-warn-mib` | 2048 | `service.notice {level: warning, code: storage_low}` (on the transition and to every newly attached client) |
| free space: reserve | `--storage-reserve-mib` | 512 | `service.notice {level: error, code: storage_critical}`; new writers are refused with `limit_reached` "The disk that holds LADE's data has … MiB free, below the … MiB LADE keeps in reserve. Free some space and try again." — `execution.start`/`resume`, writable `terminal.open`, `review.verify`; back above: `storage_ok` |

The free-space reserve complements, and does not replace, the per-execution writable-storage monitor (IMP-006:
worktree + session directory against `WRITABLE_STORAGE_BYTES`, the agent stopped with `limit_reached`): that one
bounds one Objective's growth, this one protects the filesystem shared by every Objective and the store. Four is a
measurement point, not a cap: the default admits 16 workers and the tests run five and six. Free space
(`statvfs` `f_bavail × f_frsize` of `$STATE`) is checked at start and every second. Under pressure, decisions,
drafts, accepted input (delivery `blocked` until Resume), captures, freezes, approvals, integrations and the
activity history keep being recorded inside the reserve; nothing is deleted to make room and running executions
are not stopped. Bounded spools: link queues (8 MiB), worker queues (8 MiB), terminal rings (1 MiB × terminals),
and activity items (192 KiB each) are unchanged; streamed activity text is still flushed
every second (a crash loses at most ~1 s of streamed text; accepted input and decisions are durable).

## Test-only fault injection

Both mechanisms live in `lade_system::fault` and exist only in **debug builds** (release builds compile them to
nothing; the unit never sets either variable): `LADE_FAULT_POINT` aborts the process at a crash point (see the
[crash-window matrix](#crash-window-matrix-a09a10)); `LADE_TEST_FAULTS_DIR=<absolute dir>` makes operations fail
while the service keeps running. With the latter, `laded serve` logs `test fault injection enabled` and reads that
directory at every `draft.save`: a file `draft.save` that is empty refuses the save with `durability_failure` before
anything is written; containing `delay <ms>` (≤ 10 s) it holds the save on the service thread and then saves
normally. It exists so that `scripts/e2e-resilience` can show the client's save-failure, retry and "Saved only after
the acknowledgement" behaviour against the real service; it is **not a product setting**.

## Test-only fixture provider

The offline scripted model `lade-fixture/scripted` (the worker's `src/worker/fixture.ts`, see
[worker.md](worker.md#lade-fixture-provider-test-only)) exists only for test harnesses. It is enabled only when
`laded serve` starts with **`LADE_TEST_FIXTURE_PROVIDER=1`** in its environment (read once at start, in debug and
release builds; the systemd unit never sets it). `laded` then logs `warning: offline test model lade-fixture enabled
(LADE_TEST_FIXTURE_PROVIDER=1); test harnesses only, not for real use`, sets `ServiceConfig::test_fixture_provider`
and `LauncherConfig::test_fixture_provider`, and passes the switch into the sandbox of the model catalogue and of each
worker launch, which register the provider only with it. Without the switch, at every layer:

- the worker and the catalogue do not register the provider, and the service drops it from `models.list` (also the
  fallback catalogue used when the SDK catalogue cannot run, which is then empty with its error);
- `execution.start`, `execution.resume` and `session.configure` naming it — for example from a stale stored
  default model or an Objective started by a test build — are refused with `execution_unavailable` and the detail
  `lade-fixture is an offline test provider; it is available only when laded runs with
  LADE_TEST_FIXTURE_PROVIDER=1 (test harnesses only)`; a launch reaching the sandboxed launcher (restart recovery)
  fails with the same reason.

With the switch the provider is an ordinary catalogue entry (no special badge or label in the client). The Rust
integration tests (`tests/service_support`), the worker tests, `tests/load.rs` and `scripts/e2e-gui`,
`e2e-terminal`, `e2e-resilience` and `e2e-perf` turn it on; `tests/service_compat.rs` and
`tests/supervision_launcher.rs` check that it is absent and refused without it.

## Tests

`crates/lade-system/tests/service_{admission,operations,flow,actions,terminal,storage,inspect}.rs` run the real socket, real SQLite and disposable Git
repositories: handshake/file modes, wrong capability and pre-auth frames, second client refused and reconnect,
invalid request keeps the link, admission rotates on restart; duplicate operations and `operation.get`; objective and
journal survive restart; restart interrupts a live execution and makes unacknowledged dispatch uncertain; corrupt
store refuses to start; draft CAS and whole-batch stale blocking; the full loop with the real worker (the real
bubblewrap launcher when it works, else the unsandboxed test launcher; `lade-fixture` model): message → working changes → draft → feedback → revision → freeze → sandboxed
verification (or override without bubblewrap) → approve → integrate into a clean checked-out `main` → cleanup; dirty
target refused and untouched; moved target → `stale_target`, approval invalidated, refresh, re-approve, integrate;
the real sandboxed launcher when bubblewrap works; the writable-storage monitor with a 256 KiB budget
(`service_storage.rs`: `failed` with `limit_reached`, badge, error notice, no automatic restart). `service_inspect.rs`: nested-repository and filtered-content
notices (working and candidate bases); exact content-search byte ranges against the retained bytes; chunked reading of
a > 64 MiB file (whole-file digest, exact continuation, character-boundary refusal, fresh and stale feedback on a
chunk); an untracked collision refused before the target ref moves (reflog unchanged), then integrated once moved
away; `objective.delete_history` refused before cleanup and without confirmation, then deleting every retention ref,
durable across a restart.

IMP-013: `tests/service_concurrency.rs` — four real (sandboxed) workers started and driven at once on two
repositories: each worktree holds only its own Objective's files, an absolute-path write into another
Objective's worktree fails inside the sandbox, a terminal of one Objective cannot see another's worktree or any
session directory, one `lade/*` branch per Objective in its own repository, candidate retention refs never in
the other repository; two integrations into the same `main` sent together → exactly one integrates, the other
is `stale_target` with its approval invalidated (and a third approval bound to the old target too), while an
integration into the second repository does not wait; refresh → re-approve → integrate leaves `main` with all
three changes and a clean checkout. Aggregate worker quota (five live, the sixth refused with the visible
reason, a cancel frees a slot), aggregate terminal total across Objectives, storage warning and reserve with the
refusals and the records that keep working, a client crash with four terminals (their `setsid` daemons die,
four agents keep running), the slow client above. `tests/service_crash.rs` — the crash-window matrix and the
service kill. `tests/load.rs` (ignored; `scripts/load`) — the canonical load, launch and soak procedures; reports
go to the ignored `evidence-local/imp-013/` directory by default.

## Known gaps

- Diagnostics bounds (4 × 1 MiB, 2,048-byte lines) and the redaction rules are proposals pending Matias's approval
  (#14); a secret in an unusual shape can pass redaction. Lines written to stderr while the log file is unavailable
  (state directory not yet created) reach only the journal.

- The working-basis Change Set compares against the current target commit; if the target moved after the start
  commit, target-side changes appear as reverted until a candidate is frozen (candidates merge onto the target).
- The lease refusal is a `service.notice` event (the protocol has no request to reply to).
- `SIGTERM` is not handled gracefully; recovery on the next start is the contract. Graceful stops go through
  `service.stop` (Exit, logout with a running client).
- A terminal needs an existing worktree: an Objective that never ran its agent has none (no provisioning from
  `terminal.open`).
- Files above 1 GiB cannot be inspected (`unsupported_content`); a chunk of a large text file that is not UTF-8
  is refused rather than shown with a gap.
- Search byte ranges are omitted for matched files above 16 MiB (line and column remain), and case-insensitive
  matching folds ASCII only (Git runs with `LC_ALL=C`).
- Filtered/LFS content is detected and labelled but not supported: raw working-tree bytes are compared and would
  be committed unfiltered; integrating into a checked-out target whose filtered files differ from their raw blobs
  is refused as dirty.
