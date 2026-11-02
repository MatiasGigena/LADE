# LADE visual specification

Status: visual implementation specification for review, based on the approved workbench and explicit developer corrections. Composition, neutral styling and removals are accepted. The **elevated neutral** revision (section 1, *Visual language*) replaces the original flat, divider-separated chrome with a canvas, raised sheets and soft shadows while keeping the color palette unchanged. Numerical dimensions and semantic color pairs below are **proposed implementation tokens**, not measurements extracted from the perspective mockup or already validated GTK rendering.

This document covers appearance only. It does not define lifecycle transitions, keyboard commands, permissions, IPC, storage or when actions become available. Those belong to the [workflow](docs/product/first-release-workflow.md), [UI interaction contracts](docs/product/ui-principles.md#interaction-and-accessibility-contracts), [architecture](architecture.md) and [implementation spec](docs/implementation/spec.md).

## 1. Reference and visual priorities

Canonical reference: [approved workbench PNG](docs/product/assets/lade-approved-workbench.png). Preserve the app composition, not the monitor, office, plants, photographic lighting or background slogans. It is a conceptual reference, not a working-application screenshot. No new logo or personal photograph is introduced here.

Use the [supplied Scandinavian skill](docs/product/assets/scandinavian-design.txt) through its [LADE-specific application and exceptions](docs/product/ui-principles.md#application-and-explicit-exceptions): comprehension, task completion and useful density come before decorative restraint. A native operational surface is not an editorial landing page. The source's dark, mobile and marketing examples do not create additional LADE deliverables.

For this specification the final approved image in the conversation was visually inspected and the existing repository reference path/blob metadata checked. No claim of a fresh byte-for-byte download comparison, rendered GTK implementation or runtime visual/accessibility pass is made.

### Visual language: elevated neutral

The palette stays exactly what it was: white, black, the neutral alpha ladder and the three semantic content pairs. What changed is how surfaces are built from it.

- **Canvas and sheets.** The window is a light neutral canvas. The header and the Objectives rail sit directly on it; the main work area and the inspector are white sheets with a 12-pixel radius, a hairline ring and a soft two-layer shadow. Regions are separated by the gap between sheets, not by 1-pixel rules.
- **Depth carries state.** Raised means selected or active: the selected Objective is a white card on the canvas, the active segment of a segmented control is a white pill on a gray track, a primary action casts a slightly deeper shadow than a secondary one. Hover is a flat neutral fill and never adds elevation.
- **One segmented control.** Navigation, inspector tabs, the Diff/Full file switch and the compact region switcher share one control: a gray track with the active item raised as a white pill.
- **Soft controls.** Buttons and dropdowns use an 8-pixel radius, a light ring and a hairline shadow. Text fields keep a 3:1 boundary because their edge is what identifies them.
- **Shadows are elevation, not decoration.** Every shadow is neutral black at low alpha and belongs to one of the elevation levels in section 4. No colored glows, gradients, textures or glass.

### Non-negotiable visual corrections

- Header branding is the existing mark and **LADE**, with no subtitle.
- No greetings, motivational copy, slogans or functionless cards. No bottom-left filler panel.
- Objective rows have **no lateral border, inset stripe, pseudo-stripe, shadow rail or colored accent bar**. Hover is neutral light gray. The selected row's elevation is an even ring and shadow on all sides, never a one-sided edge.
- Profile is one circular avatar and **Matias Gigena**, vertically centered together. Use the supplied avatar or an existing labelled placeholder; do not invent a portrait or make sign-in part of this layout.
- Keep comments, code references, evidence, error explanations and visible state. They are functional content, not ornament.
- Preserve code/diff and state meaning through text, signs and appropriate semantic colors. Neutral chrome does not mean neutralizing all content.

## 2. Canvas and composition

Units below are logical pixels at 100% scaling. They are initial layout targets; GTK/Pango scaling must preserve usability rather than turn them into fixed physical pixels. Use the actual available content area, excluding window-manager decorations.

| Region | Proposed wide-layout geometry | Treatment |
| --- | --- | --- |
| Header | 64 high, 24 horizontal inset, 16 between groups | On the canvas; no divider. Navigation is a segmented pill control |
| Objectives | 264 wide, adjustable approximately 224–280 | On the canvas; 16 inset; groups 24 apart; titles, group labels and the archive toggle share one text edge |
| Main work area | Remaining width; 32 inset (16 compact), 16 within related groups | White sheet, 12 radius, 12 from the window's bottom edge and 6 from the inspector sheet; dominant reading area, not a grid of unrelated cards |
| Right inspector | 344 wide, adjustable approximately 280–360; 24 inset, sections 32 apart | White sheet beside the work area, 12 from the window's right and bottom edges. Shown only when an Objective is selected; without one the work area sheet takes the full width |
| Changed-file list inside center | Approximately 192 wide when center permits | Flat rows inside the sheet: file name on the first line, directory and line counts on the second |
| Code/diff viewport | Remaining center width | White, 10 radius, hairline ring; toolbar and footer on a faint inset fill; line gutters and semantic diff fills; no shadow (it is inside a sheet) |

At a 1440-wide reference window the center receives approximately 820 pixels before internal padding. Preserve relative dominance rather than the exact photographed proportions. Header groups share a vertical centerline. Keep global search beside the profile; give search approximately 280–360 pixels in the wide layout. Align titles, list text and inspector labels to their own consistent left edges; inspector tabs and the content below them start on the same edge.

The existing Plan/Workbench/Activity navigation is a visual reference only: display the navigation destinations defined by the accepted product contract; do not invent new pages just to reproduce labels. Treat the collaboration journey as a restrained horizontal indicator, with short labels and modest connectors, not oversized navigation cards.

### Narrow and scaled layouts

At roughly 1100 logical pixels and below, use a compact version of the same workbench: reduce rail widths, present secondary rails in accessible collapsible/drawer surfaces, and keep the active code or feedback surface dominant. The compact region switcher is the same segmented pill control as navigation, on the canvas; the visible region is one sheet with 8-pixel gutters. At approximately 800 by 550 logical pixels, do not insist on three simultaneous fixed-width columns. Search remains reachable; the profile does not push navigation offscreen.

These are proposed responsive compositions, not a mobile-support promise. Do not remove functionality, shrink text below the type scale, clip actions or shrink the whole application with a visual transform. Let document content scroll within its viewport; allow deliberate horizontal scrolling for code. Long labels wrap or truncate with a way to inspect the full value under the functional contract. At 200% text/UI scaling, controls and focus must remain visible.

## 3. Color tokens

Define semantic token names centrally; do not scatter visually similar gray literals across components. Set alpha on the color, not on a parent widget and all its descendants. The neutral ladder derives from the supplied skill; functional metadata uses a stronger rung than its optional 44% example.

| Token | Proposed value | Use |
| --- | --- | --- |
| surface | `#FFFFFF` | Sheets, cards, popovers, dialogs and raised selections |
| canvas | `#F3F3F3` | Window ground behind the header, rail and sheets |
| ink.primary | `#000000` | Main text, important labels and icons |
| ink.secondary | `rgba(0,0,0,0.64)` | Descriptions, readable supporting text |
| ink.metadata | `rgba(0,0,0,0.56)` | Functional metadata; test on every actual fill |
| ink.optional | `rgba(0,0,0,0.44)` | Optional glyphs only, not important small text |
| border.separator | `rgba(0,0,0,0.10)` | Rules inside a sheet (evidence rows, toolbars), never the sole control/focus affordance |
| border.soft | `rgba(0,0,0,0.12)` | Hairline ring of sheets, cards, buttons, dropdowns and the raised segment |
| border.control | `rgba(0,0,0,0.44)` | Required text-field/checkbox boundary; at least 3:1 on every fill it sits on |
| fill.track | `rgba(0,0,0,0.06)` | Track of segmented controls |
| fill.hover | `rgba(0,0,0,0.05)` | Rows and quiet buttons |
| fill.selected / pressed | `rgba(0,0,0,0.09)` | Selected rows and press feedback |
| focus | `#000000` | At least 2-pixel full outline, 2-pixel offset where space permits |
| primary fill / ink | `#000000` / `#FFFFFF` | Available primary action |
| scrim | `rgba(0,0,0,0.44)` | Actual modal elevation only |
| shadow.soft | `rgba(0,0,0,0.06)` | Hairline contact shadow (controls, cards) |
| shadow.sheet | `rgba(0,0,0,0.08)` | Ambient shadow of sheets, the raised segment and the selected Objective |
| shadow.popover | `rgba(0,0,0,0.12)` | Ambient shadow of popovers and the primary action |

White text needs an appropriate inverse focus treatment on black buttons. A selected Objective is raised as a white card plus text weight/state; keyboard focus uses the full outline. Selection, hover and focus must not collapse into the same appearance. A region divider is permitted; an Objective-side accent is not.

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
| Objective title | 28 / 700, −0.4 tracking | About 34; leads the work area without becoming a hero headline |
| Dialog title | 18 / 600 | About 24; in-content heading of every dialog |
| Empty-state heading | 18 / 600 | About 24 |
| Section title | 16 / 600 | About 22 |
| Group label (rail) | 12 / 600, +0.3 tracking | About 16; sentence case |
| Field label | 13 / 500, ink.secondary | About 18; labels form fields |
| UI body and controls | 14 / 400 or 500 | About 20; same baseline across peers |
| Metadata | 13 / 400 | About 18; readable contrast |
| Code and diff | 13 monospace / 400 | About 20; aligned gutters and selection |
| Brand wordmark | 20 / 700, +1 tracking | Keep existing mark/name relationship |

Use available font weights honestly; do not assume every platform face has all weights. These dense-app values deliberately adapt the skill's editorial defaults. Respect user text scaling, rather than clipping to fixed heights. No 9–11 pixel functional metadata to force a screenshot fit.

Spacing scale: **4, 8, 12, 16, 24, 32**. Use 4–8 within a label/value pair, 12–16 within a component and 24–32 between unrelated sections. Put a dimension on one wrapper per axis to avoid double padding.

LADE favors air over density where it costs no information:

- **Whitespace groups.** A title sits closer to its own metadata (4) than to the next record (12 or more); groups of records sit 24 apart, inspector sections 32. Never spread lines evenly to fill a region.
- **Words keep their inset.** Text stays at least 12 from the edge of any row, card or field, 16–24 from a panel edge and 32 from the work area sheet's edge; carry the same inset round the bottom, not just the top.
- **Rows breathe.** Objective rows 10 by 12 padding with 2 between; file rows 8 by 12 with 4 between; evidence rows 12 vertical; code lines 3 above and below.
- **Running text has leading and a measure.** Read-only prose (activity, intent, empty-state bodies) uses 1.3 line height; agent and system text wrap at about 88 characters, your messages at about 64. Editable fields keep the default leading so their bounded heights still show their text.
- **Compact gives space back first.** Below the wide layout, insets drop to 16 and rows keep their padding.

Shapes: 8-pixel radius for buttons, fields, rows and segments; 10 for segmented tracks and the code viewer; 12 for sheets, cards, popovers and dialogs; 14 for the activity composer. Status labels stay sign plus text, not pills. Cards appear only where a record is a unit the person acts on (a feedback draft, the composer); lists of peers stay flat rows inside their sheet. Default control height is 36, expanding for scaled text; icon-only hit areas are at least 32 by 32. Dense file rows begin around 32 high and Objective rows expand with their content.

Elevation levels, all neutral black at low alpha:

| Level | Shadow | Used by |
| --- | --- | --- |
| 0 — flat | none | Rows, quiet buttons, hover, anything inside the code viewer |
| 1 — contact | 1-pixel ring `border.soft` + `0 1px 2px shadow.soft` | Buttons, dropdowns, search, avatar, feedback drafts |
| 2 — raised | ring + `0 1px 3px shadow.sheet` | Selected Objective, active segment |
| 3 — sheet | ring + `0 1px 2px shadow.soft` + `0 8px 24px shadow.sheet` | Work area and inspector sheets, activity composer |
| 4 — floating | ring + `0 2px 6px shadow.soft` + `0 16px 40px shadow.popover` | Popovers, menus; dialogs use the window manager's own |

The primary action sits between levels 2 and 3 so the one next action reads first. No gradients, textures, colored glows or glass. Disabled and pressed controls drop to level 0.

## 5. Component appearance

| Component | Visual contract |
| --- | --- |
| Objective row | Title, brief description and compact state grouped together; borderless on the canvas; unselected titles medium weight; selected raised as a white card with semibold title; no lateral rule |
| Header/search | On the canvas; clear but quiet brand; navigation as one segmented pill control; search a white level-1 field beside the aligned avatar/name group; no account greeting |
| Tabs and modes | One segmented control: gray track, transparent peers in secondary ink, the active one a raised white pill in primary ink and semibold. Inspector tabs split the panel width evenly |
| File list | Flat rows; file name first, directory dimmed below with change counts right-aligned beside it; full path in tooltip and accessible name; active fill; symbols distinguish file changes |
| Code/diff | Stable line-number/sign gutters, monospaced alignment, restrained syntax; semantically colored changed lines; selection remains visible over diff fill |
| Feedback reference | Compact file/range/version label above the comment text; no oversized author avatar or decorative quote card |
| Feedback draft | One level-1 card per draft holding reference, excerpt, comment field, save state and Remove, all on the card's text edge |
| Feedback composer | Real input boundary, readable draft text and small save-state line; comments remain visible and do not obscure the selected code |
| Activity transcript | No rule between items; items 12 apart. Your turns are right-aligned bubbles sized to their text; tool and system output in a full-width outlined inset block; thinking outlined; agent text plain, labelled *agent claim*, wrapped at a readable measure |
| Activity composer | One level-3 card containing the borderless input, delivery mode, hint and Send; the focus ring surrounds the whole card |
| Dialog | In-content heading, body, and actions in a tinted footer separated by a rule; primary action last |
| Evidence | Aligned name/result rows (candidates and checks share one shape: title, value at the right, metadata beneath), inspectable details and explicit failure/stale/missing labels; coverage displayed as a metric, not a success check |
| Primary/secondary actions | Black/white primary; quiet secondary with adequate affordance; consistent dimensions; no permanently emphasized unavailable Integrate |
| Error/warning | Readable local message near the relevant control; semantic sign and text; preserve room for correction rather than toast-only styling |
| Empty/loading | Functional heading or progress plus the relevant action (Select an Objective offers New Objective); no empty side columns, illustrations, motivational copy or perpetual decorative animation |
| Disclosure | Theme chevron in metadata ink beside a semibold secondary label; no text triangles |
| Destructive action | Explicit label plus distinct warning treatment; never made visually identical to an ordinary neutral action |

The above describes how supplied state looks, not which action is allowed. A disabled action keeps its place and a readable explanation; lowering the opacity of an entire panel is not an acceptable substitute. Agent claims and observed evidence must remain visually distinguishable using labels/provenance, not merely icon color.

One coherent symbolic icon family; monochrome for ordinary UI. Preserve legitimate content imagery and user-authored emoji. Do not invent a replacement for the existing logo or turn an unavailable profile asset into a fabricated face.

## 6. Visual states and motion

Capture default, hover, pressed, selected, focused, disabled, loading and error appearances. Also capture the supplied product states of unsaved/saving/saved/failed draft, stale reference, interrupted connection and unavailable integration. This document prescribes their visual distinction only.

Default to no animation for typing, selection, frequent navigation or status streams. A short opacity/color transition up to about 120 milliseconds is a proposed optional refinement, never a performance requirement. No animated panel entrances for keyboard navigation, no pulsing brand and no perpetual shimmer. Honor reduced motion; state remains visible without movement.

## 7. Visual acceptance sheet

IMP-002 establishes the initial native component visual sheet; later integrated screens reuse it. The sheet must include the workbench, Objective list, file/diff selection, feedback, evidence and action group with realistic long text and all relevant states.

Capture the actual GTK application at 1440 by 900 and 1280 by 800 logical pixels, a compact window, and the supported desktop's real 200% text/UI scaling. The ignored `capture_fixture_views` test renders populated fixture views at 1440 by 900 and 800 by 550 into `LADE_CAPTURE_DIR` for before/after comparison; it does not replace the scaled and assistive-technology passes. These are review fixtures, not certified minimum hardware requirements. Review label/avatar vertical alignment, matched control dimensions, panel proportions, neutral grays, readable small text, full focus outlines, shadows that match their elevation level and are not clipped, and the absence of lateral Objective accents.

Measure text contrast against actual composed surfaces: normal functional text at least 4.5:1, eligible large text 3:1, and required non-text indicators/control boundaries 3:1 under the accepted accessibility contract. Compare source/diff signs and selection in combination. Check actual hover/focus/pressed rendering, not declarations alone. The full keyboard/Orca acceptance belongs to the functional checklist, not a claim made by screenshots.

Record what was inspected and what remains unverified. A photographed mockup, HTML-only prototype, CSS inspection or pure color calculation is not a native-application pass. Preserve the approved identity and layout; change a numerical token deliberately when legibility or the native component requires it, and retain before/after evidence.
