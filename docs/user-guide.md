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
| `bash` in `/usr/bin` or `/bin` (sandbox launch gate) | `bash` |
| Git ≥ 2.38 | `git` |
| Node.js 22 (tested 22.23.1) on `PATH`, or pass `--node` — not needed when the bundle carries its own Node | NodeSource or an official Node 22 tarball |
| GTK ≥ 4.14 and GtkSourceView ≥ 5.12 runtime libraries | `libgtk-4-1`, `libgtksourceview-5-0` |
| `ssh-keygen` (only to verify a signed bundle) | `openssh-client` |

Optional: `libsecret-tools` (`secret-tool`) to keep provider keys in your keyring (§5); the installer does not
check it.

From a source checkout (see [build.md](implementation/build.md) for the toolchain bootstrap):

```sh
./scripts/bundle                 # release build + dist/lade-0.2.0 with MANIFEST.sha256
./scripts/install                # per-user, into ~/.local; no sudo, no network
systemctl --user daemon-reload
systemctl --user enable --now lade.service
```

`scripts/install --enable` does the last two steps for you; `--prefix DIR` installs elsewhere. A bundle directory
carries its own `install` and `uninstall` scripts (`dist/lade-0.2.0/install`). The installer verifies every file
against `MANIFEST.sha256` before and after copying and refuses a modified bundle.

- **Signatures.** `./scripts/bundle --sign KEY` (or `scripts/sign-bundle DIR KEY`) signs `MANIFEST.sha256` with an
  SSH key (`ssh-keygen -Y sign`, namespace `lade-release`). The installer checks the signature against an
  *allowed-signers* file — `--allowed-signers FILE`, or `~/.config/lade/allowed_signers` when it exists (yours, mode
  0600 or 0644, one line per trusted key: `name namespaces="lade-release" ssh-ed25519 AAAA…`). With such a trust root
  a bundle with a bad or foreign signature, or no signature, is refused before anything is written
  (`--allow-unsigned` accepts an unsigned one with a warning). Without a trust root the installer warns that the
  signature (or the bundle) is **not verified**. There is no official LADE release key yet: which key signs releases
  and how you get its public half is still to be decided, so for now trust only bundles you built or whose signer
  you know.
- **Node.** `./scripts/bundle --with-node DIR` copies the pinned Node (`DIR/bin/node` of an official
  `node-v22.23.1-linux-x64` distribution) into the bundle (about +120 MB, the bundle is then ≈ 390 MB unpacked). The
  installer then uses that Node — it becomes part of the matched set and a host Node upgrade no longer changes the
  worker runtime — and falls back to the host's Node 22 (checked as above) only for bundles without one, or when
  you pass `--node`.
- **Licences.** `share/doc/lade/THIRD_PARTY.md` (and `.json`) lists every shipped Rust crate, npm package and the
  bundled Node with its licence; Rust licence texts are in `share/doc/lade/LICENSES/`.

Installed layout: `~/.local/lib/lade/<version>/` (with `current` and `previous` links), `~/.local/bin/laded`,
`~/.local/bin/lade-desktop`, the menu entry and icon under `~/.local/share`, and the user unit
`~/.config/systemd/user/lade.service`.

- **Update**: build or obtain the new bundle and run its `install` again; the previous version is kept.
- **Roll back**: `~/.local/lib/lade/current/install --rollback`, then `systemctl --user restart lade.service`.
  Before a version migrates your database to a newer format it saves a backup (in
  `~/.local/state/lade/backups/`, the last three kept). If you roll back past such a change, the installer explains
  what will happen, asks you to type `restore` (or accepts `--yes`), stops the service and puts the backup back:
  **work recorded after the upgrade is not in it**, but the newer database is kept next to the backups, never
  deleted, and rolling forward again works. Without a matching backup the rollback is refused and nothing changes.
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
  window shows the same state. A feedback draft you were still typing is saved first (the window waits up to 2 s
  for the service to confirm). If a draft cannot be saved — the save failed, it conflicts, or the service is not
  reachable — the window asks: *Keep window open* (the default; retry beside the draft) or *Close and discard
  unsaved text* (the draft keeps the last revision the service saved). Exit asks the same way.
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
  your desktop has no notification service, nothing is lost: one quiet line under the Objectives list says
  *Desktop notifications unavailable* (its tooltip gives the reason).

Keyboard: F6 / Shift+F6 move between regions, Ctrl+1 Workbench, Ctrl+2 Activity, Ctrl+3 Terminal (and focus it),
Ctrl+Shift+E leave the terminal, Ctrl+K search, Ctrl+N new Objective, Ctrl+O open repository, Ctrl+Shift+M add
feedback on the selection, Ctrl+Shift+F switch diff/full file, Ctrl+Enter send a message, Ctrl+Q Exit, Escape
closes a dialog, Ctrl+? (or the keyboard button in the header) lists every shortcut. In that dialog each action except F6 / Shift+F6 and Escape can be given a function key F1–F35, optionally
with Shift, Control or Alt (other combinations stay with text fields for typing, editing and the clipboard); Apply
checks the whole set, including duplicates, before using it and saves it to
`$XDG_CONFIG_HOME/lade/keybindings.json` (default `~/.config/lade/keybindings.json`). An invalid file is ignored with
a notice and the defaults apply; delete the file to return to them.

**Where things are.** Each piece of information and each action has one place.
- **Objectives list.** Objectives are grouped by attention. A row adds one line only for what its group heading does not say: the attention reasons (`! check failed · unsent drafts`) or a closed lifecycle.
- **Workbench.** It shows the title, the collaboration journey and the Change Set. Its version line says only what the version selector does not: `Captured · may change · vs main@…` or `Frozen · vs main@…`.
- **Overview** (right panel):
  - the approval state;
  - the **one next review step** the service currently allows, such as *Freeze for review*, *Approve Candidate N*, *Integrate into main* or, once the Objective is closed, *Clean up branch and worktree*, with the reason in one line when it is blocked;
  - **More actions**: every other review decision (Return for revision, Refresh, Complete without changes and, last and separated, Discard, Clean up and Delete Git history), each with its reason when unavailable;
  - the intent, with *Edit intent*;
  - **Details**: lifecycle, last update, branch and start commit.
- **Activity** is the Agent Session's place:
  - its state line: one sentence on what the agent is doing (the session and generation are in its tooltip);
  - before the Objective's first start, **Set up the agent** in place of the transcript: provider, model, thinking
    and **Start agent** (the LADE profile resources collapsed below);
  - once it has an Agent Session, the one agent action that applies now beside the title — **Cancel agent** while
    it runs, **Resume agent** once it stopped — and **Agent settings** (collapsed): the model and thinking, the
    session tools and the LADE profile resources;
  - **Accepted inputs and delivery**. An uncertain or blocked delivery is stated above it;
  - the transcript and, with an Agent Session, the composer.

When the Objective waits for the agent to be started or resumed, the Overview says so in one line, with **Open Activity**.

When the service refuses an action, the reason is shown next to the control you used (for example under *Start
agent*, under the composer, or beside *Run check*), with the error code under **Details**; it goes away once that
action succeeds or the Objective changes.

Below about 1100 pixels of window width (for example 800 × 550, or a 1600 × 1100 screen at 200 % scaling) the
window shows one region at a time: switch between **Objectives**, **Work area** and **Overview · Evidence ·
Feedback** with the buttons below the header or F6. In this layout the work area also shows the Objective's status,
and **Objective details** shows or hides the journey.
Below 600 pixels the header navigation uses a second row, **Review** opens Overview / Evidence / Feedback,
and the file list sits above the inspected file. Dialog content scrolls to keep its action buttons reachable.
Text you type or paste into drafts, messages and intents is limited to 256 KiB per field (an override reason to
2,048 characters); a paste that would exceed it is not inserted and your text stays as it was.

## 3. Open a repository (Workspace)

Ctrl+O, paste the full repository folder path and choose **Open repository** (or press Enter). **Browse…** opens
the native folder chooser and returns the selected path for confirmation. Choose the **top-level directory**
containing `.git`, not the `.git` directory itself; the repository needs at least one commit.
LADE never runs the repository's hooks
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

**Costs and data**, on the initial screen and in **Set up the agent**, explains the provider boundary before a
first run. Live models use your provider account and its billing, limits, terms and privacy policy. LADE does not
estimate or cap provider charges. Instructions, repository content the agent reads and tool results are sent to
the selected provider; Objectives, messages, drafts and captured changes are stored locally. An existing pi
ChatGPT login cannot be reused by this build; the subscription-login restriction below still applies.

Pick a model when starting execution: in Activity, **Set up the agent** shows the provider, model and thinking
level *Start agent* will use (the first usable ones are chosen for you). Once the agent has run, change them under
**Agent settings** and apply them with *Apply model and thinking*.

Most providers need an API key. The recommended place is your **keyring** (GNOME Keyring, through the Secret
Service; needs `sudo apt install libsecret-tools`):

```sh
laded credentials set openai            # asks for OPENAI_API_KEY without echoing it
printf '%s\n' "$KEY" | laded credentials set groq   # or pipe it in
laded credentials set anthropic --variable ANTHROPIC_OAUTH_TOKEN   # a provider's other variable
laded credentials clear openai          # remove LADE's keyring items for a provider
```

The key travels only on standard input, never as an argument. If the keyring cannot be used (no `secret-tool`, no
desktop session bus, a locked keyring), the command says why and exits without storing anything; use the file below
instead. A custom provider's key is stored with `--variable LADE_PROVIDER_<NAME>`.

Alternatively, put the key in `~/.config/lade/credentials.env`, which must be a regular file owned by you with mode
0600 (`chmod 600 ~/.config/lade/credentials.env`); LADE refuses it otherwise. Format: `NAME=value` lines, `#`
comments, optional `export `.

For each variable LADE uses the first of: `laded`'s environment, the keyring, `credentials.env`. They are read at
each start of execution, and only the selected provider's variables reach the sandbox. The keyring is read only
when execution starts: with a usable keyring the model picker shows a provider whose key is in neither of the other
places as available, noting that LADE looks for its key in your keyring when the agent starts (a missing key is
then the start's refusal). Without a usable keyring such a provider *needs credentials*: the picker says what to
add (where it can go is under *Details*), *Start agent* stays disabled until you add it and press *Check again*,
and you can choose another provider meanwhile. LADE never copies a key from the keyring into a file.

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

Subscription logins (OAuth) and their token refresh, AWS Bedrock and Vertex are not available; a subscription
token that the provider accepts as a plain variable (`ANTHROPIC_OAUTH_TOKEN`, `KIMI_API_KEY`) works like a key and
is never refreshed by LADE. The agent's tools can read the key inside the sandbox (the keyring protects it at rest,
not from the agent), and the agent can reach only the provider's API host (HTTPS port 443) through LADE's gateway;
a proxy in `HTTPS_PROXY` of the service is honoured. LADE stores a key only when you run `laded credentials set`,
and only in your keyring.

## 6. Run, watch and steer

**Start** runs a pi Agent Session in a bubblewrap sandbox that can write only the Objective's worktree and its
session directory. If the sandbox cannot be created, execution is refused — LADE never runs an agent unsandboxed.

Limits: the worktree plus the session directory may use 2 GiB of disk; LADE checks every few seconds and, above
that, stops the agent with **limit reached** (free space, then **Resume**; the limit is never raised by itself).
Under the systemd unit each Objective's agent, terminals and checks also share 4 GiB of memory and 512
processes; started by hand, LADE logs `cgroup limits unavailable (…)` and uses per-process limits only.

- **Activity** (Ctrl+2) shows the transcript: assistant text, thinking, tool calls and results, system notes.
- The composer sends a **prompt**, a **steer** (delivered after the current tool calls) or a **follow-up**
  (delivered when the agent would stop). Every message — and every submitted feedback batch — is durably accepted
  before it is delivered; its state (queued, dispatched, received, uncertain, blocked) is listed under **Accepted
  inputs and delivery**, and an uncertain or blocked one is stated above that list. An *uncertain* message is never
  resent automatically.
- **Start agent** is in the *Set up the agent* card until the first start; it is disabled, with the reason in its
  tooltip and the state line, while the chosen provider needs credentials or is restricted. Afterwards the top of Activity shows only
  the agent action that applies: **Cancel agent** (stops the agent and keeps its work and session) while it runs,
  **Resume agent** (continues the same session) once it stopped. An unavailable one names its reason in its
  tooltip, and the state line next to it says why.
- **Agent settings** (collapsed, at the top of Activity once the agent has a session) holds the model and
  thinking pickers, the Agent Session tools — *Show session tree* (go to, fork or clone an entry), *Apply model and
  thinking*, *Compact context*, *Reload session resources*, *Export session*, *Import session…* — and the LADE
  profile resources with the approval of executable extensions.

### Terminal

**Terminal** (Ctrl+3) opens a shell for the selected Objective — for running its tests, `git status`, `ls` and the
like next to the agent. It is not a general shell: it runs inside the same sandbox as the agent, in the Objective's
worktree only, with the same limits, **no network** and **no API keys**; the repository's `.git` is read-only, so
commits and branch changes are LADE's (freeze and integrate). The prompt starts with `[lade]`.

- While the Objective is in progress the terminal can change the worktree, and those changes show up in the Change
  Set like the agent's. **Freeze for review** first closes every such terminal. While a candidate is frozen or
  approved, a terminal opens **read-only** (`[lade read-only]`) for inspection.
- An Objective needs a worktree first: start its agent once (the Terminal says so, with **Open Activity**). Whether
  a terminal can open, and why not, comes from the service.
- Keys go to the shell (Tab, Ctrl+C, Ctrl+D, arrows…). Leave the terminal with **Ctrl+Shift+E** (focus returns to
  where it was) or F6; come back with Ctrl+3. Ctrl+Shift+C / Ctrl+Shift+V copy and paste (pastes up to 256 KiB).
  All of these can be remapped in the shortcuts dialog (function keys).
- The terminal is plain text (no colours); full-screen programs work in a basic way. Very fast output is slowed down
  and, if it still cannot keep up, shortened with a visible `[… N bytes of output skipped …]` marker.
- Closing the window, losing the connection, Exit or logout ends the terminal and everything started in it; it is
  never reopened automatically.

## 7. Inspect, give feedback, review and integrate

1. **Inspect.** The Workbench lists the changed files of the Change Set with a diff or full-file view. Each view
   names its version: the selector (*Working inspection*, *Candidate N (current)*, …) and one line with what it
   does not say — *Captured · may change · vs main@…* or *Frozen · vs main@…* — with the exact capture and target
   commit in its tooltip. Ctrl+K searches Objectives, paths or content;
   opening a content result shows the full file with the first match selected, ready for *Add feedback*. Files
   larger than 64 MiB (up to 1 GiB) open in chunks of up to 256 KiB — *Load more* continues; larger files cannot be
   inspected. Syntax is highlighted for languages GtkSourceView recognises by file name; text over 2 MiB or 40,000
   lines is shown plain (the viewer says so) to stay responsive.
2. **Feedback.** Select text in a diff or file and press Ctrl+Shift+M (or *Add feedback*). Drafts are stored by the
   service (each shows whether its latest edit is saved: *Saved rN* appears only after the service confirmed that
   revision; a refused save shows *Save failed* with *Retry*) and survive closing the window. **Submit** sends all selected drafts together to the identified
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
- **Delete Git history** (under *More actions*, never suggested as the next step) — only after Clean up, and only
  when you confirm the dialog: deletes the Objective's retention
  refs (and its `lade/*` branch if one is left), so Git may later garbage-collect its captures and candidates; they can
  then no longer be inspected. Integrated commits, the Objective record, accepted inputs, review evidence and stored
  artifacts stay. LADE never does this on its own.

## 9. Where data lives

| What | Where |
| --- | --- |
| Database, artifacts, Objective worktrees, Agent Session files, verification copies | `~/.local/state/lade/` (`$XDG_STATE_HOME/lade`) |
| Control socket and capability (private, rotated at every start) | `$XDG_RUNTIME_DIR/lade/` |
| Credentials (optional, yours) | your keyring (items `service=lade provider=…`, see `laded credentials`) or `~/.config/lade/credentials.env` |
| Service diagnostics | `~/.local/state/lade/logs/` (at most 4 × 1 MiB, redacted) and the user journal: `journalctl --user -u lade.service` (codes and ids, no prompts, source or keys) |
| Pre-migration backups | `~/.local/state/lade/backups/` (the last three, plus databases set aside by a rollback) |
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
  (`Delegate=memory pids cpu`) on a cgroup v2 system; LADE asks the systemd user manager whether its unit is
  delegated (`systemctl --user show -p Delegate lade.service` must say `yes`) and checks that the limits take
  effect. The reason names what is missing. LADE keeps working with per-process limits; `LADE_CGROUPS=off` disables
  the cgroup limits explicitly.
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
  owner/mode, or the provider is not supported. The message names the missing variables, never their values, and
  says why the keyring could not be asked when it could not ("Secret Service unavailable: …"). A **locked keyring**
  is reported as such: unlock it (for example in *Passwords and Keys*) and start again. Under the systemd unit the
  keyring needs the desktop session's bus (`DBUS_SESSION_BUS_ADDRESS`, normally provided by the user manager).
- **Sending diagnostics.** `laded diagnostics export ~/lade-diagnostics.txt` writes the recent service logs and the
  migration journal, redacted (keys, tokens and your home directory are masked), to a new private file. Nothing
  leaves your machine by itself; read the file before you share it — redaction is careful but not infallible.
- **Rollback refused: "no pre-migration backup".** The newer version migrated the database and no backup of the
  older format exists (for example it was pruned after several upgrades). Stay on the newer version, or restore a
  copy of `~/.local/state/lade` you made yourself.
- **Integration refused.** The checked-out target branch has uncommitted changes or a merge/rebase in progress, or
  the target moved since approval (refresh, verify and approve again).
