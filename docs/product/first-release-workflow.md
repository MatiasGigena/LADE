# First-release collaboration workflow

Status: approved product requirements and derivations of existing contracts, with technical proposals explicitly labeled below. No implemented capability is claimed. See the [vision](vision.md), [UI principles](ui-principles.md), and [readiness gates](../planning/implementation-readiness.md).

## Authority and lifecycle sources

- [#6: pi compatibility](https://github.com/MatiasGigena/LADE/issues/6#issuecomment-5669021069) and [#7: component boundaries](https://github.com/MatiasGigena/LADE/issues/7#issuecomment-5669405012).
- [#8: permissions and enforcement](https://github.com/MatiasGigena/LADE/issues/8#issuecomment-5690106617).
- [#10: Objective and Git lifecycle](https://github.com/MatiasGigena/LADE/issues/10#issuecomment-5619982976) and [#11: durable state and recovery](https://github.com/MatiasGigena/LADE/issues/11#issuecomment-5690240957).
- [Editor-free revision](https://github.com/MatiasGigena/LADE/issues/13#issuecomment-5691260255).

**Objective → agent work → inspection → Contextual Feedback → agent revision → comparison and verification → approval → explicit integration** is a collaboration journey, not a new state machine. Steps can repeat; inspection and discussion remain available during work. It is neither automatic nor irreversible. The contracts below instantiate #10/#11 rather than replace them.

Rust alone owns product identities, operations, permission decisions and lifecycle transitions. The client requests actions and renders authoritative state. Pi owns Agent Session/conversation behavior behind the pinned SDK adapter. One foreground Agent Session is active per Objective; multiple Objectives can execute concurrently in separate worktrees.

## 1. Define and pursue the Objective

Show the expected outcome, constraints and acceptance criteria in a compact, editable statement/discussion. Agent proposals must remain distinguishable from developer-confirmed scope. Scope changes require explicit developer agreement; a progress summary cannot silently replace the original intent. Exact intent-version storage belongs to the schema proposal, not a new mandatory form.

Discussion can precede branch/worktree creation. At first repository execution, Rust provisions the isolated Objective branch/worktree; failure never falls back to the shared checkout. The starting commit and Integration Target are separate, explicit choices. Do not implicitly pull or inherit uncommitted work. The full Change Set includes inherited changes absent from the target. No implicit stacked-branch tracking or rebasing is added.

## 2. Inspect without stopping work

Read-only Inspection supports changed-file lists, search, file opening, surrounding diff context and complete files, including tests and related code that the agent did not cite. Search results identify the searched version/scope; following a result must not silently jump into another revision. Unsupported/binary/oversized content must be identified honestly, with a bounded viewing strategy rather than an apparently complete truncated file. Rendering repository content does not authorize executing it or expanding filesystem permissions.

Always show the Objective, inspected version, working-versus-frozen context, and Integration Target/commit used for the comparison. Distinguish:

- **Working inspection:** a captured view of ongoing work, potentially becoming stale. Opening a file neither freezes writers nor starts review. Do not present a multi-file live read as an atomic candidate. Indicate updates and let the developer refresh without replacing an active selection/draft invisibly.
- **Frozen review:** only the developer explicitly requests it. Rust excludes all relevant writers, reconciles actual contents, and preserves an exact candidate tied to a target commit. Paused tools or a read-only UI alone are insufficient. Unexpected changes invalidate review authority and require reconciliation.

Read-only browsing of an already identified immutable commit/artifact need not start agent execution. Any repository execution still obeys #10's provisioning boundary; no mutable shared-checkout fallback is introduced.

## 3. Minimum versioned reference contract

The **requirement** is a durable, unambiguous reference to what was actually inspected, including uncommitted work. Line numbers alone are insufficient. The following is a **logical schema proposal for #13/#14**, to be ratified before the storage/API increment; field spelling, encoding, digest algorithm and snapshot capture protocol are not selected or existing capabilities.

| Reference element | Minimum meaning |
| --- | --- |
| Ownership | Workspace and Objective identities; destination Agent Session identity and association generation. An execution generation is separately checked at dispatch, not confused with a session. |
| Inspected version | Immutable inspection artifact identity; working capture or frozen candidate; source commit when available, plus captured content identities for uncommitted bytes. A branch name or `HEAD` alone is not a version. |
| Comparison context | Integration Target identity and exact commit; for a diff, explicit before/after side and both content identities. |
| Location | Repository-relative path(s), file kind, selected range within those exact bytes, and retained selected text/context. Rename/deletion and an absent side are representable; line/column is a display hint. |
| Traceability | Feedback item/batch identity, author, capture time, requested correction and source reference; superseding/re-anchored references preserve provenance. |

Proposed implementation direction: Rust-owned immutable content artifacts plus a manifest referencing Git objects where adequate and separate captured bytes otherwise. A working file capture proves only those bytes; a whole candidate requires writer exclusion and a consistent manifest. Finalize handling of modes, symlinks, renames, deletions, untracked/binary files and storage failures in the capture protocol. Do not create development commits merely to make every comment addressable. Retain referenced artifacts with the relevant draft/accepted history; Cleanup must not break accepted review evidence.

## 4. Feedback: selection is not submission

These are **input/operation phases**, not Objective lifecycle states:

1. **Selection:** identifies inspected material; no execution or approval.
2. **Draft:** editable correction attached to that reference. Multiple comments can be collected into one review request; no worker dispatch from typing, selection or autosave.
3. **Explicit submission request:** the developer reviews the destination, references, batch and delivery mode. Submission may still be rejected or its acknowledgement lost.
4. **Durable acceptance:** Rust persists immutable accepted input and references before acknowledging acceptance or dispatch. A saved draft is not accepted input; pi transcript persistence is not Rust acceptance.
5. **Dispatch:** Rust revalidates association, execution authority, context and lifecycle before handing input to the selected SDK worker. Show queued/dispatched/blocked/uncertain delivery separately from acceptance.
6. **Result:** correlated worker activity or failure, then an agent-proposed resolution and developer inspection/confirmation. Dispatch, settled turn, and a claim of resolution are not proof that a correction is right.

An accepted batch is immutable history. Later correction creates a linked input, not a rewrite. UI labels must distinguish received input, processing, proposed resolution and human-confirmed resolution. Human confirmation of a comment is still not integration approval.

### Changed context and routing

Revalidate at submission and again at dispatch, not only when the draft opens. For v1 prefer explicit stale-context detection to semantic re-anchoring:

| Change | Required behavior |
| --- | --- |
| File/range changes, moves or disappears | Preserve original reference and text; show stale/missing context and old/current material where available. Block normal dispatch until the developer inspects and explicitly re-anchors, removes the item, or uses an explicit historical-context route if later approved. No automatic semantic relocation is promised. |
| Batch has a stale item | Proposed minimum: keep the batch intact and blocked, identify affected items; developer edits the batch before resubmission. Never silently drop comments or dispatch only a subset. |
| Destination session changes, forks, clones or is replaced | Preserve original destination. No silent redirection to the active session. Developer explicitly selects/confirms a new destination; record a linked replacement and revalidate every reference. Transcript branching does not rewind files. |
| Execution generation ends/restarts | Historical session identity does not revive grants. Dispatch waits for explicit valid continuation in Ask mode; reconcile accepted input first. |
| Integration Target changes or advances | Preserve the old comparison basis, mark it stale, recompute the total Change Set and request reconfirmation of feedback in that context. Review approval/verification overrides expire; #10's refresh/recheck/reapproval rules apply. |
| Context changes after dispatch | Do not pretend to retract already delivered input or instantly cancel tools. Display the old reference and changed context; worker adapter reports the discrepancy, and further clarification is a new explicit input. Inspection catches any incorrect application. |

A historical-context dispatch option is **deferred**, not a v1 escape hatch. The v1 proposal blocks and asks for re-anchoring; strict invalidation can produce false-stale cases but avoids silently applying a correction to a different version.

### Bounded draft persistence proposal

**New policy proposal, not an already approved #11 guarantee:** persist drafts through Rust into owned durable storage, distinct from accepted inputs. Track draft identity/revision, Objective/session/reference, content, and save acknowledgement. The client may retain unsaved text in memory but must not invent authoritative acceptance or a second durable product store.

- Display Saving / Saved / Save failed / Disconnected. Saved means the acknowledged draft revision is durable, not sent. No fixed save/checkpoint lag is approved; #13 must measure it and choose a bound.
- Disconnection allows local in-memory edits with a clear not-saved warning; submission is unavailable. On reconnect, reconcile the draft revision before saving; never overwrite a newer durable revision silently. No guaranteed offline crash recovery is promised for unacknowledged edits.
- Restore the last durable draft after reopening/restart, including stale/missing references and the original destination. Do not autosend restored drafts. Accepted input must not reappear as a fresh unsent batch after an acknowledgement was lost: query by input/operation identity first.
- Proposed safe-close rule: flush pending drafts before deliberate close/Exit LADE; on storage failure block normal disposal and offer retry or explicit discard/force-exit loss confirmation. A sudden crash can lose edits since the last acknowledged save. This is feedback text protection, not revival of TODE dirty-buffer requirements.
- Keep saved drafts until explicit deletion or successful submission; submission atomically associates the draft revision with accepted input in Rust's store, with referenced artifacts made durable first. Preserve accepted input/history under #11. Cleanup must account for unsent drafts and retain their referenced context or require an explicit disposition; it is not draft deletion.

#14 must ratify this bounded policy (including deliberate-discard behavior), and #13 must test the interactions. Exact SQLite/artifact commit ordering and fault recovery remain a protocol gate. Storage errors never count as accepted input.

## 5. Agent revision and comparison

Preserve pi steering, follow-up and abort semantics; do not redesign its loop. Steering is delivered at the pinned pi boundary after current-turn tools finish, not an instantaneous cancellation. Follow-up waits for tools/steering to finish. A stop/abort control is separate; queue clearing and abort ordering must match pinned pi behavior. The [research](../research/pi-headless-integration.md) documents these distinctions but its RPC probe does not validate the selected SDK adapter under active execution.

For frozen work, **Return for revision** is an explicit developer action enforced by Rust before any mutating continuation. It invalidates prior approval/override, preserves the old candidate and evidence as historical, then permits an explicitly requested revision under valid execution authority. A feedback submission cannot mutate the frozen candidate in place. Propose a confirmed combined UI action (return plus submit) only if its two effects and failure phases are visible; separate controls are sufficient for v1.

Minimum comparison proposal, not a universal review platform:

- **Total Change Set:** current candidate versus its exact Integration Target commit, including inherited changes.
- **Since previous review:** two retained candidate manifests, changed-file list and textual before/after diffs for affected files, additions/deletions, with links from feedback to its old context and current material. Show unchanged-but-commented files too. No inferred semantic resolution or mandatory per-comment patch attribution.
- If the target changed between candidates, show both target commits and warn that the revision delta includes target refresh effects; never attribute every line to feedback.
- If no prior candidate exists, say so. Retain accepted input context for comparison; do not fabricate a prior frozen review.

Verification evidence identifies the command, arguments, working context, execution/operation identity, candidate/content version, relevant environment, start/end, exit/result, output artifact and truncation or capture limitations. Checks may run only inside valid execution scope and must not mutate the frozen candidate: use an isolated verification copy or detect changes and invalidate it. The exact verified-copy protocol remains a technical proposal.

Rust-observed process exit/output is different from agent prose and from a claim about code correctness. A worker-supplied result must show its provenance and must not masquerade as independently observed trusted verification. Missing, failed, interrupted and stale results remain visible; coverage is separate metadata, not success. Required checks that fail block by default; missing/stale checks remain unverified. #10 permits a deliberate recorded reason to override failed/missing verification, never to bypass identity, writer exclusion, target safety or separate integration authority. Changes invalidate approval and overrides; refreshed candidates require checks again.

## 6. Transition and failure contracts

Developer actions use the controlling client; Rust validates authority and preconditions. Worker messages may report/request, never approve. The table is a projection of #10/#11 plus the input proposals above, not a replacement state enumeration.

| Action / authorized actor | Preconditions | Effects | Invalidations | Failure / recovery |
| --- | --- | --- | --- | --- |
| Define/change intent / developer; agent may propose | Correct Objective; durable input path | Record agreed outcome/constraints/criteria | Reassess acceptance for changed scope; intent-version binding is proposed | Save failure leaves prior scope authoritative; no hidden scope replacement |
| Start repository execution / Rust on developer-authorized work | Isolated worktree, valid execution identity, enforced grants | Launch SDK worker/tools in Objective scope | No review approval carries into renewed mutation | Provision/enforcement failure blocks; no shared-checkout or unsandboxed fallback |
| Inspect/select/draft / developer | Authorized read of identified context | Presentation, then separately saved draft if requested | Does not freeze or approve; changing work marks references stale | Show capture/save failure or unavailable context; never manufacture a snapshot |
| Submit feedback / developer, accepted by Rust | Explicit batch/destination/mode, current references, durable stores | Accepted input, then separate dispatch operation | No permission or integration approval granted | Rejection retains draft; lost acknowledgement queries outcome; uncertain dispatch is not blindly retried |
| Signal readiness / worker | Valid bound session/execution | Readiness signal; derived attention | No freeze, completion or approval | Ignore stale/forged messages; preserve true current state |
| Start frozen review / developer, executed by Rust | Stop/exclude all writers; reconcile worktree/target | Preserve exact candidate and verification context | Working-view assumptions replaced by identified candidate | Failed stop/capture cannot yield frozen/approved state; unexpected writes require attention |
| Return for revision / developer | Identified frozen candidate; durable transition | Preserve candidate/history; invalidate authority before mutations resume | Approval and verification override; old evidence is historical for new work | If submission fails after return, show returned-but-no-input, not a successful request; no automatic integration |
| Record verification / authorized execution observed by Rust | Candidate/copy and command identity, granted scope | Record exact result/provenance | Content/context mismatch makes evidence stale | Missing/failed/interrupted is never pass; changed candidate requires review again |
| Approve / developer only | Exact frozen candidate and target; current evidence or deliberate permitted override | Durable approval bound to both, plus reason if overridden | Invalid on candidate/target changes, resumed edits, or service restart | Storage/auth/state mismatch rejects approval; worker booleans carry no authority |
| Integrate / separate explicit developer action, Rust Git operation | Valid exact approval; target unchanged; clean target checkout, no merge/rebase; writers excluded | Journal protected operation; squash commit; proven success completes Objective | Consume approval; subsequent work is a new linked Objective | Moving target: isolated refresh, recorded checks rerun, fresh approval even without conflicts. Conflict leaves target untouched and returns for revision. Uncertain crash: reconcile actual Git, no duplicate integration |
| Complete without repository changes / developer | No changes to integrate; validated current state | Complete Objective without Git mutation | No later integrations into completed Objective | Failure preserves prior state, not a synthetic successful integration |
| Cancellation / developer, stopped by Rust | Identified Objective/execution; stop descendants | Reversible stop, preserve work/session | End execution lifetime, expire extra grants/YOLO; invalidate authority to resume mutations | Report incomplete stop; do not call running descendants canceled; explicit Ask-mode resumption |
| Discard / developer confirmation | Correct Objective; abandonment explicit | Abandon without integration; stop owned execution | Cannot retain executable/integration authority | Preserve resources/history on uncertain outcome; not Cleanup |
| Cleanup / explicit developer action through Rust | Completed/discarded; processes stopped, terminal detached; data-loss confirmation where needed | Remove Objective branch/worktree, retain accepted records/artifacts | Resources no longer executable; not history deletion | Journal/reconcile deletion; never blindly continue uncertain removal; preserve unrelated work |
| Disconnect / service reconnect; restart / supervisor + Rust | Match components; query authoritative snapshot/operations | Client-only loss permits background work; service loss stops descendants and reconciles | Restart expires prompts, extra grants, YOLO and unconsumed approvals | No blind effect replay, automatic worker resume or expired-approval restoration; explicit Ask-mode continuation |

## 7. Attention and scope

Retain #10 board precedence: **Needs attention → Ready for review → Working → Idle**, with secondary badges (including canceled/stopped) and separate completed/discarded archives. These are system-derived categories, not draggable stages. Opening an Objective, notification, file or review content never authorizes freezing or integration. The Objective sidebar complements the Workspace board.

The [acceptance matrix](../planning/implementation-readiness.md#acceptance-matrix) carries valid/stale feedback, draft/storage failures, return-for-revision, advancing targets, absent/failed/stale verification, crash windows and adversarial authority scenarios. Defer semantic re-anchoring, universal review threads, arbitrary revision graphs and IDE-level navigation; do not defer the existing security, pi compatibility or recovery contract to make a demo appear complete.
