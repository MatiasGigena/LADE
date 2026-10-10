# LADE

Delegate implementation without delegating understanding or acceptance of the system.

LADE is a local-first Linux Agentic Development Environment centered on Objectives and their resulting work.

**Status:** the first-release collaboration loop is implemented — the authoritative `laded` service (SQLite store, admission, operation journal, restart recovery), sandboxed pi Agent Sessions (bubblewrap, egress gateway, scoped credentials), byte-exact inspection and versioned feedback, freeze/verify/approve and squash integration, and the native GTK4 workbench — together with a checksummed per-user bundle and installer. Its evidence comes from the **development host** (integration tests with disposable repositories and the offline `lade-fixture` model, run by `./scripts/verify` and CI); it has **not** been validated as an installed release on the supported Mint host, with live providers, or for accessibility. It is not yet a release: the remaining gates (resource limits and quotas, signing, pi compatibility, terminal, load, Orca/scaling, the comprehension exercise and Matias's go/no-go) are listed in [limitations](docs/implementation/limitations.md) and [acceptance status](docs/implementation/acceptance-status.md).

## Start here

- [CONTEXT.md](CONTEXT.md) — domain vocabulary and first-release boundary.
- [Product vision](docs/product/vision.md) — authority, understanding and intervention.
- [First-release workflow](docs/product/first-release-workflow.md) — inspection, versioned feedback, revisions, review and explicit integration.
- [UI principles](docs/product/ui-principles.md) — approved visual direction, original image and supplied skill text, operational-desktop adaptation and interaction/accessibility obligations.
- [Implementation readiness](docs/planning/implementation-readiness.md) — decisions, evidence from other branches, acceptance matrix, blockers and next increments.
- [Decision map #1](https://github.com/MatiasGigena/LADE/issues/1) — canonical planning map and issue history.

## Quick start

Build from source on Linux x86_64 (prerequisites and pinned versions: [build.md](docs/implementation/build.md)):

```sh
./scripts/bootstrap-rust                                     # pinned Rust toolchain into .tools/
npm --prefix packages/pi-worker ci --ignore-scripts --no-audit --no-fund
./scripts/verify                                             # build, tests, lints, contract checks
./target/debug/laded serve &                                 # the service (runs until killed)
./target/debug/lade-desktop                                  # the workbench
```

Or build a bundle and install it per user (no sudo; checks bubblewrap, Git ≥ 2.38, Node 22, GTK 4.14 and GtkSourceView 5.12):

```sh
./scripts/bundle && ./scripts/install --enable               # ~/.local, user unit lade.service
```

Then read the [user guide](docs/user-guide.md) (install, credentials, the Objective workflow, data locations, troubleshooting) and the [known limitations](docs/implementation/limitations.md).

## Build and contracts

See [build and test instructions](docs/implementation/build.md), the [wire protocol v1](docs/implementation/protocol-v1.md), the [service](docs/implementation/service.md), [execution safety](docs/implementation/execution-safety.md), the [pi worker](docs/implementation/worker.md), the [delivery plan](docs/implementation/delivery-plan.md), the historical [IMP-001 wire/admission contract](docs/implementation/imp-001-contract.md) and its [retained evidence and remaining gates](docs/implementation/evidence/imp-001/README.md).

## Implementation blueprint

- [System architecture](architecture.md) — modular Rust core, layers, adapters, ownership and design patterns.
- [Visual specification](design.md) — workbench appearance and proposed native visual tokens only.
- [Implementation spec and backlog](docs/implementation/spec.md) — concrete first-boundary proposals, 16 issue-ready increments, dependencies and A01–A24 coverage.
- [Agent handoff](docs/implementation/agent-handoff.md) — separate prompts for authorized issue publication and authorized implementation.

The architectural direction and backlog are approved/published. IMP-001 is separately authorized; later increments and their technical/token gates remain bounded by the spec. Documentation alone does not authorize additional production work or change accepted product policies.

The first release has no embedded editor or TODE dependency. Historical editor research/prototypes remain evidence of earlier exploration, not current gates. Rust remains the authoritative System Layer, pi the Intelligence Layer, and the graphical client presentation only. [GTK4 native is the selected graphical client](docs/planning/client-architecture.md), preserving the approved visual direction. [Provisional performance targets and the validation handoff](docs/planning/performance-budgets.md) are accepted. Production measurements, complete native/accessibility validation and production authorization remain open; accepted targets are not runtime guarantees.

[Product/architecture planning is decision-complete](https://github.com/MatiasGigena/LADE/issues/14#issuecomment-5699037043), with #14/#1 closure approved. Concrete protocols/defaults and all unperformed implementation/release gates remain assigned prerequisites, not proven capabilities. No production implementation or promotion of a prototype is authorized by planning closure. See [AGENTS.md](AGENTS.md) for repository working conventions.
