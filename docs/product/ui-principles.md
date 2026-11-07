# UI principles and approved workbench direction

Status: **developer-approved composition and visual direction; GTK4 native is now the selected graphical client** ([accepted architecture](../planning/client-architecture.md)). The inspection/feedback prototype, including its global-search correction, is accepted as the interaction baseline—not as full native/accessibility validation. The [workflow](first-release-workflow.md) controls action semantics; illustrative labels or enabled buttons in a mockup cannot override it.

## GTK4 styling direction

Retain the approved design, not the minimal benchmark's styling. GTK CSS and native layout APIs support the intended gray surfaces, accent, typography, spacing, density, elevation and focus states. GTK CSS is not browser CSS; widget structure and highly customized controls may require GTK-specific implementation. Prefer accessible native controls over custom-drawing the interface. Rich file/diff components remain an implementation choice to validate. No pixel-perfect result, particular widget library or full accessibility guarantee follows from toolkit selection.

## Reference and adaptation

The approved direction came from a conceptual workbench image (header, Objectives sidebar, dominant Change Set inspection and right-hand evidence and review panel) and an external Scandinavian-style design guideline. Neither is part of this repository; the rules below and the [visual specification](../../design.md) are the self-contained LADE contract. The concept was **not a screenshot of a running LADE implementation**, and its depicted surroundings are not part of the application.

The concept image showed the approved LADE-only wordmark, three working columns, avatar/name and semantic diff colors; it did not prove functional behavior, pixel-level token values or accessible contrast. Perspective, photographic lighting and raster text are not an implementation palette, typography specification or standalone logo/avatar asset.

The concept composition is not a literal specification of code, labels, fixture data or action availability. Do not copy the office, monitor, plants or background text into the application. In particular, the depicted check beside coverage, “All tests passing”, “Approve review”, “Request reviews” and “Integrate to main” are illustrative: coverage is not a passing test, fixture evidence is not a real execution, approval is candidate/target-bound, and integration requires a separate explicit authorized action. The labels do not introduce multi-user review, new lifecycle states or automatic progression. Its portrait does not establish a real avatar; retain the existing placeholder rather than inventing a photo or adding authentication.

## Composition

| Region | Function |
| --- | --- |
| Header | LADE mark and **LADE** only beside it, search and profile. The fixed view navigation (including Workspace overview) sits in the tab row above the work-area pane, beside the inspector's own tabs. No “Agentic development environment” subtitle. |
| Left column | Objectives with brief context, selected Objective and system-derived attention/status. No lateral accent stripe or border on Objective cards/rows. |
| Dominant center | Objective intent/discussion when defining work; Read-only Inspection of full files and Change Sets when understanding results; revision comparison when checking corrections. |
| Right panel | Summary, observed evidence and its provenance, Contextual Feedback, review and integration actions with reasons for blocked/unavailable actions. |
| Contextual surfaces | Agent Session, tool activity and terminal in the selected Objective context, accessible on demand without forcing conversation or terminal use for every task. |

The Workspace Objectives board remains the overview; a sidebar does not replace it. Retain the lifecycle attention precedence and secondary badges; no draggable stages. The visible **Inspect → Feedback → Agent revision → Review → Integrate** journey explains collaboration, not an automatic pipeline, irreversible wizard or new lifecycle.

This supersedes the original mandatory Agent Session-centered dedicated screen and the editor pane, not the board/navigation/attention rules. Conversation remains useful while defining intent and responding to the agent. Do not silently switch the main surface or focus when background work changes state; task-led defaults guide explicit navigation, not unsolicited mode changes.

## Operational Scandinavian-style rules

Apply the Scandinavian-style guidance through its operating priority: comprehension/wayfinding, task completion/accessibility, useful hierarchy/density, brand/semantic meaning, then restraint. This is a documentation adaptation for an operational Linux desktop workbench, not a new redesign, three-variant prototype exercise or landing page. Existing approved composition and product contracts take precedence over generic guideline defaults.

### Application and explicit exceptions

| Guideline | LADE application / boundary |
| --- | --- |
| Clear hierarchy before decorative reduction | Keep context, controls, comments, evidence and errors near the task they explain. Left-align labels/prose by default; vertically centering the profile name against its avatar does not mean centering all text. Reuse accessible GTK4 primitives; specific higher-level widget libraries remain unselected. |
| Restrained type and spacing | One sans-serif UI family with monospace for code. The [visual specification](../../design.md#4-typography-spacing-and-shape) bundles Instrument Sans and Fragment Mono (SIL OFL 1.1) and falls back to the platform faces. Nothing about type is inferred from the concept image. Operational rows may need more text styles than editorial defaults. Spacing/radius values are implementation tokens, not performance budgets. |
| Avoid styled uppercase | **LADE** stays uppercase as expressly approved. Preserve code, paths, acronyms and user content rather than case-transforming them. |
| Gray chrome, one accent and preserved meaning | Retain semantic diff/status/warning/error/focus meaning with text/signs. Fluorescent lime is the one chrome accent, in its listed roles (primary action, checked and current state, journey progress, the mark, icons, selected titles, the live connection, the caret, text selection); never a border or focus ring, no tinted panels. Do not neutralize source content, screenshots or user-authored emoji as if they were UI icons. |
| Quiet boundaries and repeated records | No lateral Objective accent bar, including pseudo-elements or shadows that recreate one. Keep selected/focus states distinct; retain useful column separators and control boundaries when needed for dense simultaneous inspection. Equivalent controls share geometry; avoid pills/boxes added purely for style. |
| Spacious chapters, media dominance and mobile examples | Marketing/commerce chapters, large editorial margins and mobile scope do not transfer to LADE. The Change Set/task—not a promotional image—is dominant. Preserve useful desktop density, narrow available logical space and 200% scaling. LADE ships one dark gray appearance; this does not add a light-mode requirement. |
| Purposeful motion | Do not add animation for polish or animate frequent/keyboard actions. Any later functional motion must be interruptible, honor reduced motion and preserve state feedback; guideline timing examples are not measured LADE responsiveness budgets. |
| Verification against the real interface | When UI work is authorized, inspect actual rendered controls, computed interaction states (or native equivalents), contrast on real surfaces, keyboard/Orca behavior and clipping at supported desktop sizes/200%. An image inspection cannot satisfy these tests. No browser/application test or UI change is part of this asset/documentation follow-up. |

Operational rules retained from the approved direction:

- Charcoal window chrome, gray panes, white ink tiers and **one fluorescent lime accent**. Grays stay neutral: no beige, blue-tinted or green-tinted chrome; lime is the only hue outside semantic content.
- Clear hierarchy, useful information density, consistent spacing and minimal decoration. A single sans-serif UI family; monospace where code/output requires it. Font, metrics and spacing scale are set in the [visual specification](../../design.md) and remain subject to legibility and redistribution checks.
- Coherent iconography with accessible names; no decorative emojis. Shadows, rims and containers only to communicate grouping, elevation or interaction: raised controls and floating surfaces carry a soft shadow, panes and rows do not.
- Do not import oversized editorial margins, decorative uppercase conventions or mobile requirements automatically. LADE remains uppercase as a proper name. Linux desktop scope still requires usable 200% scaling and narrow available logical space.
- Remove motivational copy, greetings, slogans and functionless cards: no “Small steps. Meaningful progress.”, “Good morning, Alex.” or “You’re in control.”
- Removing decoration does **not** remove review comments, draft text, code references, errors, verification results, permission scope or explanations needed to understand an action.
- Semantic colors remain where useful, especially diff additions/deletions, warnings, errors and states, with signs or text labels. A restrained gray palette is not a reason to remove status information or accessibility.

### Token references

The colour, type, spacing, radius and elevation tokens live in the [visual specification](../../design.md#3-color-tokens) and, as implemented, in `crates/lade-desktop/src/assets/style.css`. In short:

| Role | Reference |
| --- | --- |
| Window chrome | `#141416` |
| Pane surface | `#1C1C1F` |
| Raised control / well | `#27272B` / `#161618` |
| Primary ink | `#E4E4E7` (strong `#F4F4F5`) |
| Secondary / metadata ink | `#B9B9C0` / `#A4A4AC` |
| Hover / selected fill | White at 6% / 10% over the surface |
| Accent | Fluorescent lime `#C4F042`, with `#151A05` ink on it |

Use alpha colors rather than fading a whole container with `opacity`, which also dims content and states. Preserve distinct functional ink levels; do not collapse them merely to fit a smaller palette. Functional metadata such as paths, versions, timestamps, verification or disabled-action explanations must keep at least 4.5:1 on every surface and fill it can sit on; `scripts/check-contrast.py` computes those pairs.

A low-alpha rim is not a focus indicator. Selected and keyboard-focus states need independent discernibility: the selected fill plus accent ink, weight and the accessible selected state, and a full neutral focus outline at 3:1 or more on every surface; do not reintroduce a lateral accent bar. Final combinations must pass testing on the rendered client.

### Profile correction

Show only a circular placeholder avatar and the local user's display name (the system account's real name, falling back to the login name), vertically centered against it. Do not invent a photograph or implement GitHub authentication for it. This display does not imply an account, connection or sign-in obligation to use LADE. No applicable image/placeholder asset is newly invented in this documentation branch.

## Interaction and accessibility contracts

Preserve the accessibility baseline except its superseded editor-specific assumptions and mandatory first-visit Agent Session input:

- Conventional, remappable keyboard navigation with conflicts, reset and discoverable shortcut reference. Board groups retain Tab/Shift+Tab between groups, Up/Down and Home/End within them, Enter to open. Where tabs exist, preserve arrow keys and Enter/Space.
- F6/Shift+F6 cycle major regions in both directions: header, Objectives, active central surface, right review/feedback panel, and contextual terminal when open. There must always be a working terminal escape route; remapping cannot remove it. Tab remains local and Escape dismisses local menus/dialogs, not the whole pane unexpectedly.
- Explicit opening moves focus to the requested task; closing returns to the opener or a predictable available fallback. Returning to an Objective restores its selected surface and focus; returning to the board restores its card. System-driven reordering keeps focus on the same Objective or a predictable neighbor/heading if removed.
- **Proposed first-visit task defaults carried from the client decision into the native component checkpoint:** intent input during definition, inspected-content heading for inspection, comparison heading for revision review. Explicit Respond/Review goes directly to the relevant content. These refine the former fixed Agent Session-input rule and require keyboard/Orca validation before finalization.
- No background event, attention request, incoming revision or notification steals focus. Announce concise important action/error/attention changes politely, group bursts, permit disabling announcements; do not narrate streaming output automatically. Content remains readable on demand.
- Meaningful names include Objective identity, not just a status; expose roles, selection, draft/save/send status, stale references, errors and action outcomes. Keyboard text/range selection, creating feedback, collecting drafts, explicit sending and comparing revisions must be possible without a mouse.
- Visible unobscured focus, text equivalents for colors, WCAG 2.2 AA contrast thresholds, usable 200% text/UI scaling, and reduced-motion behavior remain the baseline, not an asserted blanket conformance claim. Test LADE-owned core flows with Orca on Mint/Cinnamon/X11; test terminal internals and disclose limitations.
- Keep in-app attention regardless of desktop notifications. Background-only desktop notifications are best-effort, silent by default, grouped by new reason, contain Objective/reason but not code/conversation/secrets, and never grant authority. Activation rechecks state and navigates; it cannot answer, freeze, approve or integrate.

### Required visible states

Show idle/empty/loading and actionable failure states rather than hiding errors in minimal styling. Distinguish working capture, frozen candidate, stale reference, unsent draft, saving/saved/failed save, accepted input, queued/dispatched/uncertain delivery, proposed resolution and human confirmation. Show current permissions, disconnected/recovering/interrupted execution, verification missing/failed/stale, expired approval and integration conflict.

Disabled or unavailable actions retain an accessible explanation and recovery path. Integrate is not a decorative primary button: Rust-authorized availability requires the exact valid candidate/target approval and all other [workflow preconditions](first-release-workflow.md#6-transition-and-failure-contracts). Approval, feedback, execution permission, Cancellation, Discard and Cleanup remain distinct controls/actions.

## Validation boundary

Historical B styling and mock data are not the new design baseline. Its documented accessible-name, reverse-F6, contrast and scaling failures are useful test inputs, not inherited accepted behavior. Inspecting the conceptual image and the design guideline resolved reference availability only: the original asset/documentation task performed no application rendering, interaction-state, Orca, contrast-on-rendered-surfaces, performance or security validation. Subsequent interaction and minimal client evidence is linked from the [architecture decision](../planning/client-architecture.md), with its limitations preserved. The [readiness matrix](../planning/implementation-readiness.md#acceptance-matrix) assigns those tests and their evidence. The guideline's generic verification instructions neither authorize production work nor widen LADE's Linux support contract.

## Implemented tokens (GTK client, calm-UI pass)

Appended record of what the native client implements; it does not change the approved direction above. Colour tokens and their computed pairs stay in `crates/lade-desktop/src/assets/style.css` and `scripts/check-contrast.py`.

| Kind | Token | Value | Use |
| --- | --- | --- | --- |
| Spacing | tight / item / group / section / page / region | 4 / 8 / 12 / 16 / 24 / 32 px | `views::widgets::space` for gaps between widgets; CSS paddings also use the 2, 6, 10 and 20 half steps for controls and pane insets |
| Type | title | 22px, 600, strong ink | Page titles (`.objective-heading`, `.empty-title`); the Objective's title appears only in the Objectives rail |
| Type | heading | 15px, 600 (17px for `.empty-heading`) | Section headings (`.section-title`, `.empty-heading`, `.popover-heading`) |
| Type | body | platform size, 400 (600 for emphasis by weight) | Plain labels |
| Type | control | 13px, 500 | Buttons, tabs, segmented items |
| Type | meta | 12px, 500, metadata ink | Secondary text, reasons, identities, group headings |
| Type | mono | 12.5px Fragment Mono, else monospace | Code, paths, ids, commands |

Shared components (`crates/lade-desktop/src/views/widgets/mod.rs` and its `input` and `dialog` submodules), used instead of one-off layouts:

| Component | What it is | Used for |
| --- | --- | --- |
| `empty_state` | Centred heading, an optional one-paragraph explanation, the one relevant action and an optional metadata note after it; no illustration | First Objective (work area, Activity, Terminal), no selection, no changes, no file, no terminal |
| `inline_message` | Sign and plain sentence on the kind's semantic fill (info, warning, error); technical detail (error code, raw service text) in a collapsed *Details* and the accessible description | Refusals beside the control that issued the command, load failures, the catalogue warning |
| `text_field` / `placeholder_text` | Metadata label above a bounded multi-line field with a visible placeholder (shown while empty, as an entry's) | Intent fields, the composer, override reasons |
| `dropdown_or` | Drop-down that ellipsizes a long choice (full text in tooltip and list) and, with no choices, is insensitive and says what is missing | Provider, model, thinking, delivery mode, integration target |
| `disclosure` | Quiet full-width toggle (▸/▾) showing or hiding its content | More actions, Details, Agent settings, Accepted inputs, profile resources |
| `action_row`, `list_action_row`, `compact_action` | An action whose availability is the service's projection: a disabled one keeps its place with its reason (visible, tooltip, accessible description); a not-offered one is hidden; accent-filled only when cleanly available | Overview next action, More actions, Activity's agent action and Send |
| Header search | Entry and scope drop-down as one seamless linked control: one boundary, one focus ring | Ctrl+K search |

A refused command is shown once, on the surface that issued it (`PresentationState::last_refusal` per Objective and `ActionSurface`), as an `inline_message` in plain words; it clears when that surface's command succeeds or the Objective changes. Refusals no Objective surface can show are the only global notices.

Presentation rules applied with these tokens: every fact and action has one home surface (`presentation::objective_actions::home`), other surfaces get at most a one-line pointer with a jump. In the Objectives list the attention group names the status and a row carries only a status sign, with the full state, badges and outcome in its tooltip and accessible name (no pill box); the right panel's Overview shows the approval state, the one next review action the service enables for the lifecycle step (its reason in one line when blocked), then *More actions* (destructive actions last, separated) and *Details*, both collapsed; Activity owns Agent Session control (state, Start/Cancel/Resume, accepted inputs and delivery, model and thinking, session tools, profile resources): before the first start a *Set up the agent* card with Start agent stands in the transcript's place; afterwards the header shows the one agent action that applies and *Agent settings* (collapsed) holds the rest. Disabled actions keep their visible reason where they are shown, plus tooltip and accessible description.
