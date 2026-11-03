# First-release collaboration workflow

Status: approved product requirements, the release boundary's accepted feedback/reference/draft/comparison policies and derivations of existing contracts, with remaining technical proposals explicitly labeled below. No implemented capability is claimed. See the [vision](vision.md), [UI principles](ui-principles.md), and [readiness gates](../planning/implementation-readiness.md).

## Authority and lifecycle sources

- pi compatibility and component boundaries.
- Permissions and enforcement.
- Objective and Git lifecycle, and durable state and recovery.
- Editor-free revision.

**Objective → agent work → inspection → Contextual Feedback → agent revision → comparison and verification → approval → explicit integration** is a collaboration journey, not a new state machine. Steps can repeat; inspection and discussion remain available during work. It is neither automatic nor irreversible. The contracts below instantiate the lifecycle and recovery contracts rather than replace them.

Rust alone owns product identities, operations, permission decisions and lifecycle transitions. The client requests actions and renders authoritative state. Pi owns Agent Session/conversation behavior behind the pinned SDK adapter. One foreground Agent Session is active per Objective; multiple Objectives can execute concurrently in separate worktrees.

## 1. Define and pursue the Objective

Show the expected outcome, constraints and acceptance criteria in a compact, editable statement/discussion. Agent proposals must remain distinguishable from developer-confirmed scope. Scope changes require explicit developer agreement; a progress summary cannot silently replace the original intent. Exact intent-version storage belongs to the schema proposal, not a new mandatory form.

Discussion can precede branch/worktree creation. At first repository execution, Rust provisions the isolated Objective branch/worktree; failure never falls back to the shared checkout. The starting commit and Integration Target are separate, explicit choices. Do not implicitly pull or inherit uncommitted work. The full Change Set includes inherited changes absent from the target. No implicit stacked-branch tracking or rebasing is added.

## 2. Inspect without stopping work

Read-only Inspection supports changed-file lists, search, file opening, surrounding diff context and complete files, including tests and related code that the agent did not cite. Search results identify the searched version/scope; following a result must not silently jump into another revision. Unsupported/binary/oversized content must be identified honestly, with a bounded viewing strategy rather than an apparently complete truncated file. Rendering repository content does not authorize executing it or expanding filesystem permissions.

Always show the Objective, inspected version, working-versus-frozen context, and Integration Target/commit used for the comparison. Distinguish:

- **Working inspection:** a captured view of ongoing work, potentially becoming stale. Opening a file neither freezes writers nor starts review. Do not present a multi-file live read as an atomic candidate. Indicate updates and let the developer refresh without replacing an active selection/draft invisibly.
- **Frozen review:** only the developer explicitly requests it. Rust excludes all relevant writers, reconciles actual contents, and preserves an exact candidate tied to a target commit. Paused tools or a read-only UI alone are insufficient. Unexpected changes invalidate review authority and require reconciliation.

Read-only browsing of an already identified immutable commit/artifact need not start agent execution. Any repository execution still obeys the lifecycle provisioning boundary; no mutable shared-checkout fallback is introduced.

## 3. Minimum versioned reference contract

The **accepted release-boundary policy** is a durable, unambiguous reference to what was actually inspected, including uncommitted work. Retain exact inspected context with saved drafts and accepted history; Cleanup cannot silently erase it. Line numbers alone are insufficient. The following remains a **logical schema proposal** to instantiate that policy before the storage/API increment; field spelling, encoding, digest algorithm and snapshot capture protocol are not selected or existing capabilities.

| Reference element | Minimum meaning |
| --- | --- |
| Ownership | Workspace and Objective identities; destination Agent Session identity and association generation. An execution generation is separately checked at dispatch, not confused with a session. |
| Inspected version | Immutable inspection artifact identity; working capture or frozen candidate; source commit when available, plus captured content identities for uncommitted bytes. A branch name or `HEAD` alone is not a version. |
| Comparison context | Integration Target identity and exact commit; for a diff, explicit before/after side and both content identities. |
| Location | Repository-relative path(s), file kind, selected range within those exact bytes, and retained selected text/context. Rename/deletion and an absent side are representable; line/column is a display hint. |
| Traceability | Feedback item/batch identity, author, capture time, requested correction and source reference; superseding/re-anchored references preserve provenance. |

Accepted implementation direction: Rust-owned immutable content artifacts plus a manifest referencing Git objects where adequate and separate captured bytes otherwise. This approves the retention/reference policy, not a concrete capture or storage protocol. A working file capture proves only those bytes; a whole candidate requires writer exclusion and a consistent manifest. Finalize handling of modes, symlinks, renames, deletions, untracked/binary files and storage failures in the capture protocol. Do not create development commits merely to make every comment addressable. Retain referenced artifacts with the relevant draft/accepted history; Cleanup must not break accepted review evidence.

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

Revalidate at submission and again at dispatch, not only when the draft opens. **Accepted in the release-boundary policy walkthrough:** any stale feedback item blocks its entire batch until the developer explicitly updates/re-anchors or removes that item. Preserve every comment and its original context; never silently relocate comments or send only the unaffected subset. Automatic re-anchoring and historical-context dispatch remain deferred. Reference retention and draft policy were subsequently accepted separately below; concrete protocols remain to be specified.

| Change | Required behavior |
| --- | --- |
| File/range changes, moves or disappears | Preserve original reference and text; show stale/missing context and old/current material where available. Block normal dispatch until the developer inspects and explicitly re-anchors or removes the item. No automatic semantic relocation is promised. |
| Batch has a stale item | Accepted v1 policy: keep the batch intact and blocked, identify affected items; developer explicitly updates/re-anchors or removes affected items before resubmission. Never silently drop comments or dispatch only a subset. |
| Destination session changes, forks, clones or is replaced | Preserve original destination. No silent redirection to the active session. Developer explicitly selects/confirms a new destination; record a linked replacement and revalidate every reference. Transcript branching does not rewind files. |
| Execution generation ends/restarts | Historical session identity does not revive grants. Dispatch waits for explicit valid continuation in Ask mode; reconcile accepted input first. |
| Integration Target changes or advances | Preserve the old comparison basis, mark it stale, recompute the total Change Set and request reconfirmation of feedback in that context. Review approval/verification overrides expire; the lifecycle refresh/recheck/reapproval rules apply. |
| Context changes after dispatch | Do not pretend to retract already delivered input or instantly cancel tools. Display the old reference and changed context; worker adapter reports the discrepancy, and further clarification is a new explicit input. Inspection catches any incorrect application. |

A historical-context dispatch option is **deferred**, not a v1 escape hatch. The accepted v1 policy blocks and asks for explicit re-anchoring or removal; strict invalidation can produce false-stale cases but avoids silently applying a correction to a different version.

### Bounded draft persistence policy

**Accepted in the release-boundary policy walkthrough, not an implemented durability guarantee:** persist drafts through Rust into owned durable storage, distinct from accepted inputs. Track draft identity/revision, Objective/session/reference, content, and save acknowledgement. The client may retain unsaved text in memory but must not invent authoritative acceptance or a second durable product store.

- Display Saving / Saved / Save failed / Disconnected. Saved means the acknowledged draft revision is durable, not sent. [The accepted timing targets](../planning/performance-budgets.md) request saves after 300ms quiet or at least every 2s during typing, with a healthy-storage durable acknowledgement target ≤1s after request. These are unverified implementation targets, not a crash-loss guarantee. The release boundary subsequently accepted the bounded draft/save/close policy; concrete persistence protocols remain an implementation gate.
- Disconnection allows local in-memory edits with a clear not-saved warning; submission is unavailable. On reconnect, reconcile the draft revision before saving; never overwrite a newer durable revision silently. No guaranteed offline crash recovery is promised for unacknowledged edits.
- Restore the last durable draft after reopening/restart, including stale/missing references and the original destination. Do not autosend restored drafts. Accepted input must not reappear as a fresh unsent batch after an acknowledgement was lost: query by input/operation identity first.
- Accepted safe-close rule: flush pending drafts before deliberate close/Exit LADE; on storage failure block normal disposal and offer retry, stay open, or explicit discard/force-exit loss confirmation. Window close still permits authorized background agent work; Exit LADE retains the separate recovery safe-stop contract. A sudden crash can lose edits since the last acknowledged save. This is feedback text protection, not revival of TODE dirty-buffer requirements.
- Keep saved drafts until explicit deletion or successful submission; submission atomically associates the draft revision with accepted input in Rust's store, with referenced artifacts made durable first. Preserve accepted input/history under the recovery contract. Cleanup must account for unsent drafts and retain their referenced context or require an explicit disposition; it is not draft deletion.

The release-boundary walkthrough accepted save-before-close, explicit confirmation of unsaved-edit loss, Rust-owned separately saved drafts, restore-unsent behavior and the lack of offline crash recovery for unacknowledged edits. The client decision's fixture interaction evidence is retained, not a durable-storage test. Exact SQLite/artifact commit ordering, atomic submission and fault recovery remain protocol/implementation gates. Storage errors never count as accepted input.

## 5. Agent revision and comparison

Preserve pi steering, follow-up and abort semantics; do not redesign its loop. Steering is delivered at the pinned pi boundary after current-turn tools finish, not an instantaneous cancellation. Follow-up waits for tools/steering to finish. A stop/abort control is separate; queue clearing and abort ordering must match pinned pi behavior. The [worker contract](../implementation/worker.md#protocol-mapping) documents these distinctions; remaining conformance gaps are tracked in the [compatibility inventory](../implementation/pi-compatibility.md).

For frozen work, **Return for revision** is an explicit developer action enforced by Rust before any mutating continuation. It invalidates prior approval/override, preserves the old candidate and evidence as historical, then permits an explicitly requested revision under valid execution authority. A feedback submission cannot mutate the frozen candidate in place. Propose a confirmed combined UI action (return plus submit) only if its two effects and failure phases are visible; separate controls are sufficient for v1.

Accepted release-boundary minimum comparison policy, not an implemented capability or a universal review platform:

- **Total Change Set:** current candidate versus its exact Integration Target commit, including inherited changes.
- **Since previous review:** two retained candidate manifests, changed-file list and textual before/after diffs for affected files, additions/deletions, with links from feedback to its old context and current material. Show unchanged-but-commented files too. No inferred semantic resolution or mandatory per-comment patch attribution.
- If the target changed between candidates, show both target commits and warn that the revision delta includes target refresh effects; never attribute every line to feedback.
- If no prior candidate exists, say so. Retain accepted input context for comparison; do not fabricate a prior frozen review.

Verification evidence identifies the command, arguments, working context, execution/operation identity, candidate/content version, relevant environment, start/end, exit/result, output artifact and truncation or capture limitations. Checks may run only inside valid execution scope and must not mutate the frozen candidate: use an isolated verification copy or detect changes and invalidate it. The exact verified-copy protocol remains a technical proposal.

Rust-observed process exit/output is different from agent prose and from a claim about code correctness. A worker-supplied result must show its provenance and must not masquerade as independently observed trusted verification. Missing, failed, interrupted and stale results remain visible; coverage is separate metadata, not success. Required checks that fail block by default; missing/stale checks remain unverified. The lifecycle contract permits a deliberate recorded reason to override failed/missing verification, never to bypass identity, writer exclusion, target safety or separate integration authority. Changes invalidate approval and overrides; refreshed candidates require checks again.

## 6. Transition and failure contracts

Developer actions use the controlling client; Rust validates authority and preconditions. Worker messages may report/request, never approve. The table is a projection of the lifecycle and recovery contracts plus the input proposals above, not a replacement state enumeration.

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

## Planning examples: references, saves and recovery

These are **normative logical examples reviewed for the release boundary, not executed traces or selected wire schemas**. Symbols name identities, not a chosen encoding. Concrete protocol and fault tests precede the affected code boundary.

| Example | Required sequence and visible outcome |
| --- | --- |
| Exact working context | Objective O / session S association generation G inspects artifact W containing uncommitted bytes C at path P, with target T at commit H. A draft references W/C/P and its selected range/context. Further writes produce another capture, never mutate W. Working capture does not prove a whole candidate was atomic. |
| Save and reconnect | Draft D revision 4 requests save; only Rust's durable acknowledgement for revision 4 permits Saved. Revision 5 typed while disconnected stays visibly unsaved. Reconnect queries D's durable revision before writing; a newer durable revision is not silently overwritten. Crash restores the last durable revision unsent after reconciliation; a lost save acknowledgement may conceal a completed write. |
| Stale batch | Batch B contains D1–D3; D2's context changes before acceptance or dispatch. Preserve all items, identify D2 and block the whole batch. Explicitly re-anchor/update or remove D2, then revalidate all references and destination. Already accepted B is immutable; a corrected submission is linked new input, not edited accepted history. |
| Lost acceptance acknowledgement | Submit operation Q identifies an exact draft revision/batch. Required artifacts become durable before Rust atomically records accepted input I and associates the submitted draft revision with it. If the reply is lost, query Q/I before retrying: accepted input must not reappear as a fresh unsent batch or be blindly dispatched twice. Failure before acceptance retains the draft; partial artifact writes do not prove acceptance. |
| Uncertain dispatch or replacement | Rust has accepted I but cannot prove whether worker execution E received it before failure. Show accepted / delivery uncertain; do not resend automatically. A replacement session association or execution generation cannot inherit routing or authority. Require explicit destination confirmation/Ask continuation as applicable, with reconciliation and reference revalidation. |
| Frozen return, then failed send | Candidate F1 was approved against H. Explicit Return for revision durably invalidates approval before mutating continuation. If feedback acceptance then fails, show returned / input not accepted and retain the draft. F1 remains historical; no silent re-freeze or restored approval. |
| Changed target during comparison | F1 used H1; F2 uses H2. Show F2 versus H2 and F1 versus F2, both target identities and target-refresh caveat, including unchanged commented files. Neither agent resolution prose nor this delta proves correction or grants approval. |
| Storage exhaustion or corrupt state | Failed capture/save/acceptance never becomes Saved/accepted/frozen. Preserve prior durable records and unsaved text where possible; block unsafe actions and stop affected execution when required results cannot be recorded. Reclaim only disposable data under the readiness policy; no automatic history deletion/reset. |

## 7. Attention and scope

Retain the lifecycle board precedence: **Needs attention → Ready for review → Working → Idle**, with secondary badges (including canceled/stopped) and separate completed/discarded archives. These are system-derived categories, not draggable stages. Opening an Objective, notification, file or review content never authorizes freezing or integration. The Objective sidebar complements the Workspace board.

The [acceptance matrix](../planning/implementation-readiness.md#acceptance-matrix) carries valid/stale feedback, draft/storage failures, return-for-revision, advancing targets, absent/failed/stale verification, crash windows and adversarial authority scenarios. Defer semantic re-anchoring, universal review threads, arbitrary revision graphs and IDE-level navigation; do not defer the existing security, pi compatibility or recovery contract to make a demo appear complete.
