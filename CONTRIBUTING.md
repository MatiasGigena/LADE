# Contributing to LADE

Thanks for your interest. LADE is pre-release; read the [README](README.md), [CONTEXT.md](CONTEXT.md) (domain
vocabulary) and [architecture.md](architecture.md) before larger changes, and check
[limitations.md](docs/implementation/limitations.md) for what is known to be open.

Issues and pull requests go to [GitHub](https://github.com/MatiasGigena/LADE). For anything beyond a small fix,
open an issue first to agree on the approach. Security problems go through [SECURITY.md](SECURITY.md), not public
issues.

## Setup

Full details are in [build.md](docs/implementation/build.md).

Prerequisites: Linux x86_64, Node 22.23.1 / npm 10.9.8, Python 3.11+, curl, `sha256sum`, a C compiler/linker,
pkg-config, and the GTK 4.14 and GtkSourceView 5.12 development headers (on Ubuntu/Mint: `libgtk-4-dev`,
`libgtksourceview-5-dev`, `build-essential`, `pkg-config`). Running agents and most service tests also need
bubblewrap with working unprivileged user namespaces. The scripts never run sudo or change host packages.

One-time setup per clone:

```sh
./scripts/bootstrap-rust                                   # checksum-pinned rustup + Rust 1.98.1 into .tools/
npm --prefix packages/pi-worker ci --ignore-scripts --no-audit --no-fund
git config core.hooksPath .githooks                        # enable the pre-commit hook
python3 scripts/check-pins.py                              # check local toolchain and native baselines
```

Use `./scripts/cargo` (not a system `cargo`) so the pinned toolchain in `.tools/` is used. `check-pins.py` also
compares native package versions with the supported baseline; on other hosts `--portable` skips that part (this is
what `./scripts/verify` runs).

The pre-commit hook in `.githooks/pre-commit` runs only the worker lint (`npm --prefix packages/pi-worker run lint`)
against the working directory. It is not enabled automatically; the `git config` line above opts in.

## Verification

`./scripts/verify` is the gate: it must pass before a pull request is merged, and CI runs it. It covers pin checks,
schema/DTO/fixture generation drift, `cargo fmt`, locked build and tests, Clippy with `-D warnings`, architecture and
contrast checks, the license inventory, worker lint/type-check/tests, cross-language fixture comparison, the
installer tests and executable smoke checks. Individual commands are listed in
[build.md](docs/implementation/build.md#verification). The long native, load and resilience scripts
(`scripts/e2e-*`, `scripts/load`) are not part of it.

Generated files (`contracts/v1/schema.json`, `worker-schema.json`, DTOs, fixtures) are produced by
`scripts/generate-*.py`; edit the generator and re-run it rather than editing the output.

## Conventions

The full list is in [AGENTS.md](AGENTS.md). The main points:

- **Pinned inputs.** Toolchain and dependency versions are exact: `contracts/toolchain.json`, `rust-toolchain.toml`,
  `.node-version`, `=x.y.z` requirements in `Cargo.toml`, exact versions in `packages/pi-worker/package.json`, and
  the lockfiles. `scripts/check-pins.py` enforces them. Changing a dependency is a reviewed change, and npm installs
  always use `--ignore-scripts`.
- **Do not downgrade to fix type errors.** If an outdated dependency causes a type error, upgrade it instead of
  removing or weakening code. Check the dependency's own types in `node_modules`; do not guess APIs.
- **Rust.** `unsafe` is forbidden workspace-wide. New crate dependency edges, and platform imports in the domain crate, must pass the
  reviewed allowlist in `scripts/check-architecture.py`.
  A module with submodules keeps its root in `foo/mod.rs`, never `foo.rs` beside `foo/`; the same script checks it.
- **TypeScript.** The worker runs under Node's type stripping, so use only erasable syntax: no `enum`, `namespace`,
  parameter properties, `import =`/`export =`. No `any` unless absolutely necessary. Top-level imports only (no
  inline `import("pkg").Type`). External input is validated at the protocol boundaries without coercion.
- **Keybindings** are never hardcoded; add defaults to `DEFAULT_APP_KEYBINDINGS` in `crates/lade-desktop/src/presentation/keys/mod.rs`.
- **Keep it small.** Inline one-line helpers with a single call site. No backward-compatibility shims unless needed.
  Ask before removing behaviour that looks intentional.
- **Docs.** When behaviour, limits or guarantees change, update the matching document (user guide, the
  `docs/implementation/` contract, and `limitations.md`).

## Commits and pull requests

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/) with a scope, in the imperative,
lower case:

```text
fix(worker): bound streamed text and coalesced delta sizes
feat(history): paginate durable drafts and accepted inputs
test(service): correct sandbox fallback and credential checks
```

Common types are `feat`, `fix`, `test`, `refactor`, `docs` and `chore`. Keep each pull request focused, describe
what changed and how you verified it, and make sure `./scripts/verify` passes.

By contributing you agree that your contributions are licensed under the [MIT License](LICENSE).
