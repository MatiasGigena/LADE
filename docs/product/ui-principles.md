# UI principles and approved workbench direction

Status: **developer-approved composition and visual direction**, not a toolkit selection, interaction-validation result or technical guarantee. The [workflow](first-release-workflow.md) controls action semantics; illustrative labels or enabled buttons in a mockup cannot override it.

## Reference provenance and missing attachments

The developer identifies the **last attached image** as the approved conceptual workbench; earlier images are historical iterations. In this documentation task neither that image's binary file nor the complete attached **`scandinavian-design` skill (`SKILL.md` and any referenced assets)** was accessible. No file matching that skill was found in the available skill locations, and no identifiable approved image was available in the repository/consulted branches. This document applies the explicit textual rules supplied by the developer; it does **not** claim to have applied unseen skill instructions or inspected the image.

To complete the reference, Matias Gigena supplies the original last approved image and full skill attachment. The documentation maintainer then preserves the original image at `docs/product/assets/lade-approved-workbench.png` (if supplied as PNG; otherwise retain its actual format), adds a relative link/caption identifying it as an **approved conceptual reference, not a running implementation**, and records any skill/text conflicts for resolution. There is deliberately no broken image link or recreated substitute now. This attachment gap does not block the textual specification, but prevents a claim of exact visual-reference matching.

The composition is not a literal specification of code, labels, fixture data or action availability. Do not copy the office, monitor, plants or background text into the application. Coverage is not a passing test, fixture evidence is not a real execution, and Integrate cannot become available without the authoritative prerequisites.

## Composition

| Region | Function |
| --- | --- |
| Header | LADE mark and **LADE** only beside it, navigation (including Workspace overview), search and profile. No “Agentic development environment” subtitle. |
| Left column | Objectives with brief context, selected Objective and system-derived attention/status. No lateral accent stripe or border on Objective cards/rows. |
| Dominant center | Objective intent/discussion when defining work; Read-only Inspection of full files and Change Sets when understanding results; revision comparison when checking corrections. |
| Right panel | Summary, observed evidence and its provenance, Contextual Feedback, review and integration actions with reasons for blocked/unavailable actions. |
| Contextual surfaces | Agent Session, tool activity and terminal in the selected Objective context, accessible on demand without forcing conversation or terminal use for every task. |

The Workspace Objectives board remains the overview; a sidebar does not replace it. Retain [#10](https://github.com/MatiasGigena/LADE/issues/10#issuecomment-5619982976) attention precedence and secondary badges; no draggable stages. The visible **Inspect → Feedback → Agent revision → Review → Integrate** journey explains collaboration, not an automatic pipeline, irreversible wizard or new lifecycle.

This supersedes [#9's original](https://github.com/MatiasGigena/LADE/issues/9#issuecomment-5617994597) mandatory Agent Session-centered dedicated screen and the editor pane, not the board/navigation/attention rules. Conversation remains useful while defining intent and responding to the agent. Do not silently switch the main surface or focus when background work changes state; task-led defaults guide explicit navigation, not unsolicited mode changes.

## Operational Scandinavian-style rules

Apply the developer-specified design direction to an operational Linux desktop workbench, not a landing page:

- White foundation, black ink and **genuinely neutral** grays. No beige, warm, blue-tinted or green-tinted chrome.
- Clear hierarchy, useful information density, consistent spacing and minimal decoration. A single sans-serif UI family; monospace where code/output requires it. Exact font, metrics and spacing scale remain implementation choices subject to legibility and redistribution checks, not selected by this approval.
- Coherent iconography with accessible names; no decorative emojis. Shadows and containers only to communicate grouping, elevation or interaction.
- Do not import oversized editorial margins, decorative uppercase conventions or mobile requirements automatically. LADE remains uppercase as a proper name. Linux desktop scope still requires usable 200% scaling and narrow available logical space.
- Remove motivational copy, greetings, slogans and functionless cards: no “Small steps. Meaningful progress.”, “Good morning, Alex.” or “You’re in control.”
- Removing decoration does **not** remove review comments, draft text, code references, errors, verification results, permission scope or explanations needed to understand an action.
- Semantic colors remain where useful, especially diff additions/deletions, warnings, errors and states, with signs or text labels. Monochrome chrome is not a reason to remove status information or accessibility.

### Initial token references

These are starting references, **not measured passing pairs**. Test compositing over actual surfaces, hover/pressed/selected combinations, text size and rendering; functional text needs sufficient contrast under #17.

| Role | Reference |
| --- | --- |
| Base | White `#FFFFFF` |
| Primary ink | Black `#000000` |
| Secondary ink | Black at 64% opacity |
| Separators | Black at 10% opacity |
| Hover | Black at 5% opacity over the surface |
| Pressed | Black at 9% opacity over the surface |

A 10% separator is not automatically adequate for a control boundary or focus indicator. Selected and keyboard-focus states need independent discernibility. Proposed implementation: neutral selected fill plus weight/state label, and a sufficiently contrasting full focus outline; do not reintroduce a lateral accent bar. Final combinations must pass testing.

### Profile correction

Show only a circular avatar and **Matias Gigena**, vertically centered against it. For a prototype retain its existing avatar placeholder if the real avatar is unavailable; do not invent a photograph or implement GitHub authentication for it. This display does not imply an account, connection or sign-in obligation to use LADE. No applicable image/placeholder asset is newly invented in this documentation branch.

## Interaction and accessibility contracts

Preserve [#17](https://github.com/MatiasGigena/LADE/issues/17#issuecomment-5667531284) except its superseded editor-specific assumptions and mandatory first-visit Agent Session input:

- Conventional, remappable keyboard navigation with conflicts, reset and discoverable shortcut reference. Board groups retain Tab/Shift+Tab between groups, Up/Down and Home/End within them, Enter to open. Where tabs exist, preserve arrow keys and Enter/Space.
- F6/Shift+F6 cycle major regions in both directions: header, Objectives, active central surface, right review/feedback panel, and contextual terminal when open. There must always be a working terminal escape route; remapping cannot remove it. Tab remains local and Escape dismisses local menus/dialogs, not the whole pane unexpectedly.
- Explicit opening moves focus to the requested task; closing returns to the opener or a predictable available fallback. Returning to an Objective restores its selected surface and focus; returning to the board restores its card. System-driven reordering keeps focus on the same Objective or a predictable neighbor/heading if removed.
- **Proposed first-visit task defaults for #13:** intent input during definition, inspected-content heading for inspection, comparison heading for revision review. Explicit Respond/Review goes directly to the relevant content. These refine the former fixed Agent Session-input rule and require keyboard/Orca validation before finalization.
- No background event, attention request, incoming revision or notification steals focus. Announce concise important action/error/attention changes politely, group bursts, permit disabling announcements; do not narrate streaming output automatically. Content remains readable on demand.
- Meaningful names include Objective identity, not just a status; expose roles, selection, draft/save/send status, stale references, errors and action outcomes. Keyboard text/range selection, creating feedback, collecting drafts, explicit sending and comparing revisions must be possible without a mouse.
- Visible unobscured focus, text equivalents for colors, WCAG 2.2 AA contrast thresholds, usable 200% text/UI scaling, and reduced-motion behavior remain the baseline, not an asserted blanket conformance claim. Test LADE-owned core flows with Orca on Mint/Cinnamon/X11; test terminal internals and disclose limitations.
- Keep in-app attention regardless of desktop notifications. Background-only desktop notifications are best-effort, silent by default, grouped by new reason, contain Objective/reason but not code/conversation/secrets, and never grant authority. Activation rechecks state and navigates; it cannot answer, freeze, approve or integrate.

### Required visible states

Show idle/empty/loading and actionable failure states rather than hiding errors in minimal styling. Distinguish working capture, frozen candidate, stale reference, unsent draft, saving/saved/failed save, accepted input, queued/dispatched/uncertain delivery, proposed resolution and human confirmation. Show current permissions, disconnected/recovering/interrupted execution, verification missing/failed/stale, expired approval and integration conflict.

Disabled or unavailable actions retain an accessible explanation and recovery path. Integrate is not a decorative primary button: Rust-authorized availability requires the exact valid candidate/target approval and all other [workflow preconditions](first-release-workflow.md#6-transition-and-failure-contracts). Approval, feedback, execution permission, Cancellation, Discard and Cleanup remain distinct controls/actions.

## Validation boundary

Historical B styling and mock data are not the new design baseline. Its documented accessible-name, reverse-F6, contrast and scaling failures are useful test inputs, not inherited accepted behavior. This task performs no visual rendering, Orca, contrast-on-rendered-surfaces, performance or security validation. The [readiness matrix](../planning/implementation-readiness.md#acceptance-matrix) assigns those tests and their evidence.
