# Build and test IMP-001

The implemented boundary is [IMP-001 / #22](https://github.com/MatiasGigena/LADE/issues/22), not a running Agent Session or complete workbench. Read the [wire/admission contract](imp-001-contract.md) and [evidence](evidence/imp-001/README.md) before extending it.

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
white surface (4.5:1 text, 3:1 control boundaries, focus outline and markers). It is a token computation, not a
rendered-surface measurement. To sample a real native capture (optional, needs Pillow):

```sh
python3 scripts/sample-rendered-contrast.py capture.png X,Y,W,H [X,Y,W,H ...]
```

Each region should contain one text run or indicator on one fill; the script reports the dominant fill, the strongest
ink covering at least 2 % of the region and their ratio. Neither script replaces an Orca or human visual review.

Domain-only tests compile no GTK/SQLite. The fixture corpus is `contracts/v1/fixtures.json`; large stress inputs are generated from bounded recipes. Schemas, DTOs and corpus are reproducible using `scripts/generate-schema.py` (both `contracts/v1/schema.json` and `worker-schema.json`), `scripts/generate-dtos.py` and `scripts/generate-fixtures.py`; `--check` rejects drift. The JSON Schemas use documented custom formats (`uuid-v4`, `u64`, `sha256`, `chunk-hex`, `git-sha1`, `capability`, `timestamp`, `path-b64`, `page-limit`, `read-limit`; see [protocol-v1.md](protocol-v1.md#formats)); a generic JSON-Schema implementation must install those formats rather than assume they are annotations. Protocol validators intentionally support only this closed subset.

### Boundary-validator lint

`anti-slop/no-runtime-typeof` remains an error. Only `src/protocol.ts` has an override with `["error", { "allowInTypeGuards": true }]`, supported by pinned Oxlint/@oxlint/plugins 1.83.0 and the unchanged vendored rule. Even there, runtime checks outside a type-predicate function still reject (the plugin's existing `typeof x === "undefined"` existence-probe exemption remains unchanged). Other files retain the default prohibition, including guards. External JSON values are checked at the boundary without coercion; internal code consumes the validated DTOs. No `unknown` rule exception is needed: `JsonValue` honestly represents the output of the bounded parser before schema validation.

The pi worker's closed §4.2 validator `src/worker/wire.ts` is a second external-input boundary with the same shape (bounded parser output checked by type-predicate guards), so it shares exactly that override; every other worker file (`src/worker.ts`, `src/worker/*.ts`) keeps the default prohibition. `tsconfig.json` sets `skipLibCheck: true` because the published pi 0.85.1 declarations import JSON without `type: "json"` attributes and fail under `module: NodeNext`; our own sources are still checked in full strict mode. See [worker.md](worker.md).

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

`laded serve` runs in the foreground until killed (stderr diagnostics only) and refuses to start (exit 78) on an unsafe runtime directory, an unopenable/corrupt/newer database or a second instance on the same state directory. `scripts/verify` checks the usage exit and that an unsafe (0777) runtime directory is refused before any socket exists. The service, its storage, admission and recovery are described in [service.md](service.md). The service tests use disposable repositories and state; the full-loop test runs the real pi worker with the `lade-fixture` model through the real bubblewrap launcher when bubblewrap works on the host (otherwise an unsandboxed **test-only** launcher), including sandboxed verification.

`lade-desktop` is the live GTK client: it connects to `laded` over `$RUNTIME/control.sock` (u32-BE frames, `hello` + capability `auth`, single controlling lease), starts a detached sibling `laded serve` when none is running (unless `--no-spawn`), and offers the whole first-release loop — open a Workspace (`--open PATH` or Ctrl+O), create and start Objectives, Activity, Workbench inspection with Contextual Feedback, freeze, service-observed checks, approval, protected integration and cleanup. Every action's availability comes from the service's `available_actions`; the client never grants authority. `lade-desktop --preview` shows labelled fixture data without a service. Keyboard shortcut remaps are a client
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

## Pi preservation and next gate

The first-release SDK worker (`node packages/pi-worker/src/worker.ts`, delivery plan WP-D) installs the unpatched 0.85.1 npm distribution recorded under `npm_distribution` in the manifest; see [worker.md](worker.md). `vendor/pi-coding-agent/manifest.json` retains the approved pi 0.85.1 source commit, archive digest, shrinkwrap, MIT notice and currently empty patch inventory. This is provenance, not a runnable SDK fork or provider conformance. IMP-008 materializes the fork and adapter after safety gates. Independent wire/security sign-off on admission is still required to accept IMP-001; no codec test proves a worker cannot reach host control material.

## Install

`scripts/bundle` builds the release binaries and assembles the checksummed (unsigned) bundle `dist/lade-0.2.0/`;
`scripts/install`, `scripts/install --rollback` and `scripts/uninstall` manage a per-user installation; and
`scripts/test-install [--bundle DIR]` exercises them in a disposable HOME (the synthetic-bundle run is part of
`scripts/verify`). Layout, prerequisites and the systemd user unit (`packaging/`) are described in the
[user guide](../user-guide.md#1-install); open gates are in [limitations.md](limitations.md#installation-and-distribution).
