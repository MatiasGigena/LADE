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
- No live provider has been exercised end to end (credentials are never used in tests), including the subscription
  token path (`ANTHROPIC_OAUTH_TOKEN`) and the provider hosts added by IMP-011. **IMP-011** (needs an authorised live
  account).
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
| `SIGTERM` is not trapped by `laded` (no `unsafe` handlers); recovery on the next start is the contract. Graceful stops use `service.stop` (Exit; logout while the window runs) | A `systemctl stop` or session end without a window kills executions and verifications abruptly (marked interrupted on the next start, never resumed) | IMP-014 |
| A `laded` started by hand or by the window (not the systemd unit) keeps running after a logout that happens with no window open | Its agents keep running until it is killed; use the unit (the installer's default) | IMP-014 |
| Terminal quotas are the per-process worker `prlimit` values, not an aggregate per-Objective budget; a terminal and the agent are concurrent writers of an active Objective's worktree until a freeze | A heavy command in the terminal competes with the agent; terminal edits appear in the Change Set | IMP-013 |
| A terminal needs an existing worktree (the agent must have run once); the shell is `bash`/`sh` from `/usr/bin`/`/bin`, not the user's login shell or dotfiles | No terminal for a never-started Objective; no personal shell configuration inside | — (by design; revisit with Matias if needed) |

## Network and credentials

| Limitation | Impact | Tracked |
| --- | --- | --- |
| Only undici / global `fetch` traffic is tunnelled through the gateway. `node:http(s)` clients (e.g. the Bedrock SDK) and WebSocket transports have no network at all | Providers needing them cannot work (fail closed, not a bypass) | IMP-007, IMP-011 |
| Allowlist is per provider host, not per request; any code in the sandbox can use the tunnel with the released key. Content sent to an allowed host is not inspected (not DLP) | A malicious tool can exfiltrate to the provider host using the user's key | IMP-007 |
| Through an upstream `HTTPS_PROXY`, private-address checks apply only to IP literals (the proxy resolves names) | Private destinations reachable via a proxy are not blocked by LADE | IMP-007 |
| Credentials come only from `laded`'s environment or `~/.config/lade/credentials.env` (0600). No Secret Service / GNOME Keyring custody, no memory-only mode | Keys rest in a plaintext file or the unit environment | IMP-007, IMP-014 |
| The released key is readable by every process in the sandbox (accepted by #8: no per-extension secrecy) | Any tool run by the agent can read the key | IMP-007 |
| Interactive OAuth logins and refresh-token storage (Claude Pro/Max login, ChatGPT Plus/Pro, Copilot, xAI, OpenRouter OAuth, Radius), cloud credentials (Bedrock, Vertex) and account-specific endpoints (Azure, Cloudflare, OpenCode) are **restricted** with a reason shown in the picker; 23 of 41 catalogue providers are restricted ([pi-compatibility.json](pi-compatibility.json)); no graphical login/logout | Subscription access works only where pi accepts a plain token variable (`ANTHROPIC_OAUTH_TOKEN`, `KIMI_API_KEY`); custom providers need an exact endpoint in the LADE profile | IMP-007, IMP-014 (credential custody) |
| Custom providers: only `$LADE_PROVIDER_*` credential references (literal keys for local endpoints only), no `!command`, no `oauth`, no redefinition of built-ins | Some pi `models.json` files must be adapted before import | IMP-011 (trust rule) |
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
| The controlling-client refusal is a `service.notice` event, not a reply. A second launch of the window is redirected to the running one by `GApplication` only when a session bus exists | Without a session bus a second window can open; it shows "another window controls the service" and gets no other data or authority | IMP-014 |
| Content search returns line numbers, not byte offsets | Search results cannot be used directly as exact feedback references | IMP-004 |
| Files larger than 64 MiB are refused (`unsupported_content`) instead of being streamed | Very large files cannot be inspected | IMP-004 |
| No persistent diagnostics/log rotation or export; diagnostics are stderr only (the user journal when run under systemd) | No bounded diagnostic history or redacted export (#14 policy) | IMP-003, IMP-014 |
| No storage-pressure handling beyond SQLite/IO errors (no reserve, no visible warning before failure) | Disk-full surfaces as failed operations | IMP-013 |
| Fault-injection coverage of every cross-store crash window (A09/A10 matrix) is partial: restart interrupts executions and makes dispatch uncertain; integration intents are reconciled; other windows are untested | Some crash windows are covered by design only | IMP-013 |

## Agent Sessions (pi compatibility subset)

| Limitation | Impact | Tracked |
| --- | --- | --- |
| Resources come only from the LADE profile (`~/.local/state/lade/profile`) plus the worktree root's context file; repository `.pi/`, `SYSTEM.md`/`APPEND_SYSTEM.md`, themes, packages and `settings.json` are not loaded ([inventory](pi-compatibility.md)) | Repository-provided pi resources must be copied into the LADE profile deliberately | IMP-011 (trust rule) |
| Approved executable extensions run with the worker's full authority inside the sandbox, including the released provider key | Approval by content hash is a trust decision, not containment | IMP-007 |
| Terminal-only extension UI (`custom()`, editor control) fails explicitly; status/widget surfaces are ignored | TUI-only extensions do not work | IMP-011 (restricted) |
| Changing the provider of a live session needs stop → configure → resume; HTML export, session statistics, retry settings, image input and setting labels are not available | See the [inventory](pi-compatibility.md) (unresolved rows) | IMP-011 |
| Open extension dialogs are not durable: a worker or service restart closes them (`aborted`) and the extension sees no answer | A pending decision must be asked again | IMP-013 |
| Images are not part of the input protocol; tool images appear as `[image]` | Image inputs/outputs are not supported | IMP-011 |
| Activity items over 192 KiB are truncated (the full byte length is shown) | Very large tool outputs are not fully visible | IMP-011 |
| The worker depends on a second copy of `@earendil-works/pi-ai` (≈ +130 MB) and installs with `--ignore-scripts` | Larger bundle; the narrow pi fork (IMP-008) would remove the duplicate | IMP-008 |

## Desktop client and accessibility

| Limitation | Impact | Tracked |
| --- | --- | --- |
| Orca speech has **not** been validated on this client; the IMP-002 (#40) Mint/Orca evidence applied to the superseded fixture component and must be re-run here. 200 % scaling and the compact composition are captured only under Xvfb on the development host (`scale-200-800x550.png`, `compact-800x550.png`), not on Mint/Cinnamon/X11. Contrast is computed for token pairs (`scripts/check-contrast.py`); rendered-surface sampling (`scripts/sample-rendered-contrast.py`) is optional and has no recorded run against the accepted states | A17/A18 accessibility gates are open | IMP-002, IMP-010 |
| Shortcut remapping is limited by policy: remappable actions take their default or F1–F35 (optionally Shift/Control/Alt); F6/Shift+F6 and Escape are fixed. Remaps are a per-user client preference in `$XDG_CONFIG_HOME/lade/keybindings.json`, not synchronised and not product state | Letter/editing combinations cannot be chosen | — (IMP-002 policy; a change needs Matias) |
| The terminal renderer is a minimal VT100 parser in plain monospace text (no colours or attributes, wide characters count as one cell, no input method/dead keys, no mouse reporting); F6 and the window's escape keys never reach the shell | Full-screen programs (`less`, `vim`) work only roughly; IME users cannot compose text in the terminal | IMP-013 |
| Terminal output is rendered as a whole `GtkTextView` text (2,000 lines of scrollback) per main-loop iteration | Very long sessions re-render more text; not measured against the presentation budgets | IMP-013 |
| Under Xvfb on the development host (GTK 4.14, a GTK 4 clipboard owner), a paste of more than about 256 KiB never reached the client's buffer, so the 256 KiB size guard was exercised by pasting into existing text; behaviour with other clipboard owners and on Mint is unverified | A very large paste may fail silently at the toolkit level, before LADE can explain it | IMP-010 |
| Syntax highlighting is off in the file and diff viewer | Plain text only | IMP-010 |
| Dialogs (new Objective, open repository, submit feedback, approve, cleanup) are not captured as evidence | Their appearance is unverified | IMP-010 |
| The model picker, session tree, resource approval and extension dialogs (IMP-011) are captured only with preview fixture data under Xvfb (`evidence/imp-011/`); no keyboard or Orca walk-through | Their accessibility is unverified | IMP-011, IMP-010 |
| Desktop notifications rely on the session's notification service; `GApplication` reports no delivery errors, so only a missing bus or service is detected (then a quiet note under the Objectives list). Activation reaches only a running window (no D-Bus activation of a closed application) | A notification server that accepts and drops a notification is not noticed; attention stays visible in the Objectives list | IMP-014 |
| Logout handling uses `GtkApplication::query-end` (session manager); not validated on Mint/Cinnamon | If Cinnamon does not deliver `query-end`, logout falls back to the systemd user unit stopping (abrupt, still no resume) | IMP-016 |
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
