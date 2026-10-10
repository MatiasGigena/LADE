# Known limitations of the first-release build (bundle 0.2.0)

This is the consolidated list of what the current implementation does **not** enforce, has **not** validated, or
deliberately leaves out. It collects the "not enforced" and "known gaps" sections of
[execution-safety.md](execution-safety.md), [service.md](service.md) and [worker.md](worker.md), the adapter
findings of WP-C and the desktop gaps, so a reader does not have to assemble them. Each item names its impact and the
backlog key ([spec §4–§5](spec.md#4-backlog-and-dependency-graph)) that owns closing it. The per-row acceptance view is
[acceptance-status.md](acceptance-status.md).

Nothing here is silently waived: an item stays open until its owning increment closes it with evidence, or Matias
explicitly revises the underlying contract ([spec §7](spec.md#7-common-definition-of-done-and-change-control)).

## Evidence boundary

- All runtime evidence so far comes from the **development host** (disposable repositories, the `lade-fixture`
  offline model, `scripts/verify`, `scripts/test-install`). Nothing has been validated on a clean installation of the
  supported platform (Mint 22.3 / Cinnamon / X11). Installed-release validation is **IMP-016**.
- No live provider has been exercised end to end (credentials are never used in tests). **IMP-011**.
- The developer comprehension exercise (A23) has not been designed or run; it needs Matias. **IMP-015**.
- The release go/no-go is Matias's decision and has not been taken. **IMP-016**.

## Execution sandbox and resources

| Limitation | Impact | Tracked |
| --- | --- | --- |
| No disk quota on the worktree, session directory or the sandbox's `/tmp` (tmpfs is memory-backed); the §7 2 GiB writable-storage monitor is not implemented either | A runaway agent can fill the disk holding `~/.local/state/lade` or exhaust memory through `/tmp` | IMP-006, IMP-013 |
| Memory is bounded only by `RLIMIT_AS` = 8 GiB of **virtual** address space (Node cannot start under the §7 4 GiB); no cgroup memory limit | Resident memory is not capped per Objective or in aggregate | IMP-006 |
| No delegated cgroup v2 accounting; per-Objective and aggregate CPU/memory/process limits approved by #14 are not enforced | Concurrent Objectives can starve the desktop and the service | IMP-006, IMP-013 |
| `RLIMIT_NPROC` (512) counts all processes of the real UID and is not enforced for root | A fork bomb is limited only together with the user's other processes; no limit when run as root | IMP-006 |
| Supervisor-loss race: if `laded` dies in the first milliseconds of a launch (before bwrap arms its parent-death signal, or before the pidfile / namespace-init line is written), the sandbox can survive unrecorded (observed ≈ 9 of 40 forced races in WP-C tests). The startup sweep only kills recorded processes. Under systemd, `KillMode=control-group` in the shipped unit ends every descendant when the service stops | Outside systemd (manual `laded serve`), a leaked sandbox can keep running until killed | IMP-006, IMP-013 |
| No seccomp filter in the worker sandbox (namespaces, `--cap-drop ALL`, read-only binds only) | Kernel attack surface is not reduced beyond namespaces | IMP-006 |
| bwrap exports `PWD` into the sandbox environment | Reveals the worktree path to the worker (already its cwd) | IMP-006 |
| A hostile tool process can write to the worker's inherited stdout via `/proc` | Forged frames are still schema-validated and bound to one Objective/generation; content is untrusted | IMP-008 |
| `SIGTERM` is not trapped by `laded` (no `unsafe` handlers); recovery on the next start is the contract | A stop interrupts running executions and verifications (they are marked interrupted, never resumed) | IMP-012 |
| No contained terminal (PTY) | There is no interactive shell in the workbench; agents use pi's `bash` tool only | IMP-012 |

## Network and credentials

| Limitation | Impact | Tracked |
| --- | --- | --- |
| Only undici / global `fetch` traffic is tunnelled through the gateway. `node:http(s)` clients (e.g. the Bedrock SDK) and WebSocket transports have no network at all | Providers needing them cannot work (fail closed, not a bypass) | IMP-007, IMP-011 |
| Allowlist is per provider host, not per request; any code in the sandbox can use the tunnel with the released key. Content sent to an allowed host is not inspected (not DLP) | A malicious tool can exfiltrate to the provider host using the user's key | IMP-007 |
| Through an upstream `HTTPS_PROXY`, private-address checks apply only to IP literals (the proxy resolves names) | Private destinations reachable via a proxy are not blocked by LADE | IMP-007 |
| Credentials come only from `laded`'s environment or `~/.config/lade/credentials.env` (0600). No Secret Service / GNOME Keyring custody, no memory-only mode | Keys rest in a plaintext file or the unit environment | IMP-007, IMP-014 |
| The released key is readable by every process in the sandbox (accepted by #8: no per-extension secrecy) | Any tool run by the agent can read the key | IMP-007 |
| Providers authenticated by OAuth logins, profiles or ADC (Copilot/Codex subscriptions, Bedrock, Vertex) have no egress policy and are unavailable; no graphical login/logout | Only API-key providers in the [default policy table](execution-safety.md#egress-gateway) work | IMP-011 |
| HTTPS through the gateway is tested only against a plain-HTTP origin (TLS uses undici's unmodified connector) | End-to-end TLS through the gateway is not covered by tests | IMP-011 |

## Git and repository handling

| Limitation | Impact | Tracked |
| --- | --- | --- |
| Filter drivers (LFS, `clean`/`smudge`) are neutralised; filtered files show as modified; integration into a checked-out target with such files is refused as dirty; `read-tree` writes raw blobs | LFS/filtered repositories are effectively unsupported | IMP-009 |
| Untracked files in the target checkout do not block integration; a collision makes the checkout step fail and the ref is CAS-reverted (`CheckoutFailed{reverted:true}`). Between `update-ref` and `read-tree` there is a short window where the target looks reverse-dirty | A concurrent `git status` in the target can briefly see inconsistent state | IMP-009, IMP-013 |
| The working-basis Change Set compares against the **current** target; after the target moved, target-side changes appear as reverts until a candidate is frozen (candidates merge onto the target) | Working-basis diffs can mislead after the target advances | IMP-010 |
| Non-UTF-8 branch names are omitted from branch lists; a nested empty repository breaks capture; capture forces `core.fileMode`/symlink handling and ignores `core.excludesFile`; `hook.*.command` (Git > 2.43) is not neutralised | Unusual repositories may fail capture; a Git ≥ 2.44 config hook could run during LADE's Git calls | IMP-009 |
| Retention refs `refs/lade/retain/*` and `lade/*` branches are never removed (not even by Cleanup or uninstall) | Repositories accumulate refs; this is deliberate (history until explicit deletion) and there is no deletion UI yet | IMP-013 |

## Service, durability and recovery

| Limitation | Impact | Tracked |
| --- | --- | --- |
| Streamed activity text is flushed every second | A crash can lose up to ~1 s of streamed agent text (accepted inputs and decisions are durable) | IMP-013 |
| pi does not write the first user message before an assistant entry, and pi session files are not fsynced | A crash before the first reply can lose the pi-side record; LADE's own accepted input survives | IMP-008, IMP-011 |
| The controlling-client refusal is a `service.notice` event, not a reply | A second window shows "another window controls the service" and gets no other data | IMP-012 |
| Content search returns line numbers, not byte offsets | Search results cannot be used directly as exact feedback references | IMP-004 |
| Files larger than 64 MiB are refused (`unsupported_content`) instead of being streamed | Very large files cannot be inspected | IMP-004 |
| No persistent diagnostics/log rotation or export; diagnostics are stderr only (the user journal when run under systemd) | No bounded diagnostic history or redacted export (#14 policy) | IMP-003, IMP-014 |
| No storage-pressure handling beyond SQLite/IO errors (no reserve, no visible warning before failure) | Disk-full surfaces as failed operations | IMP-013 |
| Fault-injection coverage of every cross-store crash window (A09/A10 matrix) is partial: restart interrupts executions and makes dispatch uncertain; integration intents are reconciled; other windows are untested | Some crash windows are covered by design only | IMP-013 |

## Agent Sessions (pi compatibility subset)

| Limitation | Impact | Tracked |
| --- | --- | --- |
| Resource discovery is disabled in the worker: no extensions, skills, prompt templates, themes, context files (`AGENTS.md`/`CLAUDE.md`), `SYSTEM.md`, packages or `settings.json` from the profile or repository | pi behaves as a bare agent with LADE's system note only | IMP-011 |
| Extension UI is unsupported (no extensions are loaded) | Extensions requiring UI cannot be used | IMP-011 |
| No session tree/fork/clone/import/export, model/thinking switching during a session, manual compaction or reload | Only one linear session per Objective | IMP-011 |
| Images are not part of the input protocol; tool images appear as `[image]` | Image inputs/outputs are not supported | IMP-011 |
| Activity items over 192 KiB are truncated (the full byte length is shown) | Very large tool outputs are not fully visible | IMP-011 |
| The worker depends on a second copy of `@earendil-works/pi-ai` (≈ +130 MB) and installs with `--ignore-scripts` | Larger bundle; the narrow pi fork (IMP-008) would remove the duplicate | IMP-008 |

## Desktop client and accessibility

| Limitation | Impact | Tracked |
| --- | --- | --- |
| Orca speech, 200 % scaling and rendered contrast have **not** been validated on the rendered surfaces (only design tokens and native captures exist) | A17/A18 accessibility gates are open | IMP-002, IMP-010 |
| Keybindings are defined centrally but cannot be remapped by the user | Fixed shortcuts | IMP-012 |
| Syntax highlighting is off in the file and diff viewer | Plain text only | IMP-010 |
| Dialogs (new Objective, open repository, submit feedback, approve, cleanup) are not captured as evidence | Their appearance is unverified | IMP-010 |
| The model list is static on the client side | New pi models or custom providers cannot be chosen | IMP-011 |
| No notifications, background attention outside the window, or logout handling | Attention is shown only inside the open window | IMP-012 |
| Native presentation latency and memory budgets have not been measured | Performance targets in [performance-budgets.md](../planning/performance-budgets.md) are unverified | IMP-010, IMP-013 |

## Installation and distribution

| Limitation | Impact | Tracked |
| --- | --- | --- |
| The bundle is **not signed**: `MANIFEST.sha256` detects corruption and tampering of a copied bundle but not a replaced bundle with a matching manifest; there is no trust bootstrap or key | Users must obtain the bundle from a trusted build | IMP-014 |
| Node is not bundled: the installer requires the host's Node 22 and records its directory in the unit's `PATH`; a Node upgrade/removal on the host changes or breaks the worker runtime | Worker runtime is not part of the matched set | IMP-014 |
| No migration journal, pre-migration backup or protected rollback window: rolling back after the newer version migrated the database makes the older service refuse to start (exit 78, data untouched) | Rollback across a schema change requires restoring a backup by hand | IMP-014 |
| No update channel or release checking; updates are manual re-installs | — | IMP-014 |
| No shipped-dependency license inventory beyond the files inside `node_modules` and the pi licence; Rust crate licences are not collected | License inventory incomplete | IMP-014 |
| The systemd unit avoids namespace-creating hardening (`ProtectSystem=`, `PrivateTmp=`, `RestrictNamespaces=`) because `laded` must create bubblewrap user namespaces; it has been checked with `systemd-analyze verify` but not run under a real user manager in CI | Service hardening is limited; unit behaviour on Mint is unvalidated | IMP-014 |
| The installer checks prerequisites on the host it runs on; Mint package names and versions are documented, not verified by a clean-host run | Clean-host install is unvalidated | IMP-014, IMP-016 |
| CI runs on Ubuntu 24.04 with AppArmor's unprivileged-userns restriction relaxed on the disposable runner; it does not certify the supported Mint host | CI green is build/contract evidence, not platform validation | IMP-016 |
