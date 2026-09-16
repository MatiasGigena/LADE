# Build and test IMP-001

The implemented boundary is [IMP-001 / #22](https://github.com/MatiasGigena/LADE/issues/22), not a running Agent Session or complete workbench. Read the [wire/admission contract](imp-001-contract.md) and [evidence](evidence/imp-001/README.md) before extending it.

## Supported development inputs

`contracts/toolchain.json`, `rust-toolchain.toml`, `.node-version`, `Cargo.lock` and `packages/pi-worker/package-lock.json` fix the tested input set. Mint 22.3/Cinnamon/X11/x86_64, GTK 4.14.5/GLib 2.80.0, Rust 1.98.1, Node 22.23.1/npm 10.9.8. gtk4-rs is 0.11.4. The native Debian package revisions are recorded, not automatically installed/downgraded. A baseline mismatch needs review, not disabling host security or downgrading a security update.

Prerequisites: Node/npm at those versions, Python 3.11+, curl, `sha256sum`, C compiler/linker, pkg-config and GTK4 development headers. On Mint, native package installation is an explicit host-setup action; these scripts never run sudo or change services. The CI runner installs its own disposable Ubuntu prerequisites and does **not** certify the supported Mint environment.

Explicit one-time local toolchain setup (downloads checksum-pinned rustup 1.29.1 and installs the exact Rust toolchain into ignored `.tools/`; does not edit shell profiles):

```sh
./scripts/bootstrap-rust
npm --prefix packages/pi-worker ci --ignore-scripts --no-audit --no-fund
git config core.hooksPath .githooks
python3 scripts/check-pins.py
```

The tracked pre-commit hook runs the worker's anti-slop lint command. Hook activation is an explicit per-clone action; package installation does not modify Git configuration. No package-install scripts, pi loading, providers or repository tools run during these steps. Build scripts/procedural macros of the pinned Rust dependencies are trusted build inputs, not sandboxed worker execution.

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
python3 scripts/compare-fixtures.py
```

Domain-only tests compile no GTK/SQLite. The fixture corpus is `contracts/v1/fixtures.json`; large stress inputs are generated from bounded recipes. DTOs and corpus are reproducible using `scripts/generate-dtos.py` and `scripts/generate-fixtures.py`; `--check` rejects drift. The JSON Schema uses documented custom formats (`uuid-v4`, `u64`, `sha256`, `chunk-hex`); a generic JSON-Schema implementation must install those formats rather than assume they are annotations. Protocol validators intentionally support only this closed subset.

## Executables

```sh
./scripts/cargo run --locked -p lade-desktop
./target/debug/laded --version
./target/debug/laded  # exits 78: service admission/persistence not implemented
```

The desktop presents only a native build-shell status. It does not connect to a service, grant authority, access a Workspace, or simulate approvals. The actual approved visual component remains IMP-002. No systemd unit/socket, database, logging daemon, provider credential or worker entrypoint is installed in IMP-001.

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

`vendor/pi-coding-agent/manifest.json` retains the approved pi 0.85.1 source commit, archive digest, shrinkwrap, MIT notice and currently empty patch inventory. This is provenance, not a runnable SDK fork or provider conformance. IMP-008 materializes the fork and adapter after safety gates. Independent wire/security sign-off on admission is still required to accept IMP-001; no codec test proves a worker cannot reach host control material.
