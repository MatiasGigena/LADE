# pi worker (WP-D)

`packages/pi-worker/src/worker.ts` is the per-Objective Agent Session process of
[delivery plan §6](delivery-plan.md#6-worker-wp-d). It drives a real pi `AgentSession` through the
pinned `@earendil-works/pi-coding-agent@0.85.1` SDK (root exports only, no patches) and speaks the
§4.2 worker protocol as LF-delimited JSON on stdin/stdout.

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

The real `~/.pi`, `~/.agents` and the repository's `.pi/` are never read or written.

## Protocol mapping

Envelope `{"protocol":1,"bundle":"0.2.0","body":{…}}`. Inbound bodies are parsed with the IMP-001
bounded JSON parser (`src/json.ts`: 1 MiB frame, 256 KiB strings, duplicate-key rejection) and a
closed validator in `src/worker/wire.ts` (unknown fields, wrong kinds, non-UUIDv4 ids rejected).
`wire.ts` also holds the hand-mirrored DTOs and the `WorkerChannel` interface
(`send(body)` / `incoming()`); replacing it with WP-A's generated worker DTOs/validators is a
one-file change. The shared `Decoder("worker")` in `src/framing.ts` validates against the IMP-001
client schema, so `wire.ts` carries an equivalent `LineDecoder` (split on byte LF only, bound the
line before allocating, poison after the first error) until WP-A generalises it.

| Inbound | Worker action |
| --- | --- |
| `worker.start` | Create (`resume:false`) or reopen (`resume:true`, most recent session in `<session_dir>/sessions`) the pi session, then `worker.ready {pi_session_id, session_file}`. |
| `input.deliver` | `input.received {input_id}` first, then deliver by mode (below). |
| `session.abort` | `clearQueue()` (reported as a `system` item "Queue cleared" with counts only), then `abort()`; waits until the session is idle. |
| `worker.shutdown` | Abort if streaming, `dispose()`, flush stdout, exit 0. |

Delivery modes:

| Mode | Session streaming | Session idle |
| --- | --- | --- |
| `prompt` | `followUp(text)` — pi rejects an unqueued prompt while streaming | `prompt(text)` |
| `steer` | `steer(text)` — delivered after the current tool calls, before the next model call | `prompt(text)` |
| `follow_up` | `followUp(text)` — delivered when the agent would otherwise stop | `prompt(text)` |

`prompt` uses `expandPromptTemplates: false` and `source: "rpc"`; the worker awaits
`preflightResult` (acceptance), not the end of the run, before reading the next frame. Steering and
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
| `compaction_start`, `auto_retry_start` | `system` items without error text |

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

## SDK APIs used (0.85.1 root exports)

- `ModelRuntime.create({authPath, modelsPath: null, modelsStorePath, allowModelNetwork: false})`,
  `modelRuntime.registerProvider(id, config)`, `modelRuntime.getModel(provider, id)`.
- `SettingsManager.inMemory({...})` (no global or project `settings.json`; `defaultProjectTrust:
  "never"`, telemetry/analytics off, one-at-a-time queues).
- `new DefaultResourceLoader({cwd, agentDir, settingsManager, noExtensions, noSkills,
  noPromptTemplates, noThemes, noContextFiles, systemPromptOverride: () => undefined,
  appendSystemPromptOverride: () => [system_note]})` + `reload()`.
- `SessionManager.create(cwd, sessionsDir)` / `SessionManager.continueRecent(cwd, sessionsDir)`.
- `createAgentSession({cwd, agentDir, modelRuntime, model, tools: ["read","bash","edit","write"],
  resourceLoader, sessionManager, settingsManager})`.
- `AgentSession`: `subscribe`, `prompt(text, {expandPromptTemplates, source, preflightResult})`,
  `steer`, `followUp`, `clearQueue`, `abort`, `isStreaming`, `sessionId`, `sessionFile`, `dispose`.
- Types: `AgentSession`, `AgentSessionEvent`, `AgentToolResult`, `ModelRuntime`.
- From `@earendil-works/pi-ai@0.85.1`: `createAssistantMessageEventStream` and message/stream types.
- From `undici@8.9.0`: `Agent`, `buildConnector`, `setGlobalDispatcher`, `install`.

## `lade-fixture` provider

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
  truncated; stdin EOF → exit 0; unknown model → exit 1.
- `test/worker-wire.test.ts` — closed inbound validation, LF fragmentation/poisoning, outbound
  bounds, UTF-8/frame-size truncation, drain backpressure and delta coalescing.
- `test/worker-gateway.test.ts` — loopback-only CONNECT gateway on a Unix socket: tunnelled
  `fetch`, refused tunnel, invalid authority, and the no-gateway failure.

## Limitations (v1)

- **Resource discovery disabled**: no extensions, skills, prompt templates, themes, context files
  (`AGENTS.md`/`CLAUDE.md`), `SYSTEM.md`/`APPEND_SYSTEM.md`, packages or `settings.json` from the
  user profile or the repository. The only system-prompt addition is `system_note`.
- **Extension UI unsupported**: no extensions are loaded and no UI context is bound, so dialogs
  cannot occur; a future extension requiring UI must be rejected, never auto-answered.
- **Durability**: inherited from pi — first user message not flushed before an assistant entry; no
  fsync. `resume:true` with no prior session file starts a new session; the service detects this
  by a changed `pi_session_id`.
- **pi session ids are UUIDv7-shaped** strings chosen by pi, not UUIDv4: WP-A's schema must type
  `pi_session_id` as a bounded string.
- **Network coverage**: only undici and global `fetch` traffic is routed. Provider SDKs that use
  `node:http(s)` directly (e.g. the AWS Bedrock SDK) or WebSocket transports bypass the dispatcher
  and therefore have no network in the sandbox. HTTPS through the gateway is exercised only with a
  plain-HTTP origin in tests (TLS uses undici's unmodified connector).
- **Tool output isolation**: pi's `bash` tool spawns children with piped stdout, but nothing in the
  worker stops a hostile process from writing to the worker's inherited fd 1 via `/proc`; the
  service must treat every frame as untrusted (it does: closed schema validation).
- **Duplicate pi-ai**: the shrinkwrap nests pi-ai inside the coding-agent package, so the worker
  depends on a second registry copy of `@earendil-works/pi-ai@0.85.1` (≈ +130 MB on disk; its
  transitive versions are pinned by our lockfile, not by the upstream shrinkwrap). Only its
  `typebox`-only root entry and the event-stream class are loaded; the nested copy remains the one
  pi uses. TypeScript deduplicates the two by package name and version, so the types are
  identical. IMP-008's fork build can drop the duplicate.
- **Install scripts**: installed with `--ignore-scripts`; the skipped scripts (esbuild binary check,
  protobufjs, @google/genai) are not needed by the worker. `@silvia-odwyer/photon-node` is
  WebAssembly and needs none; the optional clipboard native module is unused.
- **Images** are not part of the §4.2 input body; tool images appear as `[image]` in activity text.
