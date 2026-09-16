# LADE visual specification

Status: visual implementation specification for review, based on the approved workbench and explicit developer corrections. Composition, neutral styling and removals are accepted. Numerical dimensions and semantic color pairs below are **proposed implementation tokens**, not measurements extracted from the perspective mockup or already validated GTK rendering.

This document covers appearance only. It does not define lifecycle transitions, keyboard commands, permissions, IPC, storage or when actions become available. Those belong to the [workflow](docs/product/first-release-workflow.md), [UI interaction contracts](docs/product/ui-principles.md#interaction-and-accessibility-contracts), [architecture](architecture.md) and [implementation spec](docs/implementation/spec.md).

## 1. Reference and visual priorities

Canonical reference: [approved workbench PNG](docs/product/assets/lade-approved-workbench.png). Preserve the app composition, not the monitor, office, plants, photographic lighting or background slogans. It is a conceptual reference, not a working-application screenshot. No new logo or personal photograph is introduced here.

Use the [supplied Scandinavian skill](docs/product/assets/scandinavian-design.txt) through its [LADE-specific application and exceptions](docs/product/ui-principles.md#application-and-explicit-exceptions): comprehension, task completion and useful density come before decorative restraint. A native operational surface is not an editorial landing page. The source's dark, mobile and marketing examples do not create additional LADE deliverables.

For this specification the final approved image in the conversation was visually inspected and the existing repository reference path/blob metadata checked. No claim of a fresh byte-for-byte download comparison, rendered GTK implementation or runtime visual/accessibility pass is made.

### Non-negotiable visual corrections

- Header branding is the existing mark and **LADE**, with no subtitle.
- No greetings, motivational copy, slogans or functionless cards. No bottom-left filler panel.
- Objective rows have **no lateral border, inset stripe, pseudo-stripe, shadow rail or colored accent bar**. Hover is neutral light gray.
- Profile is one circular avatar and **Matias Gigena**, vertically centered together. Use the supplied avatar or an existing labelled placeholder; do not invent a portrait or make sign-in part of this layout.
- Keep comments, code references, evidence, error explanations and visible state. They are functional content, not ornament.
- Preserve code/diff and state meaning through text, signs and appropriate semantic colors. Neutral chrome does not mean neutralizing all content.

## 2. Canvas and composition

Units below are logical pixels at 100% scaling. They are initial layout targets; GTK/Pango scaling must preserve usability rather than turn them into fixed physical pixels. Use the actual available content area, excluding window-manager decorations.

| Region | Proposed wide-layout geometry | Treatment |
| --- | --- | --- |
| Header | 56 high, 20 horizontal inset, 16 between groups | Single white band; one quiet lower divider |
| Objectives | 248 wide, adjustable approximately 224–280 | White; 12 inset; one structural divider at the region boundary, not on rows |
| Main work area | Remaining width; 24 inset, 16 within related groups | Dominant reading area, not a grid of unrelated cards |
| Right inspector | 320 wide, adjustable approximately 280–360; 20 inset | White; one structural divider; sections separated primarily with spacing |
| Changed-file list inside center | Approximately 192 wide when center permits | Flat rows; no second heavy outer frame |
| Code/diff viewport | Remaining center width | White with line gutters and semantic diff fills; no marketing shadow |

At a 1440-wide reference window the center receives approximately 870 pixels before internal padding. Preserve relative dominance rather than the exact photographed proportions. Header groups share a vertical centerline. Keep global search beside the profile; give search approximately 280–360 pixels in the wide layout. Align titles, list text and inspector labels to their own consistent left edges.

The existing Plan/Workbench/Activity navigation is a visual reference only: display the navigation destinations defined by the accepted product contract; do not invent new pages just to reproduce labels. Treat the collaboration journey as a restrained horizontal indicator, with short labels and modest connectors, not oversized navigation cards.

### Narrow and scaled layouts

At roughly 1100 logical pixels and below, use a compact version of the same workbench: reduce rail widths, present secondary rails in accessible collapsible/drawer surfaces, and keep the active code or feedback surface dominant. At approximately 800 by 550 logical pixels, do not insist on three simultaneous fixed-width columns. Search remains reachable; the profile does not push navigation offscreen.

These are proposed responsive compositions, not a mobile-support promise. Do not remove functionality, shrink text below the type scale, clip actions or shrink the whole application with a visual transform. Let document content scroll within its viewport; allow deliberate horizontal scrolling for code. Long labels wrap or truncate with a way to inspect the full value under the functional contract. At 200% text/UI scaling, controls and focus must remain visible.

## 3. Color tokens

Define semantic token names centrally; do not scatter visually similar gray literals across components. Set alpha on the color, not on a parent widget and all its descendants. The neutral ladder derives from the supplied skill; functional metadata uses a stronger rung than its optional 44% example.

| Token | Proposed value | Use |
| --- | --- | --- |
| canvas / surface | `#FFFFFF` | App and ordinary panels |
| ink.primary | `#000000` | Main text, important labels and icons |
| ink.secondary | `rgba(0,0,0,0.64)` | Descriptions, readable supporting text |
| ink.metadata | `rgba(0,0,0,0.56)` | Functional metadata; test on every actual fill |
| ink.optional | `rgba(0,0,0,0.44)` | Optional glyphs only, not important small text |
| border.separator | `rgba(0,0,0,0.10)` | Structural separation, never the sole control/focus affordance |
| border.control | `rgba(0,0,0,0.56)` | Required input/control boundary when a quiet fill is insufficient |
| fill.hover | `rgba(0,0,0,0.05)` | Rows and quiet buttons |
| fill.selected / pressed | `rgba(0,0,0,0.09)` | Selected rows and press feedback |
| focus | `#000000` | At least 2-pixel full outline, 2-pixel offset where space permits |
| primary fill / ink | `#000000` / `#FFFFFF` | Available primary action |
| scrim | `rgba(0,0,0,0.44)` | Actual modal elevation only |

White text needs an appropriate inverse focus treatment on black buttons. A selected Objective uses neutral fill plus text weight/state; keyboard focus uses the full outline. Selection, hover and focus must not collapse into the same appearance. A region divider is permitted; an Objective-side accent is not.

### Semantic content candidates

| Meaning | Proposed ink / background | Redundant visible encoding |
| --- | --- | --- |
| Addition / passed result | `#146C43` / `#EEF8F1` | `+` for diff lines; text/check for results |
| Deletion / error | `#B42318` / `#FFF0EF` | `−` for diff lines; text/error sign |
| Warning / stale context | `#855000` / `#FFF6DF` | Explicit label and warning sign |

These are narrow semantic exceptions, not colored panel chrome. Syntax highlighting may encode syntax; restrict weight and saturation so it does not compete with feedback selections or diff signs. Never use only color to distinguish added/deleted text, failure, focus or an active control. Actual rendered contrast and theme interactions must be checked; passing computed color pairs is not full accessibility conformance.

## 4. Typography, spacing and shape

Use the platform sans-serif family for UI and a platform monospace for code. Do not introduce a paid font or bundle font files without an explicit dependency/license decision. Keep source code, names and acronyms in their original case; LADE remains uppercase.

| Role | Proposed size / weight | Leading and treatment |
| --- | --- | --- |
| Objective title | 24 / 600 | About 30; no giant hero headline |
| Section title | 16 / 600 | About 22 |
| UI body and controls | 14 / 400 or 500 | About 20; same baseline across peers |
| Metadata | 13 / 400 | About 18; readable contrast |
| Code and diff | 13 monospace / 400 | About 20; aligned gutters and selection |
| Brand wordmark | 22 / 500–600 | Keep existing mark/name relationship |

Use available font weights honestly; do not assume every platform face has all weights. These dense-app values deliberately adapt the skill's editorial defaults. Respect user text scaling, rather than clipping to fixed heights. No 9–11 pixel functional metadata to force a screenshot fit.

Spacing scale: **4, 8, 12, 16, 24, 32**. Use 4–8 within a label/value pair, 12–16 within a component and 24–32 between unrelated sections. Put a dimension on one wrapper per axis to avoid double padding.

Proposed shapes: 6-pixel radius for buttons/fields/rows, 8 for popovers/dialogs; 1-pixel structural dividers. Pills are reserved for compact status labels. Large panels remain square or subtly rounded, not nested cards. Default control height is 36, expanding for scaled text; icon-only hit areas are at least 32 by 32. Dense file rows begin around 32 high and Objective rows expand with their content.

No gradients, textures, decorative shadows or glass effects. Use at most one soft neutral shadow for a genuinely elevated popover/dialog, not for every panel.

## 5. Component appearance

| Component | Visual contract |
| --- | --- |
| Objective row | Title, brief description and compact state grouped together; borderless row; selected neutral fill; no repeated lateral rule |
| Header/search | Clear but quiet brand; restrained navigation; search visible beside the aligned avatar/name group; no account greeting |
| Tabs | Uniform peer typography and spacing; active label and modest underline; no pill for each navigation item |
| File list | Flat rows; readable paths and change counts; active fill; symbols distinguish file changes |
| Code/diff | Stable line-number/sign gutters, monospaced alignment, restrained syntax; semantically colored changed lines; selection remains visible over diff fill |
| Feedback reference | Compact file/range/version label above the comment text; no oversized author avatar or decorative quote card |
| Feedback composer | Real input boundary, readable draft text and small save-state line; comments remain visible and do not obscure the selected code |
| Evidence | Aligned name/result rows, inspectable details and explicit failure/stale/missing labels; coverage displayed as a metric, not a success check |
| Primary/secondary actions | Black/white primary; quiet secondary with adequate affordance; consistent dimensions; no permanently emphasized unavailable Integrate |
| Error/warning | Readable local message near the relevant control; semantic sign and text; preserve room for correction rather than toast-only styling |
| Empty/loading | Functional heading or progress plus the relevant action; no illustrations, motivational copy or perpetual decorative animation |
| Destructive action | Explicit label plus distinct warning treatment; never made visually identical to an ordinary neutral action |

The above describes how supplied state looks, not which action is allowed. A disabled action keeps its place and a readable explanation; lowering the opacity of an entire panel is not an acceptable substitute. Agent claims and observed evidence must remain visually distinguishable using labels/provenance, not merely icon color.

One coherent symbolic icon family; monochrome for ordinary UI. Preserve legitimate content imagery and user-authored emoji. Do not invent a replacement for the existing logo or turn an unavailable profile asset into a fabricated face.

## 6. Visual states and motion

Capture default, hover, pressed, selected, focused, disabled, loading and error appearances. Also capture the supplied product states of unsaved/saving/saved/failed draft, stale reference, interrupted connection and unavailable integration. This document prescribes their visual distinction only.

Default to no animation for typing, selection, frequent navigation or status streams. A short opacity/color transition up to about 120 milliseconds is a proposed optional refinement, never a performance requirement. No animated panel entrances for keyboard navigation, no pulsing brand and no perpetual shimmer. Honor reduced motion; state remains visible without movement.

## 7. Visual acceptance sheet

IMP-002 establishes the initial native component visual sheet; later integrated screens reuse it. The sheet must include the workbench, Objective list, file/diff selection, feedback, evidence and action group with realistic long text and all relevant states.

Capture the actual GTK application at 1440 by 900 and 1280 by 800 logical pixels, a compact window, and the supported desktop's real 200% text/UI scaling. These are review fixtures, not certified minimum hardware requirements. Review label/avatar vertical alignment, matched control dimensions, panel proportions, neutral grays, readable small text, full focus outlines and the absence of lateral Objective accents.

Measure text contrast against actual composed surfaces: normal functional text at least 4.5:1, eligible large text 3:1, and required non-text indicators/control boundaries 3:1 under the accepted accessibility contract. Compare source/diff signs and selection in combination. Check actual hover/focus/pressed rendering, not declarations alone. The full keyboard/Orca acceptance belongs to the functional checklist, not a claim made by screenshots.

Record what was inspected and what remains unverified. A photographed mockup, HTML-only prototype, CSS inspection or pure color calculation is not a native-application pass. Preserve the approved identity and layout; change a numerical token deliberately when legibility or the native component requires it, and retain before/after evidence.
