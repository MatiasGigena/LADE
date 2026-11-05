# pi compatibility inventory (IMP-011)

What of the pinned pi workflow (`@earendil-works/pi-coding-agent@1.5.0`) is usable through LADE, mapped from the
pi compatibility decision and the
IMP-011 scope ([spec §5](spec.md#imp-011)). Every promise is in exactly one class:

- **Supported** — works through LADE and is covered by the named test (evidence level in brackets, as defined in
  [acceptance-status.md](acceptance-status.md#evidence-levels)).
- **Restricted** — deliberately refused or reduced by a stated LADE trust rule; the refusal is explicit to the
  developer (a reason in the UI or an error), never a silent omission or an automatic answer.
- **Unresolved** — not available yet and not covered by a decided trust rule; listed with what is missing.

The machine-readable part (provider classification, resource and UI classes) is the pinned manifest
[`pi-compatibility.json`](pi-compatibility.json). Two tests keep it honest: `worker-compat.test.ts` ("the pinned
compatibility manifest matches the SDK catalogue": SDK version and every catalogue provider id) and
`lade-system` `providers::compatibility_manifest_matches_the_reviewed_tables` (each provider's status and variables
equal what this build's egress and credential tables decide).

Test locations: **W** = `packages/pi-worker/test/worker-compat.test.ts`, **S** =
`crates/lade-system/tests/service_pi_integration.rs`, **D** = `crates/lade-desktop` unit tests
(`presentation/state/agent_session.rs`, `presentation/controller/agent_session.rs`,
`transport/requests/tests.rs`).

## Profile separation

LADE never reads or writes the developer's own pi profile (`~/.pi`, `~/.agents`) at run time. The pi resources a
session sees come from the **LADE profile** `~/.local/state/lade/profile/` (`AGENTS.md`, `skills/`, `prompts/`,
`extensions/`, `models.json`). For every start, resume and reload the service materialises a snapshot of it into
`<session_dir>/profile/`, containing only extensions whose current content hash is approved. The worker's own pi agent
dir (`<session_dir>/agent`) is not used for discovery.

`profile.import` is the only contact with a pi profile: a **copy-only** import from a directory chosen by the
developer (`~/.pi/agent` layout). The source is opened read-only; existing LADE profile entries are never
overwritten; symbolic links are skipped; bounds 2048 files / 32 MiB; `models.json` is copied only if it passes the
custom-provider rules below. Imported extensions still need approval. [S `profile_import_copies_only_and_never_touches_the_source`, integrated]

## Inventory

### Agent Session

| Promise | Class | How / reason | Evidence |
| --- | --- | --- | --- |
| Resume the Objective's session | Supported | `worker.start` carries the recorded `session_file` of the current association, so resume reopens exactly that pi session (also after a fork or import); a `session_file` outside `<session_dir>/sessions/` fails the start with `startup_failed` | `worker.test.ts` resume; S `tree_fork_export_import_…` [integrated] |
| Session tree inspection | Supported | `session.tree` → the last 1024 entries (`message`, `compaction`, `branch_summary`, `model_change`, `thinking_change`, `label`, `custom`, …) with leaf and `truncated` | W "tree navigation …"; S [integrated] |
| Tree navigation ("go here") | Supported | `session.navigate` moves pi's leaf only (`navigateTree`, no summary). **Files in the worktree are never rewound**; the Change Set is unaffected. Needs an idle session | W asserts the worktree file is unchanged after navigating [integrated] |
| Fork / clone | Supported | `session.fork {entry_id, position: before|at}` → pi `AgentSessionRuntime.fork`. The service **rebinds** the Objective to a new LADE session id (association revision + 1): input or feedback addressed to the old session is refused as `stale_association`; a system item says no approval, verification or Git state was carried over | W; S (`stale_association` after fork) [integrated] |
| Import a pi session (JSONL) | Supported | `session.import {path}`: the service copies the file (regular file, ≤ 16 MiB, first line a pi `session` header) into `<session_dir>/imports/`, the worker imports the copy with the Objective worktree as cwd. Conversation metadata (entries, labels, model/thinking changes) is preserved; **approvals, verifications and Git state are never restored** (they are LADE records, not transcript entries); the session is rebound as for a fork | W "export … import restores conversation metadata only"; S [integrated] |
| Export | Supported (JSONL) | `session.export` writes the current branch as pi JSONL into `<session_dir>/exports/` and replies with the path | W; S [integrated] |
| HTML export | Unresolved | pi's HTML export is a TUI/CLI asset path not exposed by the SDK root exports used here | — |
| New / switch to an arbitrary pi session | Restricted | One Agent Session per Objective; another session is reached by import (copy) or by a new Objective. Switching to sessions outside the Objective's session dir would read foreign state | — |
| Session name, labels | Supported read-only | Labels and session-info entries are shown in the tree; setting them is not offered | W [integrated] |
| Session statistics / usage | Unresolved | Not mapped to the protocol | — |
| Durability of the first user message | Restricted (inherited) | pi delays the first write until an assistant entry exists and does not fsync; LADE's own accepted input is durable | [limitations](limitations.md#service-durability-and-recovery) |

### Models and thinking

| Promise | Class | How / reason | Evidence |
| --- | --- | --- | --- |
| Model list from the SDK registry | Supported | `models.list` is answered from the pinned SDK's `ModelRuntime` catalogue (40 providers; the offline `lade-fixture` test provider is added only when `laded` runs with the test-only `LADE_TEST_FIXTURE_PROVIDER=1` switch, see [service.md](service.md#test-only-fixture-provider)), produced by `src/worker/catalog.ts` in a network-less sandbox and cached by the service; it replaces the client's static table. Each provider carries its access status and reason | S `models_come_from_the_sdk_catalogue_…` [integrated]; D |
| Model and thinking selection | Supported | Picker with the model's supported thinking levels (pi-ai `getSupportedThinkingLevels`). `execution.start` takes an optional `thinking`; `session.configure` changes model and thinking of an idle live session (pi `setModel`, `setThinkingLevel`, clamped by pi) or, when stopped, persists them for the next start/resume. Persisted per Objective (model on the Objective, thinking in `session_preferences`) | W "manual compaction, model/thinking configuration …"; S `model_and_thinking_selection_is_persisted_per_objective` [integrated] |
| Changing the **provider** of a live session | Restricted | A launch releases one provider's credentials and egress policy; changing provider needs stop → configure → resume (explicit error otherwise) | S [integrated] |
| Model cycling shortcuts | Restricted | Replaced by the explicit picker (no TUI cycling keys) | — |
| Model catalogue network refresh | Restricted | `allowModelNetwork: false`; the pinned catalogue only | — |

### Retry and compaction

| Promise | Class | How / reason | Evidence |
| --- | --- | --- | --- |
| Automatic retry | Supported | pi's own retry settings; `auto_retry_start`/`auto_retry_end` become system activity items ("Retrying model request", then "Retry succeeded" or "Retry gave up") | W (fixture `transient` directive) [integrated] |
| Automatic compaction | Supported | pi's default; `compaction_start`/`compaction_end` become activity items | W [integrated] |
| Manual compaction | Supported | `session.compact {instructions?}` on an idle session; "Nothing to compact" from pi is an explicit `conflict` | W; S [integrated] |
| Retry controls (toggle, delays) | Unresolved | pi defaults only; no setting surface | — |

### Resources, skills, templates

Precedence: context files are the LADE profile's `AGENTS.md` first, then the worktree root's own context file in pi's
order (`AGENTS.override.md`, `AGENTS.md`, `AGENTS.MD`, `CLAUDE.md`, `CLAUDE.MD`; first match). Skills, prompt templates
and extensions come only from the LADE profile snapshot; pi's collision rule (first match wins, with a diagnostic)
applies within it. pi auto-discovery is off (`noExtensions`, `noSkills`, `noPromptTemplates`, `noThemes`,
`noContextFiles`) and only explicit LADE paths are added.

| Promise | Class | How / reason | Evidence |
| --- | --- | --- | --- |
| Context files | Supported | As above; the loaded set is reported per generation (`resources.loaded`, activity item "Session resources") | W "profile resources load with documented precedence …" [integrated] |
| Skills (`/skill:name`, progressive disclosure, relative resources) | Supported | LADE profile `skills/`, loaded by pi unchanged | W [integrated] |
| Prompt templates | Supported | LADE profile `prompts/`. **Expanded exactly once, by pi** (`expandPromptTemplates: true` on the single delivery path); the client and service never expand | W "a prompt template is expanded exactly once" [integrated] |
| Extensions (tools, commands, events, custom entries) | Supported with approval | LADE profile `extensions/` (file or directory). Each is approved by **sha256 of its content** (file bytes, or a sorted `sha256sum`-style manifest of a directory; symlinks/special files refused; ≤ 256 files, 8 MiB). The service records decisions by (name, hash); the worker re-hashes before loading and refuses anything changed. **Changed content needs a new approval** and a reload | W "an extension whose content changed since approval is refused"; S `executable_resources_load_only_by_approved_hash_…` [integrated] |
| Extension commands while idle | Supported | Started without blocking the worker loop, so their dialogs can be answered | W [integrated] |
| Manual reload | Supported | `session.reload`: new snapshot with current approvals, pi reload, **generation + 1**, open dialogs closed as `replaced` | W "reload starts a new generation …" [integrated] |
| Repository `.pi/` resources | Restricted | Repository content is untrusted (`defaultProjectTrust: "never"`); only the root context file is read | W asserts repository `.pi` is not loaded [integrated] |
| `SYSTEM.md` / `APPEND_SYSTEM.md` | Restricted | LADE's system note is the only system-prompt addition | — |
| Themes | Restricted | The GTK client renders text itself; `setTheme` returns an explicit failure | — |
| pi packages (npm/git install) | Restricted | Package acquisition is a host effect; graphical package management is excluded by the spec | — |
| `settings.json` (global/project) | Restricted | In-memory settings chosen by LADE | — |
| Extension-registered providers | Restricted | Their hosts have no reviewed egress policy, so the gateway refuses the connection; define them as custom providers instead | — |
| Executable extension isolation from the session | Restricted (accepted) | An approved extension runs with the worker's authority inside the sandbox, including access to the released key | [limitations](limitations.md#network-and-credentials) |

### Extension UI

Dialogs are routed to the client as `ui.request {ui_id, generation, prompt}` and answered with `ui.respond`. An
answer for another generation, an unknown or already answered dialog is refused; a reload, fork or import closes open
dialogs as `replaced`; a late answer after that is refused. **Nothing is ever answered automatically**: a dialog stays
open until the developer answers or cancels (window close = cancel), the extension's own timeout or abort signal
fires (`closed: timeout`, which pi treats as a refusal), or the session is replaced.

| Promise | Class | How / reason | Evidence |
| --- | --- | --- | --- |
| `select`, `confirm`, `input`, multiline `editor` | Supported | Native GTK dialogs (`views/session.rs` `DialogHost`) | W "extension dialogs are routed with a generation …", "… timeout closes it as a refusal"; S [integrated]; D |
| `notify` | Supported | System activity item (error flag kept) | W [integrated] |
| Status, widgets, footer/header, title, working indicator, autocomplete, tools-expanded | Restricted (display-only no-op) | TUI surfaces with no decision content; ignored, never answered | — |
| `custom()` components, editor control (`pasteToEditor`, `setEditorText`, `getEditorText`, `setEditorComponent`) | Restricted (fails explicitly) | Terminal-only; the call throws/rejects with a LADE error naming the surface, so a required decision is never fabricated | W "terminal-only extension UI fails explicitly" [integrated] |
| Raw terminal input | Restricted | No terminal (IMP-012) | — |

### Providers and authentication

Classification per provider is in [`pi-compatibility.json`](pi-compatibility.json): **17 supported** and **23
restricted**, each with its reason, shown in the picker. Supported
providers work with their API key, and where pi accepts a plain variable for subscription access, with that token
(`ANTHROPIC_OAUTH_TOKEN` for Claude Pro/Max, `KIMI_API_KEY` for Kimi Code). Credentials come from `laded`'s
environment, the user's Secret Service (`laded credentials set <provider>`) or `~/.config/lade/credentials.env`
(0600), in that order; only the selected provider's variables are released, and the gateway allows only that
provider's reviewed host.

| Promise | Class | How / reason | Evidence |
| --- | --- | --- | --- |
| API-key providers | Supported (17) | anthropic, openai, google, openrouter, mistral, groq, xai, deepseek, cerebras, together, fireworks, moonshotai, huggingface, nvidia, zai, vercel-ai-gateway, kimi-coding | `supervision/provider_access.rs` tests, S [integrated]; **no live provider run** |
| Subscription access as a token variable | Supported | `ANTHROPIC_OAUTH_TOKEN`, `KIMI_API_KEY` | `supervision/provider_access.rs` tests [fixture]; **no live run** |
| Interactive OAuth login / refresh (Claude Pro/Max login, ChatGPT subscription login for `openai` and the legacy `openai-codex`, GitHub Copilot, xAI, OpenRouter OAuth, Kimi Code, Meta (Muse), Radius) | Restricted | Refresh-token custody (trusted login helper, who refreshes, where the rotated token is kept) is an open product decision; tokens in the sandbox's auth store would be readable by every tool, and refreshes go to identity hosts outside the reviewed policy. Copilot also resolves per-account hosts | manifest; `supervision/provider_access.rs` tests |
| WebSocket transport | Supported through the gateway (not needed by any supported provider) | Only `openai-codex` (restricted above) uses one in pi-ai 1.5.0, via the global `WebSocket`, which LADE's undici dispatcher tunnels like `fetch`; the `ws` package (Google Live, OpenAI Realtime) is not used by pi and has no network in the sandbox | `supervision_gateway.rs` wss test, `worker-gateway.test.ts` [integrated] |
| Cloud credentials (Bedrock, Vertex) | Restricted | AWS profiles/keys and ADC files are not released; the Bedrock SDK's `node:http` transport bypasses the gateway | manifest |
| Account-specific endpoints (Azure, Cloudflare AI Gateway / Workers AI, OpenCode Zen/Go, Radius) | Restricted | No reviewed per-account egress policy; a custom provider with the exact endpoint is the supported route | manifest |
| Other catalogue providers without a reviewed policy (ant-ling, baseten, meta, minimax, minimax-cn, moonshotai-cn, qwen token plans, typesafe (classifier models only), xiaomi and token plans, zai-coding-cn) | Restricted | No reviewed egress policy in this build (adding one is a reviewed table change in `gateway/policy.rs`/`credentials.rs`) | manifest |
| Custom providers (`models.json`) | Supported with trust rules | LADE profile `models.json`: https, or http only to local hosts; credentials only as one whole `$LADE_PROVIDER_*` variable (literal keys only for local endpoints); no `!command` values or other interpolation; no `oauth`; no redefinition of a built-in provider; egress to exactly the declared host:port (private addresses only for local endpoints) | `adapters/profile` tests, S catalogue test [integrated] |
| Graphical login / logout | Restricted | Follows from the OAuth restriction; keys are stored with `laded credentials set` (Secret Service) or in `credentials.env` | — |

### Other pi compatibility areas

| Promise | Class | How / reason |
| --- | --- | --- |
| Built-in tools `read`, `bash`, `edit`, `write` | Supported | pi's own tools, unmodified (IMP-008) |
| Parallel tool execution | Supported (pi behaviour) | Unchanged pi agent loop; not separately exercised by an IMP-011 test |
| Steering, follow-up, queue clear, abort | Supported | IMP-008 (`worker.test.ts`) |
| Direct user shell (`!cmd`) | Restricted | pi's `!cmd` user shell is not routed: input starting with `!` goes to pi as ordinary text, never to a shell. The IMP-012 contained terminal exists (a separate PTY in the Objective sandbox, per Objective), but it is not wired to `!cmd` |
| Image input | Unresolved | Not part of the input protocol; tool images appear as `[image]` |

## Protocol and generations

New client requests (additive to v1): `models.list`, `session.tree`, `resources.list` (queries);
`session.navigate`, `session.fork`, `session.import`, `session.export`, `session.compact`, `session.reload`,
`session.configure`, `resource.decide`, `profile.import`, `ui.respond` (mutating). Events: `session.changed`,
`ui.request`, `ui.closed`, `resources.changed`. Worker bodies: `session.command`, `ui.respond` (service → worker);
`session.result`, `session.changed`, `resources.loaded`, `ui.request`, `ui.closed` (worker → service). Details in
[protocol-v1.md](protocol-v1.md) and [worker.md](worker.md).

The worker's session generation starts at 1 and increases on every reload, fork and import; dialogs are bound to it.
One session command runs at a time (a second one fails at once, in the worker and in the service); commands that
change the session need an idle session; a command that does not finish within 180 s is answered `interrupted`.

## Not covered here

- No live provider has been exercised (credentials are never used in tests); HTTPS through the gateway is tested
  against a local TLS origin with a test certificate
  ([execution-safety.md](execution-safety.md#egress-gateway)), not against real provider hosts.
- Nothing has run on the supported Mint 22.3 / Cinnamon / X11 host; the new dialogs and picker have no Orca or
  keyboard walk-through.
- Exhaustive recertification of upstream reasoning/compaction internals is excluded by the spec.
