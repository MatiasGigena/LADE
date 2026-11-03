# Selected graphical client: GTK4 native

**Status: approved decision, not production authorization.** The maintainer explicitly selected **“Yes—select GTK4 native”** after reviewing the minimal comparison and styling implications. [Accepted decision in #13](https://github.com/MatiasGigena/LADE/issues/13#issuecomment-5698434718). This supersedes the earlier provisional recommendation, not its evidence limitations.

## Decision

- Use **GTK4 native widgets/layout** for LADE's primary graphical client. Tauri/WebKitGTK and Electron are not selected as the primary client architecture.
- Preserve the approved [workbench composition and visual direction](../product/ui-principles.md), and the accepted [inspection/feedback interaction](https://github.com/MatiasGigena/LADE/issues/13#issuecomment-5696881631), including global search beside the profile. The benchmark miniature is not the design baseline or a template to promote into production.
- GTK CSS and GTK layout APIs can express the intended neutral colors, typography, spacing, density, boundaries and focus states. GTK CSS is **not browser CSS**; highly customized controls may require additional implementation. Prefer accessible native primitives over custom-drawing the entire interface. Rich code/diff presentation still requires a concrete component choice and validation.
- This selects the toolkit/native approach, **not C as the production client language**, a particular Rust binding, Libadwaita, GtkSourceView, an exact GTK shipping version, or a custom rendering framework. The C host was deliberately minimal decision instrumentation.
- First release remains editor-free: Read-only Inspection, Contextual Feedback and Change Set review, with no embedded editor or new external-editor integration commitment.

## Ownership remains unchanged

[#7](https://github.com/MatiasGigena/LADE/issues/7#issuecomment-5669405012) still governs the process arrangement: one authoritative per-user **Rust System Layer**, one SDK-based Node worker per active Agent Session, and a controlling graphical client that presents state and requests actions. GTK4 selection does not move authority into the client.

**Pi owns agent behavior**, including its loop, tools, compaction and Agent Session semantics, behind the pinned adapter selected by [#6](https://github.com/MatiasGigena/LADE/issues/6#issuecomment-5669021069). LADE adapts those seams, not reimplements pi. Rust-owned host restrictions, durable acceptance, Git operations and recovery remain their accepted contracts.

Do not expand architecture planning into exhaustively retesting pi internals or building/certifying the entire authority layer. Further decision-stage probes must address a **concrete integration risk that could change a decision**. Full compatibility, security, recovery, provider/auth and installation validation remain explicit implementation/release obligations; this scope clarification does not waive them.

## Rationale and evidence boundary

The [published minimal GTK4/Tauri comparison report](https://github.com/MatiasGigena/LADE/issues/13#issuecomment-5698278583) retained three foreground runs per candidate on Mint/Cinnamon/X11. Median settled PSS was approximately **76 MiB for GTK4 versus 219 MiB for Tauri**; GTK4 also had lower stream/typing CPU and faster launch-to-ready in that fixture. This supports the resource-oriented choice.

Retain the tradeoffs: Tauri had steadier recorded input scheduling proxies and lower post-workload idle CPU. Those proxies are not actual compositor presentation latency. The clients had different C/Rust host implementations, plain text rather than a rich diff widget, no real service/workers, and only a small AT-SPI/typing check—not Orca speech or full accessibility. Future production costs cannot be copied from the miniature. No prototype source is promoted into the application by this decision.

The earlier [SDK seam](https://github.com/MatiasGigena/LADE/issues/13#issuecomment-5697152531) and [semantic extension UI](https://github.com/MatiasGigena/LADE/issues/13#issuecomment-5697370541) findings remain adapter handoff evidence, not complete product conformance.

## Open and exact next step

[#13's planning decision is resolved](https://github.com/MatiasGigena/LADE/issues/13#issuecomment-5698708894). After selecting GTK4, the maintainer explicitly approved the [provisional engineering targets and I/R evidence handoff](performance-budgets.md). The service/worker allocations, actual presentation and complete accessibility remain unverified; target acceptance is not production authorization.

**#14's release-boundary handoff is now accepted**, not another client bake-off. The [published resolution](https://github.com/MatiasGigena/LADE/issues/14#issuecomment-5699037043) ratifies exact context retention, strict-stale batches, draft/save/close behavior, minimum comparison, diagnostics/storage/resource policies, comprehension evaluation and all [implementation/release gate ownership](implementation-readiness.md). The maintainer approved closing #14 and the #1 decision map without authorizing production implementation.

Next is separately authorized implementation planning: instantiate concrete protocols/defaults before each code boundary, including approved and enforced hard limits before untrusted execution. The first GTK4 component increment must establish native text/diff, keyboard/Orca/scale feasibility before committing to the rich control; integrated/load/installed validation follows at its explicit boundary. No runtime gate is passed by planning closure.

A future implementation slice should be small and follow existing domain/workflow boundaries. It is not authorized merely because the toolkit is now selected.
