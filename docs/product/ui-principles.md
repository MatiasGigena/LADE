# UI principles and approved workbench direction

Status: **developer-approved composition and visual direction; GTK4 native is now the selected graphical client** ([accepted architecture](../planning/client-architecture.md)). The inspection/feedback prototype, including its global-search correction, is accepted as the interaction baseline—not as full native/accessibility validation. The [workflow](first-release-workflow.md) controls action semantics; illustrative labels or enabled buttons in a mockup cannot override it.

## GTK4 styling direction

Retain the approved design, not the minimal benchmark's styling. GTK CSS and native layout APIs support the intended neutral colors, typography, spacing, density, boundaries and focus states. GTK CSS is not browser CSS; widget structure and highly customized controls may require GTK-specific implementation. Prefer accessible native controls over custom-drawing the interface. Rich file/diff components remain an implementation choice to validate. No pixel-perfect result, particular widget library or full accessibility guarantee follows from toolkit selection.

## Reference provenance and assets

The developer supplied the **last approved image** and the full main `scandinavian-design` skill text in the follow-up to the initial consolidation. Earlier images remain historical iterations. The original PNG is preserved without cropping, recoloring or recreation; the skill text is archived as supplied, not installed as a harness skill or made a parallel LADE specification. This resolves the image/main-text availability gap recorded in the first documentation commit and issue comments.

![Approved conceptual LADE workbench: header, Objectives sidebar, dominant Change Set inspection and right-hand evidence and review panel.](assets/lade-approved-workbench.png)

*Approved conceptual composition/style reference supplied by Matias Gigena, **not a screenshot of a running LADE implementation**. The depicted surroundings are not part of the application.*

- [Original image](assets/lade-approved-workbench.png): PNG, 1672 × 941; SHA-256 `b152c8a6e8cde3720f3a3db5b639bd10c24ad37f4194226f79741414de87ff83`. Repository copy verified byte-for-byte against the supplied file.
- [Supplied skill text](assets/scandinavian-design.txt): complete main text including frontmatter, retained as a plain-text reference. Its literal references to `websites.md` and `scripts/{lines,tints,density,bands}.js` describe supporting files **not supplied** here; no substitute helpers were created or executed. They are not required to document this operational-desktop adaptation. Obtain/inspect them if a later authorized task needs that website-only fallback or browser harness.

The image was inspected for composition and compared with the textual requirements. It shows the approved LADE-only wordmark, three working columns, avatar/name and semantic diff colors; it does not prove functional behavior, pixel-level token values or accessible contrast. Perspective, photographic lighting and raster text are not an implementation palette, typography specification or standalone logo/avatar asset.

The composition is not a literal specification of code, labels, fixture data or action availability. Do not copy the office, monitor, plants or background text into the application. In particular, the depicted check beside coverage, “All tests passing”, “Approve review”, “Request reviews” and “Integrate to main” are illustrative: coverage is not a passing test, fixture evidence is not a real execution, approval is candidate/target-bound, and integration requires a separate explicit authorized action. The labels do not introduce multi-user review, new lifecycle states or automatic progression. The supplied portrait does not establish the real avatar's provenance; retain an existing prototype placeholder when necessary rather than inventing a photo or adding authentication.

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

Apply the supplied skill through its operating priority: comprehension/wayfinding, task completion/accessibility, useful hierarchy/density, brand/semantic meaning, then restraint. This is a documentation adaptation for an operational Linux desktop workbench, not a new redesign, three-variant prototype exercise or landing page. Existing approved composition and product contracts take precedence over generic skill defaults.

### Application and explicit exceptions

| Skill guidance | LADE application / boundary |
| --- | --- |
| Clear hierarchy before decorative reduction | Keep context, controls, comments, evidence and errors near the task they explain. Left-align labels/prose by default; vertically centering the profile name against its avatar does not mean centering all text. Reuse accessible GTK4 primitives; specific higher-level widget libraries remain unselected. |
| Restrained type and spacing | One sans-serif UI family with monospace for code; use a platform font for a native client unless an established product face is justified. Do not infer Inter, a paid font, exact weights or pixel sizes from the image. Operational rows may need more text styles than editorial defaults. Spacing/radius examples are starting guidance, not fixed layout or performance budgets. |
| Avoid styled uppercase | **LADE** stays uppercase as expressly approved. Preserve code, paths, acronyms and user content rather than case-transforming them. |
| Neutral chrome and preserved meaning | Retain semantic diff/status/warning/error/focus meaning with text/signs; no decorative accent or tinted chrome. Do not neutralize source content, screenshots or user-authored emoji as if they were UI icons. |
| Quiet boundaries and repeated records | No lateral Objective accent bar, including pseudo-elements or shadows that recreate one. Keep selected/focus states distinct; retain useful column separators and control boundaries when needed for dense simultaneous inspection. Equivalent controls share geometry; avoid pills/boxes added purely for style. |
| Spacious chapters, media dominance and mobile examples | Marketing/commerce chapters, large editorial margins and mobile scope do not transfer to LADE. The Change Set/task—not a promotional image—is dominant. Preserve useful desktop density, narrow available logical space and 200% scaling. The skill's dark-canvas recipes do not add a dark-mode requirement to the approved white base. |
| Purposeful motion | Do not add animation for polish or animate frequent/keyboard actions. Any later functional motion must be interruptible, honor reduced motion and preserve state feedback; skill timing examples are not measured LADE responsiveness budgets. |
| Verification against the real interface | When UI work is authorized, inspect actual rendered controls, computed interaction states (or native equivalents), contrast on real surfaces, keyboard/Orca behavior and clipping at supported desktop sizes/200%. An image inspection cannot satisfy these tests. No browser/application test or UI change is part of this asset/documentation follow-up. |

Operational rules retained from the approved direction:

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

Use alpha colors rather than fading a whole container with `opacity`, which also dims content and states. Preserve distinct functional ink levels; do not collapse them merely to fit a three-band palette. The skill's 44% tertiary ink is **not** a default for functional metadata such as paths, versions, timestamps, verification or disabled-action explanations; choose sufficient contrast over the actual surface. Its suggested stronger rungs are guidance, not measured LADE passes. Existing truly neutral, accessible tokens need not be rewritten solely to change their representation.

A 10% separator is not automatically adequate for a control boundary or focus indicator. Selected and keyboard-focus states need independent discernibility. Proposed implementation: neutral selected fill plus weight/state label, and a sufficiently contrasting full focus outline; do not reintroduce a lateral accent bar. Final combinations must pass testing.

### Profile correction

Show only a circular avatar and **Matias Gigena**, vertically centered against it. For a prototype retain its existing avatar placeholder if the real avatar is unavailable; do not invent a photograph or implement GitHub authentication for it. This display does not imply an account, connection or sign-in obligation to use LADE. No applicable image/placeholder asset is newly invented in this documentation branch.

## Interaction and accessibility contracts

Preserve [#17](https://github.com/MatiasGigena/LADE/issues/17#issuecomment-5667531284) except its superseded editor-specific assumptions and mandatory first-visit Agent Session input:

- Conventional, remappable keyboard navigation with conflicts, reset and discoverable shortcut reference. Board groups retain Tab/Shift+Tab between groups, Up/Down and Home/End within them, Enter to open. Where tabs exist, preserve arrow keys and Enter/Space.
- F6/Shift+F6 cycle major regions in both directions: header, Objectives, active central surface, right review/feedback panel, and contextual terminal when open. There must always be a working terminal escape route; remapping cannot remove it. Tab remains local and Escape dismisses local menus/dialogs, not the whole pane unexpectedly.
- Explicit opening moves focus to the requested task; closing returns to the opener or a predictable available fallback. Returning to an Objective restores its selected surface and focus; returning to the board restores its card. System-driven reordering keeps focus on the same Objective or a predictable neighbor/heading if removed.
- **Proposed first-visit task defaults carried from #13 into the native component checkpoint:** intent input during definition, inspected-content heading for inspection, comparison heading for revision review. Explicit Respond/Review goes directly to the relevant content. These refine the former fixed Agent Session-input rule and require keyboard/Orca validation before finalization.
- No background event, attention request, incoming revision or notification steals focus. Announce concise important action/error/attention changes politely, group bursts, permit disabling announcements; do not narrate streaming output automatically. Content remains readable on demand.
- Meaningful names include Objective identity, not just a status; expose roles, selection, draft/save/send status, stale references, errors and action outcomes. Keyboard text/range selection, creating feedback, collecting drafts, explicit sending and comparing revisions must be possible without a mouse.
- Visible unobscured focus, text equivalents for colors, WCAG 2.2 AA contrast thresholds, usable 200% text/UI scaling, and reduced-motion behavior remain the baseline, not an asserted blanket conformance claim. Test LADE-owned core flows with Orca on Mint/Cinnamon/X11; test terminal internals and disclose limitations.
- Keep in-app attention regardless of desktop notifications. Background-only desktop notifications are best-effort, silent by default, grouped by new reason, contain Objective/reason but not code/conversation/secrets, and never grant authority. Activation rechecks state and navigates; it cannot answer, freeze, approve or integrate.

### Required visible states

Show idle/empty/loading and actionable failure states rather than hiding errors in minimal styling. Distinguish working capture, frozen candidate, stale reference, unsent draft, saving/saved/failed save, accepted input, queued/dispatched/uncertain delivery, proposed resolution and human confirmation. Show current permissions, disconnected/recovering/interrupted execution, verification missing/failed/stale, expired approval and integration conflict.

Disabled or unavailable actions retain an accessible explanation and recovery path. Integrate is not a decorative primary button: Rust-authorized availability requires the exact valid candidate/target approval and all other [workflow preconditions](first-release-workflow.md#6-transition-and-failure-contracts). Approval, feedback, execution permission, Cancellation, Discard and Cleanup remain distinct controls/actions.

## Validation boundary

Historical B styling and mock data are not the new design baseline. Its documented accessible-name, reverse-F6, contrast and scaling failures are useful test inputs, not inherited accepted behavior. Inspecting the supplied conceptual image and reading the skill resolves reference availability only: the original asset/documentation task performed no application rendering, interaction-state, Orca, contrast-on-rendered-surfaces, performance or security validation. Subsequent interaction and minimal client evidence is linked from the [architecture decision](../planning/client-architecture.md), with its limitations preserved. The [readiness matrix](../planning/implementation-readiness.md#acceptance-matrix) assigns those tests and their evidence. The archived skill's generic verification instructions neither authorize production work nor widen LADE's Linux support contract.

## Implemented tokens (GTK client, calm-UI pass)

Appended record of what the native client implements; it does not change the approved direction above. Colour tokens and their measured pairs stay in `crates/lade-desktop/src/assets/style.css` and `scripts/check-contrast.py`.

| Kind | Token | Value | Use |
| --- | --- | --- | --- |
| Spacing | tight / item / group / section / page | 4 / 8 / 12 / 16 / 24 px (32 for empty-state margins) | `views::widgets::space` for gaps between widgets; the same scale for CSS paddings |
| Type | title | 20px, 600 | Objective and page titles (`.objective-heading`, `.empty-title`) |
| Type | heading | 15px, 600 | Section headings (`.section-title`, `.empty-heading`, `.popover-heading`) |
| Type | body | platform size, 400 (600 for emphasis by weight) | Plain labels |
| Type | meta | 13px, 400, metadata ink (600 for group headings) | Secondary text, reasons, identities |
| Type | mono | 13px monospace | Code, paths, ids, commands |

Presentation rules applied with these tokens: every fact and action has one home surface (`presentation::panel::home`), other surfaces get at most a one-line pointer with a jump. Status is a sign plus text (no pill box) in the Objectives list; the right panel's Overview shows the approval state, the one next review action the service enables for the lifecycle step (its reason in one line when blocked), then *More actions* (destructive actions last, separated) and *Details*, both collapsed; Activity owns Agent Session control (state, Start/Cancel/Resume, accepted inputs and delivery, model and thinking, session tools, profile resources). Disabled actions keep their visible reason where they are shown, plus tooltip and accessible description.
