# pi worker (WP-D)

`packages/pi-worker/src/worker.ts` is the per-Objective Agent Session process. It drives a real pi `AgentSession`
through the pinned `@earendil-works/pi-coding-agent@1.5.0` SDK (root exports only, no patches) and speaks the
[worker protocol](protocol-v1.md#service--worker-worker-schemajson-jsonl) as LF-delimited JSON on stdin/stdout.

## Launch

```sh
node packages/pi-worker/src/worker.ts   # Node 22.23.1, type stripping; no flags needed
```

`laded` starts it inside the bwrap sandbox with inherited pipes. Exit status: `0` after
`worker.shutdown` or stdin EOF, `1` when `worker.start` fails (for example `startup_failed`), `2`
on a protocol violation (malformed/oversized/unknown frame, frame before `worker.start`, second
`worker.start`), `70` on an internal failure.

Environment read by the worker:

| Variable | Use |
| --- | --- |
| `LADE_GATEWAY_SOCKET` | Unix socket of the service gateway. When set, all undici/`fetch` connections are tunnelled through it with HTTP CONNECT. When unset, every connection fails with `network disabled: no LADE gateway socket`. |
| provider credential variables (e.g. `ANTHROPIC_API_KEY`) | Read by pi's provider auth for the selected model only; the sandbox passes only the selected provider's variables. |
| `PATH`, `HOME`, `LANG`, `TERM` | Ordinary process environment for the built-in `bash` tool. `HOME` is the session dir in the sandbox; nothing is written under it by the worker itself. |

The worker sets for itself (inherited by tool subprocesses): `PI_CODING_AGENT_DIR=<session_dir>/agent`,
`PI_CODING_AGENT_SESSION_DIR=<session_dir>/sessions`, `PI_OFFLINE=1`, `PI_SKIP_VERSION_CHECK=1`,
`PI_TELEMETRY=0`.

Files written (all under `session_dir` from `worker.start`, plus whatever the agent's tools write
in `cwd`):

- `<session_dir>/agent/` (0700): pi agent dir; `auth.json` (empty `{}` credential store created
  by `ModelRuntime`), `models-store.json` only if a catalog is ever cached.
- `<session_dir>/sessions/` (0700): pi version-3 JSONL session file
  `<timestamp>_<session-id>.jsonl`. pi delays the first write until an assistant entry exists,
  so `session_file` in `worker.ready` may not exist yet (research §5 durability gap).
- `<session_dir>/exports/`: JSONL written by a `session.command` `export`.

Read only: `<session_dir>/profile/` (the service's snapshot of the LADE profile, IMP-011) and
`<session_dir>/imports/` (service copies of sessions to import). The real `~/.pi`, `~/.agents` and the
repository's `.pi/` are never read or written. The model catalogue for the service is produced by
`node src/worker/catalog.ts <out.json> [<models.json>]`, run by `laded` in the same network-less sandbox.

## Protocol mapping

Envelope `{"protocol":1,"bundle":"0.2.0","body":{…}}`. Body types are the generated `src/dto.ts`
(`ServiceToWorker`, `WorkerToService`, `Model`, …). Inbound frames are parsed with the IMP-001
bounded JSON parser (`src/json.ts`: 1 MiB frame, 256 KiB strings, duplicate-key rejection), validated
against the package-local generated schema by `src/worker/contract.ts` (`decodeWorkerFrame`,
`validateWorker` over `src/worker/worker-schema.json`: unknown fields, wrong kinds and non-UUIDv4 ids are
rejected) and checked for direction (`isServiceToWorker`). `src/worker/wire.ts` holds the one worker wire
codec: `parseInbound`, `encodeOutbound` (every outbound frame is re-validated against the same schema)
and `LineDecoder` (split on byte LF only, bound the line before allocating, poison after the first
error), plus per-kind aliases of the generated bodies. The test-side reference framing
(`src/framing.ts`) reuses that `LineDecoder` for worker framing.

| Inbound | Worker action |
| --- | --- |
| `worker.start` | Create (`resume:false`) or reopen (`resume:true`: the given `session_file`, otherwise the most recent session in `<session_dir>/sessions`) the pi session with the optional `thinking` level and `resources` snapshot, then `worker.ready {pi_session_id, session_file}`, `session.changed {reason:"start", generation:"1"}` and `resources.loaded`. |
| `input.deliver` | `input.received {input_id}` first, then deliver by mode (below). |
| `session.abort` | `clearQueue()` (reported as a `system` item "Queue cleared" with counts only), then `abort()`; waits until the session is idle. |
| `session.command {command_id, command}` | One session operation (`tree`, `navigate`, `fork`, `import`, `export`, `compact`, `reload`, `configure`), answered by `session.result {command_id, ok, message?, entries?, leaf_id?, truncated?}`. Not awaited by the read loop (an operation can wait for a dialog); a second command while one runs fails at once. Operations that change the session need an idle session. |
| `ui.respond {ui_id, generation, answer}` | Answers an open extension dialog of the current generation; anything else is refused and reported. |
| `worker.shutdown` | Abort if streaming, `dispose()`, flush stdout, exit 0. |

Delivery modes:

| Mode | Session streaming | Session idle |
| --- | --- | --- |
| `prompt` | `followUp(text)` — pi rejects an unqueued prompt while streaming | `prompt(text)` |
| `steer` | `steer(text)` — delivered after the current tool calls, before the next model call | `prompt(text)` |
| `follow_up` | `followUp(text)` — delivered when the agent would otherwise stop | `prompt(text)` |

`prompt` uses `expandPromptTemplates: true` and `source: "rpc"`: prompt templates and skill
commands are expanded exactly once, by pi, on this single path (the service and client never expand).
The worker awaits `preflightResult` (acceptance), not the end of the run, before reading the next
frame; an extension command (`/name` registered by an approved extension) is started without
awaiting it, because it may wait for a dialog that only a later frame answers. Steering and
follow-up modes are pi's `one-at-a-time`. A rejected input is `worker.error {code:"delivery_failed"}`;
a failure after acceptance is `session_failed`.

SDK events → outbound bodies (`src/worker/activity.ts`):

| SDK event | Outbound |
| --- | --- |
| `agent_start` / `agent_settled` | `turn.state running` / `idle` (only on change; `agent_settled` is pi's session-level completion, so retries and queued continuations stay `running`) |
| `message_update` `text_start`/`thinking_start` | `activity.item` (empty text) with a new `item_id` per content index, role `assistant` / `thinking` |
| `message_update` `text_delta`/`thinking_delta` | `activity.delta {item_id, append_text}` by content index (surrogate pairs never split; deltas stop at the item bound) |
| `message_end` (assistant) | `activity.item` with the same `item_id` and the authoritative text per block (replaces the streamed text); `stopReason:"error"` → `system` item + `worker.error {code:"provider_failed"}`; `"aborted"` → `system` item "Turn aborted" |
| `message_end` (user) | `activity.item` role `user` — shows when queued steering/follow-up actually entered the conversation |
| `tool_execution_start` | `activity.item` role `tool`, `tool_name`, title `<tool> <path or command>`, text `arguments:` + JSON |
| `tool_execution_end` | same `item_id`: `arguments:` + `result:` text (images as `[image]`), `is_error` |
| `compaction_start` / `compaction_end` | `system` items ("Compacting context", then "Context compacted", "Compaction aborted" or "Compaction failed" with the error flag) |
| `auto_retry_start` / `auto_retry_end` | `system` items ("Retrying model request", then "Retry succeeded" or "Retry gave up" with the error flag) |

An `activity.item` with a known `item_id` is an upsert of that item. Every item text is bounded to
192 KiB of UTF-8 (cut at a character boundary, lone surrogates normalised) and further if JSON
escaping would push the frame near 1 MiB; then `truncated:true` and the full byte length is in the
title (`… (truncated; N bytes)`). `worker.error.message` is one line, ≤ 512 chars, long
token-like runs masked; start/internal failures use fixed messages and stderr carries only error
class names.

Stdout carries only protocol frames: `src/worker/stdio.ts` is the first import, rebinds `console`
to stderr and redirects `process.stdout.write` to stderr before the SDK loads; the channel keeps the
only reference to the real stdout writer. Outbound frames are written in order; a full pipe is
awaited with `drain`. Consecutive deltas for the same item are coalesced, and above 4 MiB of queued
output further deltas are dropped (the final item still carries the complete bounded text).
Inbound frames are read one at a time (async iteration of stdin), so stdin is not buffered
unboundedly either.

## Sessions, resources and extension UI (IMP-011)

`src/worker/session.ts` (`SessionHost`) owns pi's `AgentSessionRuntime`
(`createAgentSessionRuntime` with a factory over
`createAgentSessionFromServices`). Fork and import go through the runtime (`fork`, `importFromJsonl`),
which replaces the `AgentSession`; `setRebindSession` re-subscribes and re-binds extensions, and every
replacement or reload increments the **generation** (starting at 1) reported in `session.changed`
`{generation, reason, model, thinking, pi_session_id, session_file, leaf_id?}`. Navigation uses
`navigateTree(entry, {summarize:false})`, which moves pi's leaf only and never touches files. Imports
are accepted only from `<session_dir>/imports/`, exports written only under `<session_dir>/exports/`,
reload snapshots only inside `session_dir`. `configure` calls `setModel` and `setThinkingLevel` (pi
clamps the level to the model).

`src/worker/resources.ts` (`LadeResourceLoader`) builds a `DefaultResourceLoader` per reload with all
discovery disabled and only explicit paths of the service's profile snapshot: `skills/`, `prompts/`,
the approved extensions that the worker **re-hashes** (`extensionHash`; a mismatch is reported as
`refused` and the extension is not loaded), and `agentsFilesOverride` with the LADE profile's
`AGENTS.md` followed by the worktree root's context file. `resources.loaded {generation, resources}`
lists every context file, skill, template and extension with source and status (`loaded`, `refused`,
`failed` with the diagnostic). Precedence and trust rules: [pi-compatibility.md](pi-compatibility.md).

`src/worker/ui.ts` (`UiBridge`) is the `ExtensionUIContext` bound with `bindExtensions`. `select`,
`confirm`, `input` and `editor` send `ui.request {ui_id, generation, prompt}` and wait for the
matching `ui.respond`; the extension's timeout or abort sends `ui.closed {timeout|aborted}`, and a new
generation closes open dialogs as `replaced`. `notify` becomes a system item. Display-only TUI
surfaces are no-ops; `custom()` and editor control throw `UnsupportedUiError`, so nothing is answered
automatically.

## SDK APIs used (1.5.0 root exports)

- `ModelRuntime.create({authPath, modelsPath: null, modelsStorePath, allowModelNetwork: false})`,
  `modelRuntime.registerProvider(id, config)`, `modelRuntime.getModel(provider, id)`.
- `SettingsManager.inMemory({...})` (no global or project `settings.json`; `defaultProjectTrust:
  "never"`, telemetry/analytics off, one-at-a-time queues).
- `new DefaultResourceLoader({cwd, agentDir, settingsManager, noExtensions, noSkills,
  noPromptTemplates, noThemes, noContextFiles, additionalExtensionPaths, extensionFactories,
  additionalSkillPaths, additionalPromptTemplatePaths, agentsFilesOverride, systemPromptOverride: () => undefined,
  appendSystemPromptOverride: () => [system_note]})` + `reload()`.
- `createAgentSessionRuntime`, `createAgentSessionFromServices`,
  `AgentSessionRuntime` (`fork`, `importFromJsonl`, `setRebindSession`, `setBeforeSessionInvalidate`,
  `dispose`), `SessionManager.open`, `Theme` (a plain theme for the UI context).
- `AgentSession`: `bindExtensions`, `navigateTree`, `compact`, `setModel`, `setThinkingLevel`,
  `exportToJsonl`, `reload`, `extensionRunner.getCommand`, `sessionManager` (`getEntries`, `getLeafId`,
  `getLabel`).
- `ModelRuntime.getProviders` / `getModels` and pi-ai `getSupportedThinkingLevels` (catalogue).
- `SessionManager.create(cwd, sessionsDir)` / `SessionManager.continueRecent(cwd, sessionsDir)`.
- `createAgentSession({cwd, agentDir, modelRuntime, model, tools: ["read","bash","edit","write"],
  resourceLoader, sessionManager, settingsManager})`.
- `AgentSession`: `subscribe`, `prompt(text, {expandPromptTemplates, source, preflightResult})`,
  `steer`, `followUp`, `clearQueue`, `abort`, `isStreaming`, `sessionId`, `sessionFile`, `dispose`.
  `preflightResult` reports only how an accepted prompt was dispatched; a delivery cancelled before
  the run starts (a newer input epoch) is dropped by the worker's own `input` handler (`InputGate` in
  `src/worker/session.ts`, passed as the only `extensionFactories` entry), which answers `handled`
  for that `rpc` input.
- Types: `AgentSession`, `AgentSessionEvent`, `AgentToolResult`, `ModelRuntime`.
- From the SDK's own `@earendil-works/pi-ai@1.5.0` (the single installed copy, re-exported by
  `src/worker/pi-ai.ts`; see [Dependencies](#dependencies)): `createAssistantMessageEventStream`,
  `getCurrentSystemPrompt` (provider streams receive a normalized `TranscriptContext` whose system
  prompt is in `messages`), `getSupportedThinkingLevels`, message/stream types, and
  `normalizeContext` (dependency test only).
- From `undici@8.12.3`: `Agent`, `buildConnector`, `setGlobalDispatcher`, `install`.

## `lade-fixture` provider (test-only)

The offline fixture provider exists only for test harnesses. The worker (`src/worker/session.ts`) and the
model catalogue (`src/worker/catalog.ts`) register it only when `LADE_TEST_FIXTURE_PROVIDER=1` is set in
their environment; `laded` sets it in the sandbox only when it was itself started with that switch (see
[service.md](service.md#test-only-fixture-provider)). Without it the provider is not in the catalogue and a
`worker.start` naming it fails with `startup_failed` ("the requested model is not registered"). The worker
tests (`test/worker-support.ts`) switch it on.

`src/worker/fixture.ts` registers provider `lade-fixture` with model `scripted`
(`lade-fixture/scripted`, display name "LADE fixture: scripted (offline test model)") through
`ModelRuntime.registerProvider` with a custom `api` and `streamSimple` — the documented
custom-provider mechanism. Its apiKey is a fixed non-secret placeholder (pi requires an auth method).
The stream goes through pi's real agent loop, and its tool calls execute pi's real `write` tool.
Behaviour, decided only from the visible conversation (last user message):

- default: streams `lade-fixture scripted reply (offline fixture model, not a real model). The
  context holds N user message(s).` in four deltas (N proves resumed history is in context);
- `lade-fixture: write <relative-path> <content…>` lines (one per file; `\n`, `\t`, `\\` escapes):
  a short text, then one real `write` tool call per line (`stopReason: "toolUse"`); after the tool
  results it replies `lade-fixture: K of M tool call(s) succeeded. Done.`;
- `lade-fixture: fail`: emits a provider error `lade-fixture: scripted provider failure`;
- `lade-fixture: delay <ms>` (≤ 2000): waits before each delta/tool call, honouring the abort
  signal — used to test steering and abort during a running turn.

## Gateway

With `LADE_GATEWAY_SOCKET`, `installGateway` sets an undici `Agent` whose custom connector opens
the Unix socket, sends `CONNECT host:port HTTP/1.1`, requires a 2xx status (16 KiB header bound,
30 s timeout, strict `host:port` authority), and then hands the tunnel to undici's own TLS
connector (`buildConnector(...)` with `httpSocket`) for `https:` origins, so TLS and certificate
verification are end-to-end with the origin; `http:` origins use the tunnel directly. `install()`
makes global `fetch` use the same undici. The gateway sees only the authority.

## Tests

`npm --prefix packages/pi-worker test` runs, in addition to the IMP-001 suites:

- `test/worker.test.ts` — spawns `node src/worker.ts` with disposable HOME/session dir/worktree
  over real pipes (every stdout line must be a valid envelope): start → ready with isolated
  profile; write directive → streamed items, real file in cwd, idle; steer + follow-up ordering;
  prompt while streaming → follow-up; abort clears the queue and keeps the worker usable; resume
  keeps `pi_session_id`/`session_file` and history; invalid frame → exit 2 without echo; frame
  before start → exit 2; fixture failure → `provider_failed` and recovery; > 192 KiB tool item
  truncated; stdin EOF → exit 0; unknown model → exit 1; without `LADE_TEST_FIXTURE_PROVIDER=1` the
  fixture model is not registered and `worker.start` fails with `startup_failed`.
- `test/worker-wire.test.ts` — closed inbound validation, LF fragmentation/poisoning, outbound
  bounds, UTF-8/frame-size truncation, drain backpressure and delta coalescing.
- `test/worker-gateway.test.ts` — loopback-only CONNECT gateway on a Unix socket: tunnelled
  `fetch`, refused tunnel, invalid authority, and the no-gateway failure.
- `test/worker-deps.test.ts` — the dependency seam: exactly one pi-ai is locked and installed (no
  direct dependency, no nested copy), every locked package that depends on pi-ai (pi-coding-agent,
  pi-agent-core, …) resolves the hoisted copy that `src/worker/pi-ai.ts` re-exports, the fixture
  provider registered on a real `ModelRuntime` streams through the SDK's own
  `AssistantMessageEventStream` class, and the installed SDK is the pinned distribution: its
  `package.json` hash equals `vendor/pi-coding-agent/manifest.json`, it ships no shrinkwrap, the patch
  inventory is empty, direct dependencies match `npm_distribution`, each exact dependency pin of the
  SDK is locked and installed at that version where the SDK resolves it, every production package is
  in the SDK's dependency closure, and that closure is exactly upstream's release install lock
  (`vendor/pi-coding-agent/upstream/install-lock/package-lock.json`: names, versions, resolved URLs
  and integrity).
- `test/worker-compat.test.ts` — IMP-011 corpus with a disposable LADE profile: shared hash
  constants, resource precedence and untrusted repository `.pi`, single template expansion, refused
  changed extension, dialogs with generations, timeout close, explicit failure of `custom()`, reload
  generations and late answers, tree/navigate (files unchanged)/fork/clone, export/import, compaction,
  configure and retry activity, concurrent command refusal, and the pinned manifest against the SDK
  catalogue.

## Dependencies

Direct production dependencies: `@earendil-works/pi-coding-agent@1.5.0` and `undici@8.12.3` (gateway
dispatcher; the same version the SDK pins). From 1.0 pi publishes no `npm-shrinkwrap.json`: the SDK
tarball pins its direct dependencies exactly, and upstream pins the transitive tree in
`packages/coding-agent/install-lock/` (the lockfile root its installer and updater use), retained
under `vendor/pi-coding-agent/upstream/install-lock/`. npm therefore hoists and dedupes the SDK's
tree normally; `packages/pi-worker/package-lock.json` resolves it to exactly that install lock,
including its `protobufjs` 7.6.6 override, without an `overrides` entry of our own. (0.85.1 shipped a
shrinkwrap, which npm installs nested and never dedupes, and which made root `overrides` ineffective
for security fixes inside it.)

The worker does not depend on pi-ai directly: `src/worker/pi-ai.ts` is the only module that names it,
re-exporting from the hoisted copy's `.` export target (`node_modules/@earendil-works/pi-ai/dist/index.js`)
by relative path, so the worker loads exactly the pi-ai instance the SDK loads and a direct dependency
cannot add a second copy with transitive versions outside the upstream lock.
`scripts/check-architecture.py` allows that relative import only from `src/worker/pi-ai.ts` and
rejects a bare `@earendil-works/pi-ai` import anywhere in the worker. The path depends on npm hoisting
pi-ai to the top level, which the lockfile pins and `test/worker-deps.test.ts` checks; an SDK upgrade
must re-run that test.

## Limitations (v1)

- **Resources only from the LADE profile snapshot**: themes, `SYSTEM.md`/`APPEND_SYSTEM.md`,
  packages, `settings.json` and the repository's `.pi/` are never loaded; see
  [pi-compatibility.md](pi-compatibility.md) for the full inventory.
- **Terminal-only extension UI** fails explicitly (`custom()`, editor control) or is ignored
  (status, widgets); approved extensions run with the worker's authority in the sandbox.
- **Durability**: inherited from pi — first user message not flushed before an assistant entry; no
  fsync. `resume:true` with no prior session file starts a new session; the service detects this
  by a changed `pi_session_id`.
- **pi session ids are UUIDv7-shaped** strings chosen by pi, not UUIDv4: WP-A's schema must type
  `pi_session_id` as a bounded string.
- **Network coverage**: undici/global `fetch` traffic and `node:http(s)` clients that use the global
  agents are routed through the gateway (`src/worker/gateway.ts`). SDKs that bring their own agent
  (or `agent: false`), raw sockets and other runtimes have no network in the sandbox. TLS through the
  real gateway is tested against a local origin with a test-generated certificate, for both `fetch`
  and `node:https` (`supervision_gateway.rs::worker_https_reaches_a_local_tls_origin_through_the_gateway`).
- **Tool output isolation**: pi's `bash` tool spawns children with piped stdout, but nothing in the
  worker stops a hostile process from writing to the worker's inherited fd 1 via `/proc`; the
  service must treat every frame as untrusted (it does: closed schema validation).
- **Install scripts**: installed with `--ignore-scripts`; the reasoning per package is in
  [build.md](build.md#worker-dependencies-and-install-scripts). None of the skipped scripts is needed.
- **Images** are not part of the §4.2 input body; tool images appear as `[image]` in activity text.
