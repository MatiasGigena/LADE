# Build and test

Build and verification instructions for the service, native workbench and pi worker. Read the [wire contract](protocol-v1.md), [service admission](service.md#admission-transport) and [remaining release gates](limitations.md) before extending them.

## Supported development inputs

`contracts/toolchain.json`, `rust-toolchain.toml`, `.node-version`, `Cargo.lock` and `packages/pi-worker/package-lock.json` fix the tested input set. Mint 22.3/Cinnamon/X11/x86_64, GTK 4.14.5/GLib 2.80.0, Rust 1.98.1, Node 22.23.1/npm 10.9.8. gtk4-rs is 0.11.4; sourceview5 is 0.11.2 (features `v5_12`, `gtk_v4_14`) for GtkSourceView 5.12, the reviewed read-only diff and file viewer of the desktop client — chosen over a custom text widget because it supplies custom gutters, paragraph fills and native keyboard selection without subclassing (the crate forbids `unsafe`). The native Debian package revisions are recorded, not automatically installed/downgraded. A baseline mismatch needs review, not disabling host security or downgrading a security update.

Prerequisites: Node/npm at those versions, Python 3.11+, curl, `sha256sum`, C compiler/linker, pkg-config and GTK4 development headers. On Mint, native package installation is an explicit host-setup action; these scripts never run sudo or change services. The CI runner installs its own disposable Ubuntu prerequisites and does **not** certify the supported Mint environment.

Explicit one-time local toolchain setup (downloads checksum-pinned rustup 1.29.1 and installs the exact Rust toolchain into ignored `.tools/`; does not edit shell profiles):

```sh
./scripts/bootstrap-rust
npm --prefix packages/pi-worker ci --ignore-scripts --no-audit --no-fund
git config core.hooksPath .githooks
python3 scripts/check-pins.py
```

The tracked pre-commit hook runs only the worker's anti-slop lint command against the **working directory**, not an exact snapshot of staging. Partially staged changes can therefore differ from what the hook checks; CI verifies the published content with `scripts/verify`. Hook activation is an explicit per-clone action; package installation does not modify Git configuration. The hook does not stash, stage, autofix, install dependencies or require the whole test suite. No package-install scripts, pi loading, providers or repository tools run during these steps. Build scripts/procedural macros of the pinned Rust dependencies are trusted build inputs, not sandboxed worker execution.

## Verification

```sh
./scripts/verify
```

This runs locked workspace build/tests, formatting and Clippy, crate/source architecture checks, TypeScript linting/checking/tests, byte-fragmented common fixtures and cross-language comparison, and executable version/fail-closed smoke checks. Individual commands:

```sh
./scripts/cargo build --workspace --locked
./scripts/cargo test --workspace --locked
./scripts/cargo test -p lade-core --locked
./scripts/cargo clippy --workspace --all-targets --locked -- -D warnings
npm --prefix packages/pi-worker run lint
npm --prefix packages/pi-worker run check
npm --prefix packages/pi-worker test
python3 scripts/check-architecture.py
python3 scripts/check-contrast.py
python3 scripts/compare-fixtures.py
```

`scripts/check-contrast.py` (part of `scripts/verify`, no display needed) reads the desktop stylesheet
`crates/lade-desktop/src/assets/style.css`: it fails on colour literals outside `@define-color` tokens and on viewer
colours (set from Rust) that are not tokens, and prints the WCAG ratio of each specified ink/fill pair composited on the
black surface (4.5:1 text, 3:1 control boundaries, focus outline and markers). It is a token computation, not a
rendered-surface measurement. To sample a real native capture (optional, needs Pillow):

```sh
python3 scripts/sample-rendered-contrast.py capture.png X,Y,W,H [X,Y,W,H ...]
```

Each region should contain one text run or indicator on one fill; the script reports the dominant fill, the strongest
ink covering at least 2 % of the region and their ratio. Neither script replaces an Orca or human visual review.

Domain-only tests compile no GTK/SQLite. The fixture corpus is `contracts/v1/fixtures.json`; large stress inputs are generated from bounded recipes. Schemas, DTOs and corpus are reproducible using `scripts/generate-schema.py` (both `contracts/v1/schema.json` and `worker-schema.json`), `scripts/generate-dtos.py` and `scripts/generate-fixtures.py`; `--check` rejects drift. The JSON Schemas use documented custom formats (`uuid-v4`, `u64`, `sha256`, `chunk-hex`, `git-sha1`, `capability`, `timestamp`, `path-b64`, `page-limit`, `read-limit`; see [protocol-v1.md](protocol-v1.md#formats)); a generic JSON-Schema implementation must install those formats rather than assume they are annotations. Protocol validators intentionally support only this closed subset.

### Boundary-validator lint

`anti-slop/no-runtime-typeof` remains an error. Only `src/schema.ts` has an override with `["error", { "allowInTypeGuards": true }]`, supported by pinned Oxlint/@oxlint/plugins 1.83.0 and the unchanged vendored rule. Even there, runtime checks outside a type-predicate function still reject (the plugin's existing `typeof x === "undefined"` existence-probe exemption remains unchanged). Other files retain the default prohibition, including guards. External JSON values are checked at the boundary without coercion; internal code consumes the validated DTOs. No `unknown` rule exception is needed: `JsonValue` honestly represents the output of the bounded parser before schema validation.

The pi worker's closed §4.2 validator `src/worker/wire.ts` is a second external-input boundary with the same shape (bounded parser output checked by type-predicate guards), so it shares exactly that override; every other worker file (`src/worker.ts`, `src/worker/*.ts`) keeps the default prohibition. `tsconfig.json` sets `skipLibCheck: true` because the published pi 1.5.0 declarations import JSON without `type: "json"` attributes and fail under `module: NodeNext`; our own sources are still checked in full strict mode. See [worker.md](worker.md).

`python3 scripts/check-worker-lint.py` exercises the real installed CLI/config on small disposable examples: allowed boundary guards, rejected non-guards/nested callbacks, guards outside the override, and other enabled rules. It runs in `scripts/verify`/CI, not in the lightweight commit hook, and never modifies production files or installs packages.

## Executables

```sh
./scripts/cargo run --locked -p lade-desktop
./target/debug/laded --version
./target/debug/laded serve                     # $XDG_STATE_HOME/lade, $XDG_RUNTIME_DIR/lade
./target/debug/laded serve --state-dir /abs/state --runtime-dir /abs/run
./target/debug/laded  # exits 64: usage
./scripts/cargo test -p lade-system --test service_admission --test service_operations --test service_flow
```

`laded serve` runs in the foreground until killed (redacted diagnostics on stderr and in the bounded `$STATE/logs`, see [service.md](service.md#diagnostics)) and refuses to start (exit 78) on an unsafe runtime directory, an unopenable/corrupt/newer database or a second instance on the same state directory. `scripts/verify` checks the usage exit and that an unsafe (0777) runtime directory is refused before any socket exists. The service, its storage, admission and recovery are described in [service.md](service.md). The service tests use disposable repositories and state; the full-loop test runs the real pi worker with the `lade-fixture` test model (a test-only provider: the harnesses enable it with `LADE_TEST_FIXTURE_PROVIDER=1`, see [service.md](service.md#test-only-fixture-provider)) through the real bubblewrap launcher when bubblewrap works on the host (otherwise an unsandboxed **test-only** launcher), including sandboxed verification.

Tests that need the sandbox (bubblewrap, prlimit and working user namespaces; the worker launcher tests also need
node) print a `skipping:` note and pass when it is missing. With `LADE_REQUIRE_SANDBOX=1` they fail instead, and the
tests that choose between the bubblewrap launcher and the unsandboxed test launcher refuse the fallback; CI sets it for
`scripts/verify`, so a CI run never passes with the sandbox tests silently skipped. Some sandbox tests assume a
non-root user (root is exempt from `RLIMIT_NPROC`).

IMP-013 adds `service_concurrency.rs` and `service_crash.rs` (part of `scripts/verify`; the crash tests run the real
`laded` binary with `LADE_FAULT_POINT`, a debug-build-only hook, and need bubblewrap) and the long
procedures, which are **not** part of `scripts/verify`:

```sh
scripts/load [OUT_DIR]          # canonical 1/4-worker load, launches, GTK client phases (~1 h; release build)
scripts/load --soak [OUT_DIR]   # 5 min warmup + 30 min soak (LADE_SOAK_SECS / LADE_SOAK_WARMUP_SECS)
```

`lade-desktop` is the live GTK client: it connects to `laded` over `$RUNTIME/control.sock` (u32-BE frames, `hello` + capability `auth`, single controlling lease), starts a detached sibling `laded serve` when none is running (unless `--no-spawn`), and offers the whole first-release loop — open a Workspace (`--open PATH` or Ctrl+O), create and start Objectives, Activity, Workbench inspection with Contextual Feedback, freeze, service-observed checks, approval, protected integration and cleanup. Every action's availability comes from the service's `available_actions`; the client never grants authority. Keyboard shortcut remaps are a client
presentation preference in `$XDG_CONFIG_HOME/lade/keybindings.json` (else `~/.config/lade/keybindings.json`): JSON
`{"version": 1, "bindings": {"focus-search": "F8", …}}` with action ids from `DEFAULT_APP_KEYBINDINGS`; the whole
file is validated with the remapping policy on load and ignored (defaults, plus a visible notice) when invalid. The worker package is found through `LADE_WORKER_DIR`, else the bundle layout `bin/../lib/lade/pi-worker`, else (debug builds only) this repository's `packages/pi-worker`; `LADE_NODE` selects the node runtime (default: `node` on `PATH`). Installation, the bundle and the systemd user unit are described below and in `packaging/`.

## Native capture

After a locked build on X11, capture **only the application's XID**, never the whole desktop:

```sh
umask 077
mkdir -p evidence-local
./target/debug/lade-desktop >evidence-local/desktop.stdout 2>evidence-local/desktop.stderr &
pid=$!
window=$(timeout 15 xdotool search --sync --onlyvisible --pid "$pid" --name '^LADE$' | head -1)
xprop -id "$window" _NET_WM_PID WM_CLASS WM_NAME
xwd -silent -id "$window" -out evidence-local/build-shell.xwd
ffmpeg -loglevel error -y -i evidence-local/build-shell.xwd -frames:v 1 evidence-local/build-shell.png
kill "$pid"
```

Inspect the resulting PNG; do not infer native rendering from source/CSS. Capture helpers are optional developer tools, not application dependencies. IMP-002 still owns workbench appearance, keyboard/Orca/200% evidence. No browser is used.

Scripted native runs under Xvfb (development host): `scripts/e2e-gui` drives the whole review loop and
`scripts/e2e-terminal` the IMP-012 terminal, single-instance, client-close, Exit and restart flows (it runs inside its
own `dbus-run-session`). Both need a debug build, Xvfb, xdotool, ImageMagick and bubblewrap and are not part of
`scripts/verify`. The `#[ignore]`d `lade-desktop` display tests (launch QA layout checks and the fixture-view
capture, which writes PNGs to `LADE_CAPTURE_DIR` and does nothing without it) are not in `scripts/verify` either; CI
runs them after it with `xvfb-run -a ./scripts/cargo test -p lade-desktop --locked -- --ignored`. Native, resilience, performance and load scripts write captures and reports under the ignored
`evidence-local/` directory by default; pass an output directory to retain them elsewhere.

The contained terminal (IMP-012) adds no dependency: the PTY comes from rustix (`pty`, `termios` features of the
already-reviewed `rustix =1.1.4`) and the client renders it with its own minimal VT parser
(`presentation/terminal.rs`). VTE (`vte4`) was not adopted: it is not in the CI image or the documented host
prerequisites, its Rust bindings would add a native dependency chain for a supporting feature, and its full
terminal emulation (hyperlinks, clipboard escape sequences, its own key handling) would bypass the reviewed keymap
escape routes.

## Pi preservation and next gate

The first-release SDK worker (`node packages/pi-worker/src/worker.ts`) installs the unpatched 1.5.0 npm distribution recorded under `npm_distribution` in the manifest; see [worker.md](worker.md). `vendor/pi-coding-agent/manifest.json` retains the pi 1.5.0 source commit (tag `v1.5.0`), archive digest, the coding-agent `package.json`, upstream's release install lock (`upstream/install-lock/`, which replaces the shrinkwrap pi published before 1.0), the MIT notice and the currently empty patch inventory. With an empty patch inventory the narrow fork's runnable form is that registry tarball: `packages/pi-worker/test/worker-deps.test.ts` checks that the installed `package.json` is the pinned upstream bytes, that the lockfile resolves the SDK's dependency closure to exactly the upstream install lock (versions, resolved URLs and integrity), and that the worker loads only the SDK's own pi-ai. Not done: a reproducible build of the coding-agent from the pinned source archive (needs the archive, upstream's build tooling and a byte comparison against the registry tarball); it becomes necessary once a patch is recorded. Provider conformance is IMP-011. Independent wire/security sign-off on admission is still required to accept IMP-001; no codec test proves a worker cannot reach host control material.

### Worker dependencies and install scripts

The worker's direct production dependencies are `@earendil-works/pi-coding-agent` and `undici`; pi-ai is used only through the single hoisted copy the SDK resolves ([worker.md](worker.md#dependencies)). Every install, including the bundle's, uses `--ignore-scripts`. npm runs `prepare` only for git and directory dependencies, never for registry tarballs, so only `preinstall`/`install`/`postinstall` (and an implicit `node-gyp rebuild` for a `binding.gyp`, which no locked package has) would run. Of the 122 production packages, two declare one (`hasInstallScript` in `packages/pi-worker/package-lock.json`):

| Package | Skipped script | Why the worker does not need it |
| --- | --- | --- |
| `@google/genai` 2.21.0 | `preinstall`: `echo 'preinstall: no-op'` | Does nothing. |
| `protobufjs` 7.6.6 | `postinstall`: `node scripts/postinstall` | Only prints a warning when a dependent's version range uses another version scheme; writes nothing. |

Native code that does ship needs no script: `@silvia-odwyer/photon-node` and `quickjs-wasi` are WebAssembly, and pi-tui ships prebuilt `.node` helpers (for Linux an X11 clipboard helper, loaded only on demand by the interactive TUI's clipboard calls, which the worker never makes). Re-check this table on any dependency change.

## Install

`scripts/bundle [--with-node DIR] [--sign KEY]` builds the release binaries and assembles the checksummed bundle
`dist/lade-0.2.0/`; `scripts/install`, `scripts/install --rollback` and `scripts/uninstall` manage a per-user
installation; and `scripts/test-install [--bundle DIR [--allowed-signers FILE]]` exercises them in a disposable HOME
(the synthetic-bundle run is part of `scripts/verify`). Layout, prerequisites and the systemd user unit
(`packaging/`) are described in the [user guide](../user-guide.md#1-install); open gates are in
[limitations.md](limitations.md#installation-and-distribution).

- **Signing**: `scripts/sign-bundle DIR KEY` (called by `bundle --sign`) sets `signed=yes` and `signer=SHA256:…` in
  `VERSION`, regenerates `MANIFEST.sha256` and writes `MANIFEST.sha256.sig` with `ssh-keygen -Y sign -n
  lade-release` (OpenSSH ≥ 8.2; Mint's `openssh-client`). The installer verifies it with `ssh-keygen -Y
  find-principals` / `-Y verify` against an allowed-signers trust root. No key is shipped: the release key, its
  custody and the trust bootstrap (how users obtain and pin the public key) are pending the maintainer's decision. Tests use
  throwaway keys generated in `scripts/test-install`; CI generates one per run and signs the real bundle with it.
- **Node runtime**: `--with-node DIR` copies `DIR/bin/node` (must report exactly the pinned version), its `LICENSE`
  and a `VERSION` into `lib/lade/node/`; the bundle smoke test then runs the worker with it. Size impact: the Node
  22.23.1 binary is ≈ 120 MB (bundle ≈ 177 MB on disk, 13.9k files → ≈ 297 MB; it was ≈ 275 MB and 24.8k files before the duplicate pi-ai was dropped). In CI:
  `--with-node "$(dirname "$(dirname "$(readlink -f "$(command -v node)")")")"` (the `setup-node` toolcache
  distribution); locally any official `node-v22.23.1-linux-x64` directory.
- **Licence inventory**: `python3 scripts/license-inventory.py --check` (in `scripts/verify`) walks `cargo metadata
  --offline --locked` from `lade-system` and `lade-desktop` over normal dependencies for x86_64 Linux (proc-macro and
  build-only crates are not shipped) and the production entries of the worker's `package-lock.json`, and fails when
  any of them has neither a licence expression nor a licence file. `--bundle DIR` (run by `scripts/bundle`) writes
  `share/doc/lade/THIRD_PARTY.{json,md}` from what is actually in the bundle (including the bundled Node) and copies
  every Rust crate's licence files to `share/doc/lade/LICENSES/rust/`; npm packages keep their own licence files in
  `node_modules`.
- **Schema**: `VERSION` records `schema=` (from `laded store info`), which `install --rollback` compares with the
  database to decide on a pre-migration backup restore ([service.md](service.md#migration-safety-imp-014)).
- `scripts/verify` needs `ssh-keygen` for the signature cases of `scripts/test-install` (skipped with a note when
  absent) and `systemd-analyze` for the unit check (likewise).
