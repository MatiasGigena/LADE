# LADE system architecture

Status: pre-release; the system described here is implemented. The developer approved the architectural direction in the 2026-09-16 handoff: a modular Rust core, layers with inward dependencies, ports/adapters at external boundaries, and separate graphical-client/agent processes. Choices still marked **Proposed** below record the reasoning behind them, and their acceptance evidence and open gates are tracked in [acceptance-status.md](docs/implementation/acceptance-status.md) and [limitations.md](docs/implementation/limitations.md).

Baseline: planning commit `60118474b226d1d1d518bd445d25977aa2b6cce3` and the accepted release boundary. The product decision map remains closed. This document specifies system structure; [design.md](design.md) specifies appearance and [the implementation spec](docs/implementation/spec.md) specifies delivery and executable acceptance.

## 1. Architectural choice

Use a **modular monolith for the authoritative System Layer**, with a separate GTK4 client and one isolated SDK worker process per active Agent Session. This is a local multiprocess application, not a microservice platform and not a single process containing everything.

The reasoning is specific to LADE: Objectives, accepted input, grants, frozen candidates and approvals require coordinated authority on one developer's machine. Keep that authority together, but separate capabilities and dependencies inside it. Process boundaries exist for lifecycle, isolation and runtime compatibility, not to independently deploy a service for every domain noun.

Do not introduce per-module databases, remote RPC between local domain modules, a distributed message broker, service discovery, Kubernetes or independent microservice release schedules. Revisit process separation only with a concrete isolation or measured failure/resource requirement. Do not reopen the GTK4 selection by default.

### Runtime topology

```text
GTK4 client (presentation; survives service failure)
        |
        | private, authenticated, version-matched Unix socket
        v
laded: unprivileged per-user Rust System Layer
  application use cases + domain rules + infrastructure adapters
  SQLite/product records | immutable artifacts | protected Git
        |
        | private versioned JSONL pipes; launch-bound worker identity
        +---- Node/pi worker: Objective A, Agent Session A
        +---- Node/pi worker: Objective B, Agent Session B
                   |
                   +---- tools / commands / descendants in granted scope

systemd user manager supervises laded and its delegated execution subtree.
LADE-launched effect-capable descendants cannot outlive lost Rust authority.
A controlled network gateway and PTY/process helpers remain Rust-owned.
```

The gateway is a capability-enforcement component, not another business service. An independently running helper must remain in the controlled lifetime and have a documented narrow protocol. The diagram omits ordinary operating-system services and does not imply that all worker syscalls are brokered: approved ordinary operations remain direct inside the OS boundary.

The accepted topology and wire families come from the component-boundaries decision; the OS-enforced boundary comes from the permissions-and-enforcement decision. Module privacy, Rust memory safety and a read-only UI are not that security boundary.

## 2. Module ownership and layers

### Core capabilities

| Module | Owns | Must not own |
| --- | --- | --- |
| Objectives | Workspace association, intent, Objective lifecycle, derived attention | Model reasoning, widget state or Git implementation |
| Inspection | Capture/reference identities, candidate manifests, search/version scope | Unrestricted filesystem reads from a worker-supplied path |
| Feedback | Draft revisions, batches, immutable accepted input and routing prerequisites | Grants, automatic resolution approval or implicit integration |
| Review | Frozen candidate identity, evidence association, approvals and integration preconditions | Shell execution details or source rendering |
| Execution | Session associations, execution generations, grants and supervised operation state | Pi conversation semantics or provider-specific authentication logic |

These are modules, not five deployable services. They expose domain values and transitions rather than reaching into one another's storage. Cross-module orchestration belongs to the application layer in `lade-system/src/service`. A small shared set of typed IDs is appropriate; an unbounded `common`, `utils` or `manager` module is not.

### Dependency direction

- **Domain:** values, invariants and allowed transitions. No GTK, SQLite, Git subprocess, Node SDK, wall-clock access or network I/O.
- **Application:** `lade-system/src/service`, one module per capability, coordinates use cases by calling the domain transitions in `lade-core` and the adapters directly. It owns the sequence for durable acceptance, dispatch and reconciliation, and it is the only layer that maps protocol DTOs to domain values (`service/convert.rs`). `lade-core` has no use-case or port layer of its own: the service is single-threaded over in-memory state with one SQLite transaction per change, so a port abstraction would only mirror the storage functions it already calls.
- **Adapters/composition:** implements persistence, filesystem/Git, process supervision, gateway, credential custody and transports. Wires explicit dependencies at startup.
- **Client presentation:** projects authoritative state, keeps transient selection/layout/unsaved editing state, and requests actions. It never independently authorizes a domain transition.

Dependency arrows point from adapters into application/domain contracts, not outward from domain code. Protocol DTOs are a boundary representation, not the mutable domain entities or pi's exported SDK types.

### Repository layout

```text
architecture.md, CONTEXT.md, design.md
docs/implementation/            # contracts, limits and acceptance status per area
crates/
  lade-core/src/
    domain/{objectives,feedback,review,execution}/, domain/{inspection,recovery,ids}.rs
                                # values, invariants and transitions; no platform imports
  lade-protocol/src/            # wire DTOs, validators, framing (client and worker)
  lade-system/src/
    service/                    # application layer: one module per capability
                                #   (objectives, feedback, capture, inspect, review,
                                #   execution, agent_session, models, resources,
                                #   terminal), dispatch, recovery, convert.rs, and
                                #   projection/ (summaries, details, offered actions
                                #   and the reply budget)
    storage/                    # SQLite schema, migrations, rows, backups
    supervision/                # worker launch, egress gateway, credentials, terminals
    adapters/{artifacts,git,profile,providers,sandbox}/
    transport/                  # client socket admission and framing
    support/                    # clock, randomness, hex, private directories, child waits
    diagnostics.rs, fault.rs    # service log capture; test fault injection
    bin/laded.rs                # composition root and CLI
  lade-desktop/src/
    presentation/               # state projection and controller, no business authority
    views/                      # GTK widgets and view composition
    transport/                  # bounded client connection
    assets/                     # reviewed icons, GTK styles, resources
  */tests/                      # per-crate integration tests and fixtures
packages/pi-worker/             # narrow TypeScript SDK adapter
vendor/pi-coding-agent/         # pinned narrow product-fork source/patch record
contracts/                      # versioned schemas, toolchain pins and wire examples
packaging/                      # matched component bundle and user-service setup
scripts/                        # build, verification and generator scripts
```

`storage` and `supervision` are adapters too, but they sit beside `adapters/` because each is a subsystem with its own state (the database and its migrations; worker processes and their gateways) rather than a single external seam. Lower modules never import `service`: shared primitives live in `support`, and provider facts that profile import and supervision both need live in `adapters/providers.rs`; `supervision/provider_access.rs` only classifies access and egress from those facts. Constants live in, and are imported from, the module that owns them, never through a re-export.

Split a module into another crate only for a real dependency/test/reuse boundary. `lade-core` has no platform dependencies. `lade-desktop` depends on `lade-protocol`, never on `lade-system` or its SQLite connection. `lade-system` maps protocol data into core transitions. The protocol crate is independent of core and contains no approval authority. Cargo dependency checks enforce crate boundaries; import checks and focused review enforce layers inside a crate.

**Proposed implementation choices:** Rust/gtk4-rs for the client; TypeScript for the Node adapter; GtkSourceView 5 as the first read-only text-control candidate; VTE's GTK4 variant only as a terminal presentation candidate. No Libadwaita requirement, web engine or custom-drawn whole UI. Pin runtime, bindings and native libraries in IMP-001; validate rich controls in IMP-002 and terminal ownership in IMP-012. These were not selected by the client decision's C benchmark.

The four-crate boundary uses Rust/gtk4-rs, locked build inputs and protocol fixtures. See the [wire contract](docs/implementation/protocol-v1.md), [service admission](docs/implementation/service.md#admission-transport) and [remaining release gates](docs/implementation/limitations.md).

The narrow coding-agent product fork required by the pi compatibility decision is retained. Do not replace it with floating upstream packages or fork the agent/provider layers without demonstrated need. Record upstream commit, runtime/dependency lockfiles, notices and each necessary patch; do not copy historical prototypes into production.

## 3. State and concurrency

| State | Authoritative owner / storage |
| --- | --- |
| Objectives, grants, accepted inputs, operations, review/approval records | Rust / one SQLite product database |
| Pi conversation trees, compaction, extension session semantics | Authorized pi writer / owned native session files |
| Inspected bytes, candidate manifests, retained verification output | Rust-controlled immutable artifact storage; retained references |
| Repository contents and commits | Objective worktrees and Git; protected metadata/refs mediated by Rust |
| Credentials | Rust custody / accepted secure storage; scoped release to trusted worker domains |
| View selection, expansion, focus, unacknowledged edits | Client transient presentation state; not a second product database |

Only the service writes product records. Each pi session has one authorized writer. Clients cannot open the database or edit review artifacts. An imported transcript never restores approvals or repository state. Completed/discarded worktree Cleanup does not delete history or referenced inspection context.

**Proposed coordination:** one serialized database-write path with short transactions; bounded background executors for blocking Git/filesystem operations; per-Objective command sequencing, and per-Integration-Target serialization for LADE-owned integrations. Validate generations and optimistic versions again when delayed operations complete. Never hold a database transaction across a provider call or long-running tool. Locks inside LADE do not stop external Git clients: validate actual state at correctness boundaries.

GTK work stays on its main context. Heavy capture, search, Git and storage work stays outside the UI loop. Transfer plain presentation data through bounded channels; coalesce replaceable visual updates only. Do not spawn an executor/thread per widget or rebuild the full widget tree on every token. This follows the [GTK Rust main-loop guidance](https://gtk-rs.org/gtk4-rs/git/book/main_event_loop.html), not a claimed benchmark.

## 4. Durable commands, artifacts and recovery

Adopt typed commands and queries, explicit operation IDs and an operation journal. This is **not full CQRS infrastructure or event sourcing**: current records remain authoritative; a transient replay cache is not the system of record.

For accepted feedback, require this order:

1. Authenticate the controlling channel; validate Objective/session associations, lifecycle, draft versions and exact references.
2. Make required captured artifacts durable before publishing durable references to them.
3. In one product-store transaction, record immutable accepted input, associate the exact submitted draft revisions and record the operation outcome/dispatch intent.
4. Acknowledge durable acceptance. Before dispatch, revalidate routing, execution authority and stale context.
5. Record observed delivery/result facts separately. An uncertain send after interruption is attention/reconciliation work, never permission for automatic replay.

**Proposed storage implementation:** SQLite WAL with `synchronous=FULL`, foreign keys and serialized migrations; immutable artifacts written through same-filesystem temporary files, synchronized, published without replacement and directory-synchronized before reference commits. Validate the actual filesystem/VFS and failure cases. SQLite's [synchronous documentation](https://www.sqlite.org/pragma.html#pragma_synchronous) informs this choice; configuration alone is not a demonstrated cross-store durability guarantee. Never claim a transaction spanning SQLite, pi files, Git and external effects.

Artifact collection is reference-aware. Git garbage collection must not remove an object still needed by retained review history: pin it through service-owned retention refs or copy its exact bytes into owned storage. Clean only proven-unreferenced temporary artifacts, not history or unsent draft context. Numeric reserves, spool bounds and hard writable-storage enforcement are preconditions assigned in the spec, not invented defaults here.

A duplicate request identity with the same canonical payload queries/returns the original durable product outcome; the same identity with different content is rejected. This deduplicates product acceptance, not arbitrary external effects. After service restart, old grants, pending prompt authority and unconsumed approvals expire; interrupted work waits for explicit continuation. Pi and service reconciliation must preserve uncertainty rather than manufacture success.

## 5. Host authority and protected Git

Maintain distinct trusted client, worker and helper admission routes. Worker-controlled IDs or `approved: true` fields confer no authority. Socket ownership/peer credentials identify an OS principal; they are not enough to distinguish a trusted client from a worker running as that principal. The IPC gate must define launch-bound capabilities, session/execution generations and exclusive controlling-client admission; control material must never enter a worker's environment or accessible filesystem.

Contain code before loading extensions/providers or running tools. No home directory, other Objective, common writable Git metadata, host control sockets or unrestricted bus access by default. Retain explicit immutable extension trust and the accepted in-process credential exposure. Destination filtering is not data-loss prevention. Permission revocation takes effect only when retained handles, connections and descendants no longer carry the old authority.

The service owns the PTY and process lifecycle. A future VTE widget renders the terminal; it does not launch an unrestricted shell or supply an alternate authority path. Closing the GUI does not end authorized background execution; Exit LADE and loss of the service have their separately specified effects. Worker/session continuity cannot be promised across a security-driven recreation.

For Git integration, separate candidate construction, observed verification, human approval and explicit integration. Freeze or exclude every relevant writer using the enforcement boundary, not UI state. Recheck exact candidate and target at integration, journal the intended commit and expected target, and preserve the original candidate for historical comparison.

Use an expected-old-value reference update as part of the proposed Git adapter: [Git documents this check](https://git-scm.com/docs/git-update-ref.html). It is **not** a complete safe integration protocol for a checked-out branch. IMP-009 must specify checkout/index consistency, ordering, failure recovery and external-change detection without `reset --hard`, automatic stash or overwriting unrelated work. Serialize protected operations, treat repository config/hooks/filters/helpers as untrusted, and never let them execute with the broker's ambient authority.

## 6. Patterns adopted and boundaries

| Pattern | Apply it to | Limit |
| --- | --- | --- |
| Ports and adapters | Git, artifacts, storage, supervision, worker and credentials | No interface for every trivial function; no generic repository hiding transaction boundaries |
| Explicit state transitions and typed IDs | Objective/review/approval/input/execution identities | Preserve the accepted lifecycle; do not invent a giant shared enum or combinations of approval booleans |
| Command/query separation | Effectful use cases versus read projections | One product store; no distributed CQRS or mandatory separate read database |
| Durable operation journal and reconciliation | Acceptance, dispatch, Git integration, Cleanup and migrations | No exactly-once promise and no automatic replay of ambiguous external effects |
| Optimistic versions plus scoped serialization | Draft revisions, candidate/target approval and association changes | A service lock is not external writer exclusion |
| Explicit dependency injection | Construct adapters (launcher, store, artifact store) at service startup; controllable launcher and faults in tests | Constructors, not a global service locator or DI framework |
| Unidirectional presentation | User action to request to service state to GTK projection | Optimistic visuals cannot claim Saved, approved or integrated before authority confirms |

Keep pi SDK types and differences inside the adapter. Preserve the recorded findings: clear queued input before abort when that is the intended action; distinguish live context from persisted history; checkpoint/reconcile a selected tree leaf explicitly; implement host-side semantic-UI cancellation before a blocking replacement; unsupported required UI must be rejected before activation, not handled by assuming exceptions veto operations. Use the [compatibility inventory](docs/implementation/pi-compatibility.md) and its remaining conformance gaps.

## 7. Enforceable architecture rules

- CI checks the allowed crate graph and forbidden imports; no GTK/SQL/process APIs in domain modules and no service/storage dependency in the client.
- Core tests exercise exact candidate/target binding, stale batches, immutable input and legal transitions without GTK or pi.
- Adapter contract tests cover real serialized messages and installed pinned SDK seams; do not require identical model-generated prose.
- Fault tests inject failure before/after artifact publication, product commit, dispatch, Git ref change and cleanup; query outcomes and retain ambiguous cases.
- Security tests execute direct Node calls, commands and descendants, not just intercepted tool hooks. Include forged authority, retained handles, host sockets, disk exhaustion and supervisor loss.
- No production feature bypasses the accepted hard limits or installed containment because a fixture used a fake provider. Read-only client tests may use a labelled simulator with no real effects.
- [Performance budgets](docs/planning/performance-budgets.md) are the sole numeric runtime/transport baseline. Do not duplicate or silently relax them. PSS targets are not cgroup kill limits; cgroups are not disk quotas.

## 8. Decisions remaining within implementation

The architectural style is settled for this handoff. The following are bounded implementation decisions, not reasons to reopen the product map:

| Decision | Proposed direction / latest gate |
| --- | --- |
| Client language, bindings, dependency versions | Rust/gtk4-rs proposed; pin supported dependency set in IMP-001 |
| Native text/diff and terminal composition | GtkSourceView 5 / service-owned PTY with VTE candidates; validate in IMP-002 / IMP-012 |
| Wire/schema identity encoding and request limits below aggregate caps | Initial examples in spec; ratify/generated-schema checks in IMP-001 before IPC code |
| Artifact/database ordering, storage reserves and migrations | Above proposal; concrete fault matrix in IMP-003/004 before those effects |
| Hard process, CPU, memory and writable-storage defaults/mechanism | Driver proposes and the maintainer approves in IMP-006 before untrusted launch |
| Checked-out target integration and recovery | Bounded Git protocol review in IMP-009 before target mutation |

Document a significant implementation tradeoff beside its owning contract, or in a focused ADR when it changes an architectural boundary. Do not manufacture a catalogue of patterns or ADRs. Changes to accepted product/security policies need explicit approval; completing an engineering detail does not.
