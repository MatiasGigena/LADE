# LADE

Delegate implementation without delegating understanding or acceptance of the system.

LADE is a local-first Linux Agentic Development Environment centered on Objectives and their resulting work. This repository contains **initial IMP-001 build/contract foundations**, planning and research—not a usable production application.

## Start here

- [CONTEXT.md](CONTEXT.md) — domain vocabulary and first-release boundary.
- [Product vision](docs/product/vision.md) — authority, understanding and intervention.
- [First-release workflow](docs/product/first-release-workflow.md) — inspection, versioned feedback, revisions, review and explicit integration.
- [UI principles](docs/product/ui-principles.md) — approved visual direction, original image and supplied skill text, operational-desktop adaptation and interaction/accessibility obligations.
- [Implementation readiness](docs/planning/implementation-readiness.md) — decisions, evidence from other branches, acceptance matrix, blockers and next increments.
- [Decision map #1](https://github.com/MatiasGigena/LADE/issues/1) — canonical planning map and issue history.

## Build foundations

See [build and test instructions](docs/implementation/build.md), the [IMP-001 wire/admission contract](docs/implementation/imp-001-contract.md) and [retained evidence and remaining gates](docs/implementation/evidence/imp-001/README.md). The native build shell and fail-closed service entrypoint do not execute agents or access a Workspace.

## Implementation blueprint

- [System architecture](architecture.md) — modular Rust core, layers, adapters, ownership and design patterns.
- [Visual specification](design.md) — workbench appearance and proposed native visual tokens only.
- [Implementation spec and backlog](docs/implementation/spec.md) — concrete first-boundary proposals, 16 issue-ready increments, dependencies and A01–A24 coverage.
- [Agent handoff](docs/implementation/agent-handoff.md) — separate prompts for authorized issue publication and authorized implementation.

The architectural direction and backlog are approved/published. IMP-001 is separately authorized; later increments and their technical/token gates remain bounded by the spec. Documentation alone does not authorize additional production work or change accepted product policies.

The first release has no embedded editor or TODE dependency. Historical editor research/prototypes remain evidence of earlier exploration, not current gates. Rust remains the authoritative System Layer, pi the Intelligence Layer, and the graphical client presentation only. [GTK4 native is the selected graphical client](docs/planning/client-architecture.md), preserving the approved visual direction. [Provisional performance targets and the validation handoff](docs/planning/performance-budgets.md) are accepted. Production measurements, complete native/accessibility validation and production authorization remain open; accepted targets are not runtime guarantees.

[Product/architecture planning is decision-complete](https://github.com/MatiasGigena/LADE/issues/14#issuecomment-5699037043), with #14/#1 closure approved. Concrete protocols/defaults and all unperformed implementation/release gates remain assigned prerequisites, not proven capabilities. No production implementation or promotion of a prototype is authorized by planning closure. See [AGENTS.md](AGENTS.md) for repository working conventions.
