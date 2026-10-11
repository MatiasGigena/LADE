# LADE user guide (first release, bundle 0.2.0)

LADE lets you delegate implementation to an agent while you keep understanding and acceptance: you define an
**Objective**, the agent works in an isolated Git worktree, you inspect the resulting **Change Set**, send
**Contextual Feedback** on exact lines, and you alone freeze, verify, approve and integrate it. Vocabulary:
[CONTEXT.md](../CONTEXT.md). Everything this build does not do yet is listed in
[limitations.md](implementation/limitations.md) — read it before using LADE on work that matters.

## 1. Install

Supported platform: Linux Mint 22.3 / Cinnamon / X11 / x86_64 (Ubuntu 24.04 works for development). Required on the
host (the installer checks each and refuses to install with a list of what is missing):

| Requirement | Mint / Ubuntu package |
| --- | --- |
| bubblewrap, working unprivileged user namespaces | `bubblewrap` |
| `prlimit` | `util-linux` |
| Git ≥ 2.38 | `git` |
| Node.js 22 (tested 22.23.1) on `PATH`, or pass `--node` | NodeSource or an official Node 22 tarball |
| GTK ≥ 4.14 and GtkSourceView ≥ 5.12 runtime libraries | `libgtk-4-1`, `libgtksourceview-5-0` |

From a source checkout (see [build.md](implementation/build.md) for the toolchain bootstrap):

```sh
./scripts/bundle                 # release build + dist/lade-0.2.0 with MANIFEST.sha256
./scripts/install                # per-user, into ~/.local; no sudo, no network
systemctl --user daemon-reload
systemctl --user enable --now lade.service
```

`scripts/install --enable` does the last two steps for you; `--prefix DIR` installs elsewhere. A bundle directory
carries its own `install` and `uninstall` scripts (`dist/lade-0.2.0/install`). The installer verifies every file
against `MANIFEST.sha256` before and after copying and refuses a modified bundle. The bundle is **not signed**: only
install bundles you built or obtained from a trusted source.

Installed layout: `~/.local/lib/lade/<version>/` (with `current` and `previous` links), `~/.local/bin/laded`,
`~/.local/bin/lade-desktop`, the menu entry and icon under `~/.local/share`, and the user unit
`~/.config/systemd/user/lade.service`.

- **Update**: build or obtain the new bundle and run its `install` again; the previous version is kept.
- **Roll back**: `~/.local/lib/lade/current/install --rollback`, then `systemctl --user restart lade.service`. If the
  newer version already migrated the database, the older service refuses to start and leaves the data untouched.
- **Uninstall**: `~/.local/lib/lade/current/uninstall`. Your data is kept (see §9). `--purge-data` also deletes it
  after you type a confirmation at the terminal.

### Running from a source checkout instead

```sh
./scripts/cargo build --locked
./target/debug/laded serve &     # uses packages/pi-worker from the checkout
./target/debug/lade-desktop
```

## 2. Start

Open **LADE** from the applications menu (or run `lade-desktop`). The window connects to the per-user service
`laded`; if it is not running, the window starts the `laded` installed next to it in the background (`--no-spawn`
disables that; `--open PATH` opens a repository at start). Prefer the systemd unit, which also stops every agent
sandbox with the service. LADE runs as one window: launching it again brings the open window to the front instead of
opening a second one (and the service accepts only one controlling window anyway).

- **Close the window** to step away: the service and running agents **keep working**; terminals end. Reopening the
  window shows the same state.
- **Exit** (the power button in the header, or Ctrl+Q, after a confirmation) stops LADE: running agents are stopped
  and recorded as interrupted, terminals end, the service exits. Nothing resumes on its own when LADE starts again —
  you resume each Objective explicitly. A window that does not control the service (another window does, or it is
  disconnected) only closes.
- **Logging out** with the window open does the same as Exit. Without a window, the per-user unit stops with your
  session and ends every agent sandbox with it. `systemctl --user stop lade.service` also stops everything; in every
  case running executions are marked interrupted and never resumed automatically.
- **Notifications.** When an Objective starts to need you, or becomes ready for review, while the window is not in
  front, LADE shows a desktop notification. Clicking it brings the window forward on that Objective (its Evidence
  for a review) — it never approves or sends anything. The Objectives list always shows the same attention, so if
  your desktop has no notification service, nothing is lost: the window says once that notifications are
  unavailable.

Keyboard: F6 / Shift+F6 move between regions, Ctrl+1 Workbench, Ctrl+2 Activity, Ctrl+3 Terminal (and focus it),
Ctrl+Shift+E leave the terminal, Ctrl+K search, Ctrl+N new Objective, Ctrl+O open repository, Ctrl+Shift+M add
feedback on the selection, Ctrl+Shift+F switch diff/full file, Ctrl+Enter send a message, Ctrl+Q Exit, Escape
closes a dialog, Ctrl+? (or the keyboard button in the header) lists every shortcut. In that dialog each action except F6 / Shift+F6 and Escape can be given a function key F1–F35, optionally
with Shift, Control or Alt (other combinations stay with text fields for typing, editing and the clipboard); Apply
checks the whole set, including duplicates, before using it and saves it to
`$XDG_CONFIG_HOME/lade/keybindings.json` (default `~/.config/lade/keybindings.json`). An invalid file is ignored with
a notice and the defaults apply; delete the file to return to them.

Below about 1100 pixels of window width (for example 800 × 550, or a 1600 × 1100 screen at 200 % scaling) the
window shows one region at a time: switch between **Objectives**, **Work area** and **Overview · Evidence ·
Feedback** with the buttons below the header or F6, and use **Objective details** to show the outcome and journey.
Text you type or paste into drafts, messages and intents is limited to 256 KiB per field (an override reason to
2,048 characters); a paste that would exceed it is not inserted and your text stays as it was.

## 3. Open a repository (Workspace)

Ctrl+O and choose the **top-level directory** of a non-bare Git repository. LADE never runs the repository's hooks
(including hooks defined in configuration), filters or credential helpers, and never edits your checkout except when
you integrate into a checked-out target branch (§7).

- **Git LFS and other filters are not supported.** LADE compares and stores the raw bytes in the worktree; files whose
  `filter` attribute names a driver are marked *filtered (LFS) — raw bytes, unsupported* in the Change Set, with a
  notice above the file list.
- A nested repository without any commit inside the Objective's worktree is left out of the Change Set and named in a
  notice (commit inside it, or move it out). A nested repository with a commit is recorded as a submodule entry.
- Ignore rules are the repository's `.gitignore` files and `.git/info/exclude` plus **your own global excludes
  file** (`core.excludesFile` in `~/.gitconfig` or `~/.config/git/config`, else `~/.config/git/ignore`). A
  `core.excludesFile` set in the repository's own configuration is ignored. File modes and symlinks are always
  recorded exactly, whatever the repository's `core.fileMode`/`core.symlinks`.
- Branch names that are not valid UTF-8 are listed with `\xHH` escapes; they cannot be chosen as an integration
  target.

## 4. Create an Objective

Ctrl+N. Give a title and the intent — **outcome**, **constraints** and **acceptance** criteria — and choose the
**Integration Target** branch (where an accepted result will land) and the starting point (the target's tip or a
specific commit). The intent stays editable until approval. Nothing runs yet: the worktree and branch
`lade/<id>` are created on the first run, under LADE's data directory, never in your checkout.

## 5. Choose a model and provide credentials

Pick a model when starting execution. `lade-fixture / scripted` is an **offline test model** (always labelled as
such): it writes files only when told `lade-fixture: write <path> <content>` and is useful for trying the workflow.

Real providers read API keys from `laded`'s environment or from `~/.config/lade/credentials.env`, which must be a
regular file owned by you with mode 0600 (`chmod 600 ~/.config/lade/credentials.env`); LADE refuses it otherwise.
Format: `NAME=value` lines, `#` comments, optional `export `. The environment wins over the file; it is re-read at
each start of execution; only the selected provider's variables reach the sandbox.

| Provider | Variables |
| --- | --- |
| `anthropic` | `ANTHROPIC_API_KEY` (or `ANTHROPIC_OAUTH_TOKEN`, `ANTHROPIC_AUTH_TOKEN`) |
| `openai` | `OPENAI_API_KEY` |
| `google` | `GEMINI_API_KEY` |
| `openrouter` | `OPENROUTER_API_KEY` |
| `mistral` | `MISTRAL_API_KEY` |
| `groq` | `GROQ_API_KEY` |
| `xai` | `XAI_API_KEY` |
| `deepseek` | `DEEPSEEK_API_KEY` |
| `cerebras` | `CEREBRAS_API_KEY` |
| `together` | `TOGETHER_API_KEY` |
| `fireworks` | `FIREWORKS_API_KEY` |

Subscription logins (OAuth), AWS Bedrock and Vertex are not available. The agent's tools can read the key inside
the sandbox, and the agent can reach only the provider's API host (HTTPS port 443) through LADE's gateway; a proxy in
`HTTPS_PROXY` of the service is honoured. No credential is stored by LADE.

## 6. Run, watch and steer

**Start** runs a pi Agent Session in a bubblewrap sandbox that can write only the Objective's worktree and its
session directory. If the sandbox cannot be created, execution is refused — LADE never runs an agent unsandboxed.

Limits: the worktree plus the session directory may use 2 GiB of disk; LADE checks every few seconds and, above
that, stops the agent with **limit reached** (free space, then **Resume**; the limit is never raised by itself).
Under the systemd unit each Objective's agent, terminals and checks also share 4 GiB of memory and 512
processes; started by hand, LADE logs `cgroup limits unavailable (…)` and uses per-process limits only.

- **Activity** (Ctrl+2) shows the transcript: assistant text, thinking, tool calls and results, system notes.
- The composer sends a **prompt**, a **steer** (delivered after the current tool calls) or a **follow-up**
  (delivered when the agent would stop). Every message is durably accepted before it is delivered; its state
  (queued, dispatched, received, uncertain, blocked) is shown. An *uncertain* message is never resent automatically.
- **Cancel** stops the agent and keeps its work and session; **Resume** continues the same session.

### Terminal

**Terminal** (Ctrl+3) opens a shell for the selected Objective — for running its tests, `git status`, `ls` and the
like next to the agent. It is not a general shell: it runs inside the same sandbox as the agent, in the Objective's
worktree only, with the same limits, **no network** and **no API keys**; the repository's `.git` is read-only, so
commits and branch changes are LADE's (freeze and integrate). The prompt starts with `[lade]`.

- While the Objective is in progress the terminal can change the worktree, and those changes show up in the Change
  Set like the agent's. **Freeze for review** first closes every such terminal. While a candidate is frozen or
  approved, a terminal opens **read-only** (`[lade read-only]`) for inspection.
- An Objective needs a worktree first: start its agent once.
- Keys go to the shell (Tab, Ctrl+C, Ctrl+D, arrows…). Leave the terminal with **Ctrl+Shift+E** (focus returns to
  where it was) or F6; come back with Ctrl+3. Ctrl+Shift+C / Ctrl+Shift+V copy and paste (pastes up to 256 KiB).
  All of these can be remapped in the shortcuts dialog (function keys).
- The terminal is plain text (no colours); full-screen programs work in a basic way. Very fast output is slowed down
  and, if it still cannot keep up, shortened with a visible `[… N bytes of output skipped …]` marker.
- Closing the window, losing the connection, Exit or logout ends the terminal and everything started in it; it is
  never reopened automatically.

## 7. Inspect, give feedback, review and integrate

1. **Inspect.** The Workbench lists the changed files of the Change Set with a diff or full-file view. Each view
   names its version (working copy capture or a frozen candidate). Ctrl+K searches Objectives, paths or content;
   opening a content result shows the full file with the first match selected, ready for *Add feedback*. Files
   larger than 64 MiB (up to 1 GiB) open in chunks of up to 256 KiB — *Load more* continues; larger files cannot be
   inspected.
2. **Feedback.** Select text in a diff or file and press Ctrl+Shift+M (or *Add feedback*). Drafts are stored by the
   service (each shows whether its latest edit is saved) and survive closing the window. **Submit** sends all selected drafts together to the identified
   Agent Session after you confirm the destination. If any referenced text changed since you selected it, the
   **whole batch** is refused as stale and nothing is sent; re-select and resubmit. Feedback is guidance, never
   approval.
3. **Freeze.** When the work looks ready, *Freeze* stops the agent and records an immutable **candidate**. *Return
   for revision* goes back to work (and invalidates an approval); comparing a later candidate with an earlier one
   shows exactly what the revision changed.
4. **Verify.** *Run check* runs a command (for example `cargo test` or `git diff --check`; the program must live in
   `/usr/bin` or `/bin`) in a sandboxed disposable copy of the candidate. Its exit status and output are recorded by
   LADE; agent claims are never treated as verification.
5. **Approve.** Approval binds the exact candidate and the current target commit. It requires passing checks or an
   explicit written override reason. If the target branch moves, the approval is invalidated: *Refresh* rebuilds the
   candidate on the new target (or reports a conflict), then verify and approve again.
6. **Integrate.** A separate, explicit action creates one squash commit on the target branch. If the target is
   checked out, it must be clean and not mid-merge/rebase; otherwise integration is refused and nothing changes.
   An untracked file in the target checkout where the candidate adds one is named in the refusal; move or remove it
   and integrate again. LADE never stashes, resets or force-updates.

## 8. Finish: complete, discard, clean up

- **Complete without changes** — for an Objective whose Change Set is empty.
- **Discard** — abandon the Objective without integrating; its record and history remain.
- **Clean up** — after integration, completion or discard, removes the Objective's worktree and branch. If the
  worktree holds work not in the branch, LADE asks you to confirm the data loss. Records, history, retention refs
  (`refs/lade/retain/*`) and artifacts stay.
- **Delete Git history** — only after Clean up, and only when you confirm the dialog: deletes the Objective's retention
  refs (and its `lade/*` branch if one is left), so Git may later garbage-collect its captures and candidates; they can
  then no longer be inspected. Integrated commits, the Objective record, accepted inputs, review evidence and stored
  artifacts stay. LADE never does this on its own.

## 9. Where data lives

| What | Where |
| --- | --- |
| Database, artifacts, Objective worktrees, Agent Session files, verification copies | `~/.local/state/lade/` (`$XDG_STATE_HOME/lade`) |
| Control socket and capability (private, rotated at every start) | `$XDG_RUNTIME_DIR/lade/` |
| Credentials (optional, yours) | `~/.config/lade/credentials.env` |
| Service diagnostics | the user journal: `journalctl --user -u lade.service` (codes and ids, no prompts, source or keys) |
| In your repositories | branches `lade/*`, retention refs `refs/lade/retain/*`, registered worktrees |

Uninstalling never deletes `~/.local/state/lade` unless you ask with `--purge-data` and confirm. Back it up like any
other project data; its history is kept until you delete it. Retention refs in your repositories are removed only by
*Delete Git history* (§8).

## 10. Troubleshooting

- **"Sandbox unavailable" / execution refused.** `bwrap` is missing or cannot create user namespaces. Check
  `bwrap --unshare-all --ro-bind / / true`. On systems that restrict unprivileged user namespaces (for example
  AppArmor's `kernel.apparmor_restrict_unprivileged_userns=1`), allow bubblewrap through your distribution's
  supported mechanism; LADE does not change host security settings. Inspection and review keep working without the
  sandbox, but agents and checks do not run.
- **"cgroup limits unavailable (…)" in the journal.** The per-Objective memory/process limits need the systemd unit
  (`Delegate=memory pids cpu`) on a cgroup v2 system; the reason names what is missing. LADE keeps working with
  per-process limits; `LADE_CGROUPS=off` disables the cgroup limits explicitly.
- **"Worker package not found" / "node runtime not found" in the journal.** The unit sets `LADE_WORKER_DIR` and a
  `PATH` containing the Node 22 recorded at install time. If you moved or upgraded Node, re-run the installer
  (optionally with `--node /path/to/node`). The launchers `~/.local/bin/laded` and `~/.local/bin/lade-desktop`
  set both, so a `laded` started by hand or by the window (when the unit is not running) finds them too; running
  the binaries under `~/.local/lib/lade/` directly does not.
- **"Service unavailable".** Start it: `systemctl --user start lade.service`; inspect
  `systemctl --user status lade.service` and `journalctl --user -u lade.service`. Exit status 78 means it refused to
  start: unsafe runtime directory permissions, a corrupt or newer database (never reset automatically — restore from
  backup or reinstall the newer version), or another `laded` already using the same data directory.
- **"Another window controls the service."** Only one window may control LADE at a time. Close the other window (or
  its process); the control is released as soon as it disconnects.
- **Provider unavailable.** The selected provider's API key is missing (see §5), `credentials.env` has the wrong
  owner/mode, or the provider is not supported. The message names the missing variables, never their values.
- **Integration refused.** The checked-out target branch has uncommitted changes or a merge/rebase in progress, or
  the target moved since approval (refresh, verify and approve again).
