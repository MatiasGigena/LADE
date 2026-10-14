# LADE implementation specification and execution backlog

Status: **approved specification; execution backlog published in [IMP-MAP #21](https://github.com/MatiasGigena/LADE/issues/21)**. Backlog publication does not authorize production implementation. Technical details identified below as implementation proposals remain subject to ratification at their named increments.

Baseline: [PR #19](https://github.com/MatiasGigena/LADE/pull/19) at `60118474b226d1d1d518bd445d25977aa2b6cce3`; implementation blueprint [PR #20](https://github.com/MatiasGigena/LADE/pull/20); [#13 resolution](https://github.com/MatiasGigena/LADE/issues/13#issuecomment-5698708894); [#14 resolution](https://github.com/MatiasGigena/LADE/issues/14#issuecomment-5699037043). Product planning is complete. Do not reopen the old map or repeat client/agent research merely to begin implementation.

## 1. Document authority and scope

| Document | Responsibility |
| --- | --- |
| [CONTEXT.md](../../CONTEXT.md) and accepted issue resolutions | Vocabulary, product policy and authority; substantive changes require explicit approval |
| [Workflow](../product/first-release-workflow.md) and [UI principles](../product/ui-principles.md) | Functional interaction and accepted design/accessibility constraints |
| [architecture.md](../../architecture.md) | System decomposition, layers, ports/adapters, state ownership and patterns |
| [design.md](../../design.md) | Visual tokens, composition and component appearance only |
| This spec | Implementation contracts, ordered deliverables and acceptance mapping |
| [Readiness matrix](../planning/implementation-readiness.md#acceptance-matrix) and [performance budgets](../planning/performance-budgets.md) | Canonical A01–A24 obligations and numerical targets; linked, not independently redefined |

Later explicit issue resolutions supersede earlier clauses only within their recorded scope. A mockup or tool response cannot change lifecycle rules. Newly proposed bindings, dimensions, protocol details and storage implementation are not retroactively approved by #13/#14. Record their acceptance in the relevant PR before coding that boundary.

Scope remains one developer, one Linux machine and one Git Workspace, isolated concurrent Objectives, GTK4, the Rust System Layer and pinned pi coding-agent SDK. No embedded editor/TODE, cloud requirement, microservice platform, provider subset or redesigned pi loop. The first vertical slice is a development milestone, not the release boundary.

## 2. Delivery model and authorization

The execution backlog is published in [IMP-MAP #21: Deliver the first credible LADE release](https://github.com/MatiasGigena/LADE/issues/21), referencing the closed decision map #1. Its children below are work items, not one issue per A-row. IDs such as IMP-004 are stable planning keys, not GitHub numbers.

The authoritative stable-key-to-GitHub mapping and native dependency graph live in #21; do not duplicate that mapping here. Search all issue states before any future split or tracker maintenance, reuse matching stable keys and read back native links. Do not infer issue numbers, silently change existing dependencies, recreate the closed planning issues or let backlog publication authorize production work.

The implementation driver produces evidence; Matias approves consequential defaults, changes to accepted contracts, product acceptance and release go/no-go. Work in an isolated branch per reviewable increment. Do not merge prototype branches wholesale. New native production components are built against the accepted contracts, not by promoting permissive fixture brokers.

## 3. Initial technical contract proposal

These decisions make the first increment concrete. They are **implementation proposals for review**, not assertions about existing code. IMP-001 ratifies the wire envelope and toolchain; each storage/security/Git increment ratifies its bounded protocol before effects.

The foundational DTO/schema, framing, identities and decoded/chunk bounds are defined in the [wire contract](protocol-v1.md), with [build commands](build.md) and [service admission](service.md#admission-transport). Independent wire/security sign-off remains pending. The examples below remain illustrative; `contracts/v1/schema.json` specifies the actual envelope. Current implementation coverage and remaining gates are recorded in [acceptance status](acceptance-status.md).

### 3.1 Identity and command envelope

Propose protocol major version 1. Use opaque typed IDs with explicit validation; UUIDv4 is the proposed generated-ID representation. Carry integer revisions/sequence values as bounded decimal strings across Rust/TypeScript to avoid numeric precision loss. Do not encode authorization in an ID. Git identities carry the repository object-format algorithm and complete object ID; do not assume every repository uses SHA-1.

Client frames: four-byte unsigned big-endian UTF-8 JSON body length, followed by exactly that body. Worker frames: one UTF-8 JSON object per byte LF. Escape embedded newlines; do not split JSONL on Unicode U+2028/U+2029. Both instantiate the already accepted 1 MiB encoded-body cap. Reserve control capacity and apply the canonical frame, queue and replay limits **before** unbounded allocation. Define bounded JSON depth/collections/strings and finite chunk reassembly in IMP-001; an encoded-size check alone is insufficient.

An illustrative request, with symbolic IDs rather than real repository data:

```json
{
  "protocol": 1,
  "kind": "command",
  "request_id": "request-identity",
  "operation_id": "stable-user-action-identity",
  "expected_service_epoch": "current-service-identity",
  "scope": {
    "workspace_id": "workspace-identity",
    "objective_id": "objective-identity",
    "session_id": "session-identity",
    "association_revision": "3"
  },
  "name": "feedback.submit",
  "payload": {
    "batch_id": "batch-identity",
    "expected_draft_revision": "7",
    "delivery_mode": "follow_up"
  }
}
```

The service derives caller authority from the admitted channel, never a payload `actor`, `role` or `approved` field. Service/association/execution generations reject stale requests but are not secrets. A separate authenticated connection/control capability protocol must prevent workers from impersonating the single controlling client.

Response families: rejection with typed reason; durable acceptance with input/operation identity; query result; ordered state/event projection. Operation completion is a separate result. After lost acknowledgement query `operation.get`; do not submit the action as a new operation to make the error disappear. Matching duplicate operation/payload returns the original input outcome; mismatched reuse rejects.

Large logical pi messages/images/tool results are chunked or referenced through authorized artifacts, not truncated to 1 MiB. Pin correlation, order, checksum/length, ownership, cancellation and reassembly bounds. Streaming visual projections may be coalesced; accepted input, evidence, protected transitions and pi semantics may not be silently dropped. A slow client resynchronizes from a snapshot and paged history; a worker has a bounded drain/spool or explicit failure path, never unbounded disk buffering.

### 3.2 Exact inspection and drafts

Propose an immutable content manifest with: artifact ID, content digest/length, repository-relative path representation, file kind/mode, capture kind, source Git identity where applicable, Objective/session association, comparison target and diff side. Use SHA-256 for owned byte-content identities as an implementation proposal, distinct from Git's object IDs. Specify canonical manifest serialization before relying on its hash.

Selections use zero-based half-open **byte offsets in exact retained content**. Text widgets convert character/grapheme positions at the presentation boundary; test non-ASCII, tabs, combining marks and CRLF without rewriting the source bytes. Lines are display aids, not authority. Linux paths may contain non-UTF-8 bytes: define a lossless encoded path representation and a separate display label, rather than silently replacing characters or treating a display path as an authorization decision.

A live capture proves the displayed bytes, not an atomic multi-file state. A frozen candidate requires writer exclusion, coherent manifest construction and target association. Symlink handling, modes, deletes, renames, untracked files, binary content, capture races and Git object retention are explicit IMP-004/009 acceptance cases.

Draft revisions use compare-and-set updates. Return the acknowledged durable revision; a newer edit never becomes Saved because an older write completed. A stale item blocks its **whole** batch until explicit update/removal, including if it becomes stale between durable acceptance and dispatch. Accepted input remains immutable; a corrected submission is linked new input. Missing/stale destination or generation never triggers silent rerouting.

### 3.3 Records and durability

Define migrations for these record families, not separate databases per module:

| Record family | Required identity/invariant |
| --- | --- |
| Workspace, Objective, intent | Stable IDs, repository association, explicit human-owned intent and accepted lifecycle |
| Session association and execution | Pi-native ID/file reference, association revision, execution generation and lifecycle |
| Draft and batch | Monotonic draft revision, exact retained context, submitted-input association |
| Accepted input and operation | Immutable canonical payload, operation identity, acceptance fact and distinct delivery/result state |
| Candidate, verification, approval | Exact manifest/target, evidence provenance, override reason where allowed; consumable invalidatable approval |
| Grant, extension trust and pending interaction | Execution-bound authority versus immutable-code trust; request validity and expiry |
| Artifact and reference ownership | Immutable identity, durable bytes, retention references and bounded temporary state |

SQLite WAL/FULL, explicit dependency injection and artifact-before-reference ordering are the proposed architecture implementation, not a new durability promise. IMP-003/004 must prove crash outcomes and migrations before these paths are used. State-changing operations use short explicit transactions; cross-module atomic association must not be split by generic independent repositories. No transaction spans Git, the pi session files and the product database.

Durable dispatch intent is not a generic retrying outbox. Record known phases; a write to a worker pipe followed by failure may remain uncertain. Reconcile and surface that state without replay. Distinguish reversible product bookkeeping from non-retractable external effects.

### 3.4 Contract error outcomes

Define typed errors for unauthorized caller, stale service/association/execution/reference/candidate/target, invalid lifecycle, unsupported content/version, durability failure, permission/limit denial, interrupted/unknown operation and expired replay cursor. Preserve a safe action: refresh, inspect previous context, query operation, retry an idempotent read or request explicit continuation. Do not mark every error automatically retryable. No raw secret, prompt or source content in default diagnostics.

### 3.5 Safety prerequisites and numeric sources

The accepted [performance/transport budgets](../planning/performance-budgets.md) are the only runtime target table. Their fixture-derived targets are not observed production performance, kill thresholds or permission to reduce pi compatibility. Four workers is a measurement point, not a product concurrency limit. First-slice and concurrency issues use the exact reference workloads, accounting, repeats, latency definitions and soak criterion from that document.

Hard execution CPU/memory/process-thread/writable-storage limits, capture/spool/history reserves and diagnostic rotation defaults remain explicit approval gates. IMP-004/006 proposes finite defaults with units, aggregate accounting, mechanism and exhaustion tests; Matias approves before the affected boundary or untrusted execution. An unspecified quota or disk monitor alone does not satisfy hard storage enforcement. Do not substitute cgroups for disk quotas, automatically expand limits, or delete retained history under pressure.

## 4. Backlog and dependency graph

All dependencies below are prerequisites for the issue's accepted result, not a ban on isolated design work. No untrusted execution precedes IMP-006/007 regardless of development order. Fixture work must remain effect-free or explicitly controlled in disposable scratch contexts.

| ID | Deliverable | Depends on | Milestone |
| --- | --- | --- | --- |
| IMP-001 | Establish the build boundary and versioned contract fixtures | None | M0 foundations |
| IMP-002 | Validate the native inspection/feedback visual component | IMP-001 | M0 foundations |
| IMP-003 | Open a durable Workspace through the authoritative service | IMP-001 | M1 vertical slice |
| IMP-004 | Inspect and search exact retained repository context | IMP-003 | M1 vertical slice |
| IMP-005 | Save, restore and explicitly submit versioned feedback | IMP-004 | M1 vertical slice |
| IMP-006 | Launch and stop a bounded isolated Objective execution domain | IMP-003 | M1 safety prerequisite |
| IMP-007 | Enforce network, credential and executable-extension authority | IMP-006 | M1 safety prerequisite |
| IMP-008 | Run an Objective through the pinned SDK worker | IMP-005, IMP-007 | M1 vertical slice |
| IMP-009 | Freeze, verify, approve and integrate an exact Change Set | IMP-004, IMP-006, IMP-008 | M1 vertical slice |
| IMP-010 | Demonstrate the complete one-Objective collaboration loop | IMP-002, IMP-005, IMP-009 | M1 vertical slice |
| IMP-011 | Complete pi session, resource, extension and provider compatibility | IMP-008 | M2 complete scope |
| IMP-012 | Add the contained terminal and desktop lifetime/attention flows | IMP-006, IMP-008, IMP-010 | M2 complete scope |
| IMP-013 | Harden concurrent Objectives, recovery and bounded load | IMP-010, IMP-011, IMP-012 | M2 complete scope |
| IMP-014 | Install, update and roll back a signed coordinated bundle | IMP-007, IMP-010, IMP-011, IMP-012 | M3 installed release |
| IMP-015 | Validate developer comprehension with a seeded-defect exercise | IMP-013 | M3 product acceptance |
| IMP-016 | Assemble installed release evidence and obtain go/no-go | IMP-013, IMP-014, IMP-015 | M3 release acceptance |

M0 does not certify the app; M1 does not permit shipping a provider subset. M2 includes full accepted compatibility and failure handling. M3 requires all applicable A01–A24 gates. The published backlog contains 16 implementation issues plus one map, not 24 independent mini-projects.

## 5. Published issue specifications

The fields below, this spec's common Definition of Done and the linked A-rows form the specification for each published child of #21. The real key-to-number mapping remains in #21 rather than being duplicated here. First-increment details are deliberately more concrete; later contract details are finalized at their named boundary rather than guessed for the whole application now.

<a id="imp-001"></a>
### IMP-001 — Establish the build boundary and versioned contract fixtures

**Outcome:** the selected native/Rust/Node arrangement can build from pinned inputs, with machine-checkable dependency boundaries and protocol fixtures before runtime features.

**Scope:** review the four-crate layout and proposed Rust/gtk4-rs client; pin supported compiler/native/runtime/dependency set on Mint; preserve the narrow pi fork/patch manifest. Establish command/response/event DTOs, framing, identity validation, admission design, decoded/chunk bounds and error fixtures. Add build/test commands, locked manifests and architecture checks. No provider calls, shell tools or user-repository writes.

**Acceptance:** clean checkout builds the minimal native shell and service entry points; the client cannot depend on service/storage; domain tests need neither GTK nor SQLite. Rust/TypeScript fixtures agree on valid/invalid frames, Unicode, partial reads, duplicate requests and oversized/deep input. Exact version mismatches reject before commands. Wire/security reviewers sign off the controlling-client versus worker admission design; a UI role field is insufficient.

**Evidence:** dependency graph, pinned manifests, documented commands and actual outputs; fixture corpus and size-bound failures. Proposed commands once configured: locked workspace build/tests and the package's named TypeScript test script. Do not report commands as run before they exist. **A coverage:** A12, A15, A19, A22 foundations. **Excluded:** feature implementation, invented full API parity and a new framework.

<a id="imp-002"></a>
### IMP-002 — Validate the native inspection/feedback visual component

**Outcome:** a small actual GTK4 component proves selection, feedback display and the visual specification before committing to a rich control.

**Scope:** build the approved workbench composition with explicitly labelled fixtures, global search placement and the visual state sheet. Evaluate GtkSourceView 5 first; implement a minimum readable diff surface rather than assume it supplies a review platform. Demonstrate the bounded native selection-to-feedback path and return to inspection with no repository mutation.

**Acceptance:** screenshot/contrast sheet matches design rules; no Objective accent bar, extra tagline or greeting. Native keyboard/F6/focus, Orca selected-context/signs and actual 200% scaling pass the accepted checkpoint. Source-byte-to-selection mapping is tested. A failing component is simplified/replaced before rich-control commitment; no silent waiver or automatic toolkit bake-off. No real worker or service authority is simulated as proven.

**Evidence:** actual native captures, focus trace, Orca task record, contrast pairs and failure corrections. Record untested states. **A coverage:** A01–A06 presentation, A17/A18 component gate. **Excluded:** new product flows, webview, permanent fixture state or claim of integrated runtime performance.

<a id="imp-003"></a>
### IMP-003 — Open a durable Workspace through the authoritative service

**Outcome:** launch the client, connect to the single service, create/read an Objective and reconnect without losing authoritative state.

**Scope:** implement admitted versioned IPC, snapshot plus ordered projections, serialized SQLite writes/migrations, service epoch/operation queries and bounded default diagnostics. Propose and approve log limits/redaction/export exclusions before persistent diagnostics. Install the development user-service arrangement with explicit host-setup authorization.

**Acceptance:** only the controlling client can request mutations; wrong/stale identities and versions reject; domain/state does not live in widgets. Lost acknowledgement is queried, no duplicate Objective. Database write failure/corruption is visible and preserved; reopening never silently resets the store. Client loss does not stop service availability; service restart invalidates authority as specified. No blocking I/O on GTK's main context.

**Evidence:** IPC integration tests, migration/fault traces and reconnect snapshots; diagnostic fixtures without secrets/bodies. **A coverage:** A09/A10/A12/A21/A24. **Excluded:** agent execution and any promise that session files participate in SQLite transactions.

<a id="imp-004"></a>
### IMP-004 — Inspect and search exact retained repository context

**Outcome:** inspect files and search results independently of agent prose, with addressable retained bytes for feedback.

**Scope:** safe repository access, immutable artifacts/manifests, version-labelled file/diff views, Objective/file/content global search with explicit scope, lossless paths and text offsets. Finalize capture consistency, quota/reserve defaults, GC-root retention and artifact-before-reference ordering before implementing writes.

**Acceptance:** uncommitted text is retained without creating a development commit; viewing does not freeze writers. A working capture never claims atomic multi-file state. Unicode/CRLF/non-UTF-8 paths, symlinks, renames, binary/large files, external changes and search-result staleness behave explicitly. Read access cannot escape grants. Disk-full at each publication step preserves prior references; Cleanup/Git GC cannot erase referenced bytes. Stress content is bounded/paged, not apparently complete truncation.

**Evidence:** exact byte/hash fixtures, race/symlink cases, storage faults and native version-labelled captures. **A coverage:** A01/A02/A03/A06/A19/A24. **Excluded:** semantic navigation engine, automatic re-anchoring and embedded editing.

<a id="imp-005"></a>
### IMP-005 — Save, restore and explicitly submit versioned feedback

**Outcome:** collect a batch, save exact revisions, reopen it unsent and explicitly accept it for the intended session without ambiguous UI status.

**Scope:** draft CAS, accepted-input records, whole-batch stale rules, destination confirmation, safe close, operation queries and pending dispatch records. Use a non-effectful worker stub until the safety/SDK gates pass.

**Acceptance:** scheduling and durable-save labels match the canonical timing targets; stale save acknowledgements never mark newer edits Saved. Failed saving supports retry/stay open or explicit loss confirmation. Stale items block the full batch at submission and dispatch. Lost acceptance acknowledgement returns/query-resolves the original input. Reconnected drafts reconcile rather than overwrite; accepted input cannot reappear as a new unsent draft. Submission grants neither capabilities nor integration approval.

**Evidence:** save/ack/disconnect/corruption cases, immutable-history assertions and native status captures. **A coverage:** A02/A03/A04/A09/A24. **Excluded:** silent partial batches, history rewrites, automatic replay and arbitrary offline-crash recovery.

<a id="imp-006"></a>
### IMP-006 — Launch and stop a bounded isolated Objective execution domain

**Outcome:** service-controlled scratch execution cannot escape its scope or retain authority after the service is lost.

**Scope:** lazy worktree creation, systemd/delegated cgroup arrangement, pinned namespace/seccomp launcher, filesystem/FD/environment restrictions, mediated development Git operations and per-Objective/aggregate resource accounting. Driver proposes hard CPU/memory/process-thread/storage defaults and an actually enforceable disk-quota mechanism for the supported installation; Matias approves before untrusted launch. No unsandboxed fallback or global host-security disablement.

**Acceptance:** direct Node/subprocess/symlink/FD/host-socket and protected-ref attempts are denied. Daemonizing descendants terminate on supervisor loss. Missing controllers/namespace/quota prerequisites block execution. Revocation accounts for open handles; limit exhaustion is visible, never automatic restart or quota expansion. Common Git metadata is not writable; repository helpers cannot execute with broker authority. Cancellation preserves work but cannot claim stop while relevant descendants still run.

**Evidence:** installed-boundary adversarial tests, quota exhaustion, scoped host configuration and complete descendant/accounting traces. **A coverage:** A07/A10/A11/A12/A13/A20/A24. **Excluded:** gateway bypass, real credential release and assuming cgroups enforce disk quotas.

<a id="imp-007"></a>
### IMP-007 — Enforce network, credential and executable-extension authority

**Outcome:** isolated execution can contact authorized destinations and load trusted code without gaining client approval authority or unrestricted secrets.

**Scope:** enforced egress gateway, endpoint grants, immutable executable snapshots/dependencies, Secret Service custody/memory-only option, credential refresh coordination and trusted login helpers without Workspace extensions. Preserve provider-owned authentication in pi; design transports for the full pinned provider contract.

**Acceptance:** direct bypass, redirects, DNS/address changes, IPv4/IPv6 and private/host endpoints obey scope. New/changed executable code requires approval. Keyring lock/cancel/error never creates plaintext fallback. Worker exposure is explicit; outer isolation is not per-extension secrecy. Grant changes close old access and require explicit continuation where recreation is needed. No released control-client capability is reachable from a worker.

**Evidence:** network/admission/extension snapshot fixtures, revoked-connection tests and auth/refresh races. **A coverage:** A12/A13/A14/A16. **Excluded:** DLP claims, shared pi credentials, provider subset as final acceptance and automatic broad network approval.

<a id="imp-008"></a>
### IMP-008 — Run an Objective through the pinned SDK worker

**Outcome:** accepted input reaches a real SDK Agent Session under Rust's execution authority, and its activity survives honest reconnect/recovery.

**Scope:** TypeScript adapter over the pinned narrow fork; semantic UI routing, streaming/tool events, durable input dispatch and generation handling. Start with a deterministic credential-free provider and disposable tools inside the real approved boundary. Do not require fresh research into unrelated pi internals.

**Acceptance:** maintain clear/abort/steering/follow-up ordering; settled turn is not Objective completion. Preserve native tool payloads and logical large results. Persist/reconcile selected tree-leaf identity and live-versus-history differences. Host owns semantic-UI cancellation and stale replies; respect valid pending interactions and actual timeout/disconnect policy. Required unsupported extension UI is rejected before activation, not by assuming an exception vetoes effects. Crash-before/after dispatch/tool effects remains known or explicitly uncertain, with no blind resend.

**Evidence:** retained SDK seam scenarios adapted to Rust supervision, real framed IPC, worker/service fault injection and context-generation rejection. **A coverage:** A02/A03/A09/A10/A12/A15/A16 foundations. **Excluded:** loop reimplementation, fixtures as full provider conformance and test-provider-only release scope.

<a id="imp-009"></a>
### IMP-009 — Freeze, verify, approve and integrate an exact Change Set

**Outcome:** the human can freeze a candidate, examine real evidence and authorize one safe squash integration, or explicitly return it for revision.

**Scope:** writer exclusion, candidate manifests, isolated verification context, target refresh, approval/override identity and protected integration/Cleanup journals. Review a concrete target checkout/index/ref sequencing and recovery protocol before any real target mutation. Preserve original worktrees on uncertainty.

**Acceptance:** blocked/missing/stale/failed checks are never a green pass. Approved candidate/target changes expire approval/override; moving target requires isolated refresh/rechecks/reapproval even without textual conflict. Dirty target, merge/rebase, conflict and unauthorized caller leave target untouched. Ref compare-and-set alone does not establish checkout consistency. Return-for-revision invalidates authority before mutations and preserves history; subsequent send failure is visible. Cleanup requires explicit data-loss handling and cannot delete retained context.

**Evidence:** disposable Git repositories, malicious config/helper cases, all mutation crash windows and exact before/after refs/trees/index/worktree assertions. **A coverage:** A05/A06/A07/A08/A10/A11/A20/A24. **Excluded:** auto-merge, automatic stash, blind uncertain deletion and agent-written conflict resolution directly into target.

<a id="imp-010"></a>
### IMP-010 — Demonstrate the complete one-Objective collaboration loop

**Outcome:** the approved native UI drives a real Objective from intent through inspection, correction, review and integration without synthetic product authority.

**Scope:** integrate M1 components, two-view candidate comparison, independent search, unchanged commented files, feedback resolution presentation and the ordinary non-change completion path. Run the one-worker reference performance trace and real durable-save/queue evidence.

**Acceptance:** a selected correction leads to a revision that the human can compare without trusting the final agent message. A target refresh is visibly distinguished from feedback edits. Exercise close/reconnect, stale reference, save failure, service restart and returned-but-not-submitted cases in the native UI. Meet targets or retain failures and obtain explicit adjustments; never silently lower compatibility or hide memory of helper jobs. Native presentation latency is measured or explicitly unverified, not replaced with frame callbacks.

**Evidence:** end-to-end trace tied to commit/toolchain, native captures and A-row observations, one-worker performance report. **A coverage:** A01–A10/A17/A18/A19/A20/A24. **Excluded:** declaration of release readiness and removal of unfinished full-provider scope.

<a id="imp-011"></a>
### IMP-011 — Complete pi session, resource, extension and provider compatibility

**Outcome:** the accepted pinned pi workflow is usable through LADE beyond the first deterministic-provider slice.

**Scope:** tree/fork/clone/import/export, resume, resources/skills/templates and precedence, model/thinking selection, retry/compaction integration, manual reload, semantic extension UI and all accepted provider/API-key/subscription/custom-provider flows. Keep profile separation and original source profiles untouched.

**Acceptance:** compatibility inventory maps every #6 promise to observed support, explicit trust-rule restrictions or unresolved failure. Imports preserve metadata but never restore approvals/Git state; tree navigation does not rewind files. Resource expansion is not applied twice; changed executable resources require approval. Refresh/replacement/late UI callbacks preserve generations. No silent automatic confirmation for unsupported required UI and no API-key-only reduction.

**Evidence:** seam-level deterministic corpus plus authorized live transport/auth checks where required, failure records and pinned compatibility manifest. **A coverage:** A03/A09/A10/A14/A15/A16. **Excluded:** exhaustive recertification of upstream reasoning/compaction internals and graphical package management.

<a id="imp-012"></a>
### IMP-012 — Add the contained terminal and desktop lifetime/attention flows

**Outcome:** supporting terminal access and desktop integration do not bypass Objective scope or interrupt the human unexpectedly.

**Scope:** service-owned PTY/process groups, GTK terminal renderer, bounded output, resize/cancel; contextual navigation, notifications, single-client activation and explicit close/Exit/logout behavior.

**Acceptance:** the GUI cannot spawn a shell outside Rust supervision; terminal descendants obey quotas, grants and review writer exclusion. Both keyboard escape directions work, output floods stay bounded, no unbounded diagnostic copy. Window close preserves authorized background work; Exit and logout safely stop according to policy, without lingering or automatic resume. Notification failure leaves in-app attention intact; clicks navigate without approving. Second client cannot establish another approval authority.

**Evidence:** PTY/focus/Orca tests, detached descendants, terminal flood, close/logout/restart and missing desktop facilities on supported host. **A coverage:** A10–A14/A17/A18/A19/A21/A24. **Excluded:** a terminal-first product and editor embedding.

<a id="imp-013"></a>
### IMP-013 — Harden concurrent Objectives, recovery and bounded load

**Outcome:** independent Objectives remain isolated and usable with real concurrent workers, integration contention and prolonged activity.

**Scope:** one/four-worker traces, full SDK/provider matrix completion, cross-Objective grants/Git isolation, slow-client resync, operation recovery, aggregate quotas and storage pressure. Complete outstanding adversarial cases rather than treating initial smoke tests as sufficient.

**Acceptance:** same-target integrations serialize and invalidate stale review correctly; branches and refs do not cross scopes. No worker/terminal survives authority loss. Bounded queues/spools and durable-history retention hold under disk pressure. Run the exact canonical repeated load, input/file-open/launch and 30-minute soak procedures; report client/service/worker/jobs separately and all-owned totals. Failed/unverified targets remain visible; four is not an enforced concurrency cap.

**Evidence:** measured distributions and timelines, crash/authority corpus, cross-Objective failure traces and memory-growth analysis. **A coverage:** A07/A09–A21/A24; all applicable earlier safety regressions. **Excluded:** silent policy waivers and adding distributed orchestration.

<a id="imp-014"></a>
### IMP-014 — Install, update and roll back a signed coordinated bundle

**Outcome:** a clean supported machine can install and maintain the actual matched client/service/worker bundle without damaging user work.

**Scope:** component pinning/notices, signing/trust bootstrap, scoped prerequisites, user service/desktop integration, manual coordinated update, migration journal, protected-window rollback and data-preserving uninstall. Apply diagnostics policy to stderr, system journal and crash-dump paths as well as application logs.

**Acceptance:** tampered/wrong-key/mismatched-version packages reject before activation. Missing isolation/quota/keyring prerequisites have the approved fail-closed or explicit memory-only behavior. Interrupted migration, disk-full, corrupt state and downgrade outside the rollback window preserve both failed and new state as required. No independent pi update, hidden network telemetry, plaintext fallback or automatic host-security disablement. Inventory actual shipped native/transitive artifacts, not obsolete TODE components.

**Evidence:** clean-host install/update/fault/uninstall records, signed manifests, component hashes, license inventory and export/redaction tests. **A coverage:** A10/A14/A16/A21/A22/A24; installed repetition of relevant native/security gates. **Excluded:** support beyond the accepted platform and automatic data deletion.

<a id="imp-015"></a>
### IMP-015 — Validate developer comprehension with a seeded-defect exercise

**Outcome:** demonstrate that the human understands and can correct work, not just that the interface can dispatch it.

**Scope:** comparable current-workflow and LADE tasks, interruption, seeded semantic defect, correction and reinspection. Establish the baseline before proposing numerical improvement targets. Use disposable fixtures, disclose the seed during debrief and keep it out of real work.

**Acceptance:** record correct acceptance, explanation of effects/verification limits, context reconstruction and correction iterations. A missed defect or false acceptance is investigated, corrected and repeated; speed does not mask failure. Record learning/order limitations, not a claim of universal productivity improvement.

**Evidence:** task definitions, baseline, observation protocol, debrief and explicit Matias acceptance. **A coverage:** A01/A23 with A02–A08 supporting flows. **Excluded:** invented speedup thresholds, agent-count/line-count success metrics and unapproved telemetry.

<a id="imp-016"></a>
### IMP-016 — Assemble installed release evidence and obtain go/no-go

**Outcome:** one traceable release candidate satisfies every applicable accepted obligation or is explicitly blocked.

**Scope:** consolidate A01–A24 evidence at the exact signed candidate, rerun critical installed flows and confirm backlog completion/dependency state. Distinguish documentation, fixture, integrated and installed evidence. Record remaining limitations and any separately approved contract revisions.

**Acceptance:** all applicable rows have evidence, responsible driver and disposition; no security, durability, accessibility, provider or comprehension failure is waived by a passing happy path. Budgets use the accepted method, not prototype proxies. Matias records release go/no-go. No issue closure is interpreted as proof without its artifacts.

**Evidence:** release manifest, acceptance report linked to all source commits/runs, supported-host records and explicit decision. **A coverage:** A01–A24. **Excluded:** autonomous release, remote deployment or self-granted authorization.

## 6. Coverage crosswalk

This is a navigation index, not a replacement acceptance matrix. IMP-016 verifies **every** row on the installed release; the table lists the main implementation owners and supporting issues. Initial fixtures never count as the complete obligation.

| Accepted gate | Main implementation evidence |
| --- | --- |
| A01 Intent/understanding | IMP-002, IMP-004, IMP-010, IMP-015 |
| A02 Valid feedback | IMP-004, IMP-005, IMP-008, IMP-010 |
| A03 Stale context/routing | IMP-004, IMP-005, IMP-008, IMP-011 |
| A04 Draft/submission durability | IMP-003, IMP-004, IMP-005, IMP-010 |
| A05 Frozen return | IMP-009, IMP-010 |
| A06 Revision comparison | IMP-004, IMP-009, IMP-010 |
| A07 Target/checkout safety | IMP-006, IMP-009, IMP-013 |
| A08 Verification provenance | IMP-009, IMP-010 |
| A09 Input crash phases | IMP-003, IMP-005, IMP-008, IMP-013 |
| A10 Recovery/authority expiry | IMP-003, IMP-006, IMP-008, IMP-009, IMP-012, IMP-013, IMP-014 |
| A11 Filesystem/Git containment | IMP-006, IMP-009, IMP-013 |
| A12 Forged authority | IMP-001, IMP-003, IMP-006, IMP-007, IMP-008, IMP-013 |
| A13 Descendants/revocation | IMP-006, IMP-007, IMP-012, IMP-013 |
| A14 Network/trust/credentials | IMP-007, IMP-011, IMP-013, IMP-014 |
| A15 SDK conformance | IMP-001, IMP-008, IMP-011, IMP-013 |
| A16 Full provider/auth | IMP-007, IMP-011, IMP-013, IMP-014 |
| A17 Keyboard/focus | IMP-002, IMP-010, IMP-012, IMP-013 |
| A18 Orca/contrast/scaling | IMP-002, IMP-010, IMP-012, IMP-013 |
| A19 Load/backpressure | IMP-001, IMP-004, IMP-008, IMP-010, IMP-012, IMP-013 |
| A20 End actions/integration | IMP-006, IMP-009, IMP-010 |
| A21 Platform/lifetime/attention | IMP-003, IMP-012, IMP-014 |
| A22 Distribution/update | IMP-001, IMP-014 |
| A23 Product comprehension | IMP-015 |
| A24 Diagnostics/retention | IMP-003, IMP-004, IMP-005, IMP-006, IMP-010, IMP-013, IMP-014 |

## 7. Common Definition of Done and change control

Every implementation PR states the stable issue key, exact scope, source decision/contract and covered A-rows. Retain reproducible fixtures, component versions and commands actually run. Tests must cover the primary path and the relevant rejection/interruption path, not just snapshots of successful UI.

No unrelated edits, real credentials, personal source text or raw provider transcripts in evidence. Diagnostics redaction and artifact permissions apply to testing output too. A failed prerequisite produces an explicit blocked state. Document unexecuted tests; never upgrade a fixture result into installed conformance.

Update the owning contract when its implementation changes; do not fork another spec into an issue comment. UI changes update visual tokens centrally and retain actual native captures. New durable stores, frameworks, security exceptions, APIs or dependencies need a concrete reason and review, not a generic claim of cleanliness. Product-policy changes require Matias; a supported implementation detail can be accepted in its owning technical PR.

Execution map #21 holds the real issue mapping and native dependency links. Keep that mapping there rather than adding a second separately maintained backlog to this document. Before choosing work, read the current issue and dependencies. Further splitting is allowed only for a genuinely reviewable result with explicit acceptance; do not create a task for every folder.

## 8. Handoff and next authorized action

This spec, the architecture and the proposed visual tokens have been reviewed, and the execution backlog is published in #21. Publishing or updating the backlog does not start production work.

Once production implementation is separately authorized, assign a driver to IMP-001 and then IMP-002/003 as their prerequisites permit. No extra broad decision-stage prototype, pi-internals certification project or microservice design phase is required. The hard-limit and containment gates still precede any untrusted execution.

This specification is deliberately executable in increments: initial contracts and native component validation first, one complete collaboration loop next, full compatibility/concurrency and installed release afterward. A later issue's open engineering detail cannot silently relax an accepted contract; resolve it at its named boundary before the corresponding code performs effects.
