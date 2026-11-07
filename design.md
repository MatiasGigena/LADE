# LADE visual specification

Status: visual implementation specification, based on the approved workbench composition and explicit developer corrections. The **gray workbench** revision (section 1, *Visual language*) replaces the former black monochrome field: a charcoal window chrome carries rounded gray panes, white ink sets the text tiers and one salmon accent marks the primary action, focus and checked state. The semantic diff and status hues keep their meaning, retuned for the gray ground. Numerical dimensions and semantic color pairs below are **implementation tokens**: the colour pairs are computed by `scripts/check-contrast.py`, not yet measured on a rendered GTK surface.

This document covers appearance only. It does not define lifecycle transitions, keyboard commands, permissions, IPC, storage or when actions become available. Those belong to the [workflow](docs/product/first-release-workflow.md), [UI interaction contracts](docs/product/ui-principles.md#interaction-and-accessibility-contracts), [architecture](architecture.md) and [implementation spec](docs/implementation/spec.md).

## 1. Reference and visual priorities

Reference: the approved conceptual workbench composition described in [UI principles](docs/product/ui-principles.md#reference-and-adaptation). Preserve the app composition, not the monitor, office, plants, photographic lighting or background slogans of the concept. No new logo or personal photograph is introduced here.

Comprehension, task completion and useful density come before decorative restraint. A native operational surface is not an editorial landing page; the [LADE-specific application rules](docs/product/ui-principles.md#application-and-explicit-exceptions) still apply.

This specification makes no claim of a rendered GTK implementation or runtime visual/accessibility pass.

### Visual language: gray workbench

LADE is a dark, gray, desktop tool. Three ideas carry the whole interface:

- **Chrome and panes.** The window is one charcoal *chrome*. The header and the Objectives rail sit directly on it. The work area and the inspector are *panes*: rounded gray surfaces inset from the window edges and from each other by the same small gap, so the chrome shows as an even frame around them. Panes are separated by that gap, never by rules.
- **One tab row, tabs joined to their pane.** A single row of fixed tabs runs across the window on the chrome: Workbench, Activity and Terminal above the work-area pane, Overview, Evidence and Feedback above the inspector pane. The current tab of each pane takes the pane's fill and joins it, with a concave curve on either side where its edges meet the pane's top; the other tabs stay on the chrome. Tabs are never closed, reordered or added.
- **Surfaces by recipe, not by colour.** Every non-text element is one of five recipes: *chrome*, *pane*, *raised* (buttons, selects, popovers, the composer, drafts), *well* (sunken: text fields, search, segmented tracks, checkboxes) and *ghost* (no fill until hover: quiet buttons, rows, tabs). Raised surfaces have a hairline rim lit slightly brighter at the top and a short, soft shadow; wells have a rim and no shadow; panes have neither: their shape against the chrome is the edge.
- **One accent.** Salmon is the only hue in the chrome. It marks the available primary action, keyboard focus, checked controls, completed and current journey steps, the LADE mark and text selection. It is never a panel tint, row stripe, tab underline or decoration, and never the only signal of a state.

Selection and hover are fills, not inversions: hover lays the *hover* fill (white at 6%) over the ghost item; the current Objective, file, tab or menu row lays the *selected* fill (white at 10%) and moves its text to the strong ink. Text keeps its ladder: strong, primary, secondary and metadata ink are white at decreasing luminance. No gradients, textures, glows or glass.

### Non-negotiable visual corrections

- Header branding is the existing mark, drawn in the accent, and **LADE**, with no subtitle.
- No greetings, motivational copy, slogans or functionless cards. No bottom-left filler panel.
- Objective rows have **no lateral border, inset stripe, pseudo-stripe, shadow rail or colored accent bar**. Hover and selection are even fills on all sides, never a one-sided edge.
- Profile is one circular placeholder avatar and the local user's display name, vertically centered together. The name comes from the system account (real name, falling back to the login name). Do not invent a portrait or make sign-in part of this layout.
- Keep comments, code references, evidence, error explanations and visible state. They are functional content, not ornament.
- Preserve code/diff and state meaning through text, signs and appropriate semantic colors. A restrained palette does not mean neutralizing content.

## 2. Canvas and composition

Units below are logical pixels at 100% scaling. They are layout targets; GTK/Pango scaling must preserve usability rather than turn them into fixed physical pixels. Use the actual available content area, excluding window-manager decorations.

| Region | Wide-layout geometry | Treatment |
| --- | --- | --- |
| Header | 52 high, 16 left and 12 right inset, 16 between groups | On the chrome; no divider. Brand, connection status, search and profile; the view tabs live above their panes, not here |
| Objectives | 264 wide, adjustable approximately 224–280 | On the chrome; 10 left inset; one-line rows 28 high with 8 side padding, 1 apart; groups 24 apart; titles, group labels and the archive toggle share one text edge |
| Tab row | 36 high: 6 above the tabs, tabs 30 high flush with the pane top; each pane's strip starts 6 in from its edge | On the chrome above each pane; the current tab joins its pane with 10-pixel concave feet, which overlap between neighbouring tabs |
| Main work area | Remaining width; 32 inset (16 compact), 16 within related groups | A pane: surface fill, 14 radius; 8 from the window's bottom edge and 8 from the inspector pane. Dominant reading area, not a grid of unrelated cards |
| Right inspector | 344 wide, adjustable approximately 312–360; 20 inset, sections 32 apart; at the tight 312 width its three tabs must still fit | A pane like the work area with its own tabs in the same row, 8 from the window's right and bottom edges. Shown only when an Objective is selected; without one the work area takes the full width and its own 8 right gap |
| Changed-file list inside center | Approximately 192 wide when center permits | Ghost rows: file name on the first line, directory and line counts on the second |
| Code/diff viewport | Remaining center width | On the pane surface; path toolbar, gutters and footer unruled, aligned to the work area's text edge; semantic diff fills only |

At a 1440-wide reference window the center receives approximately 820 pixels before internal padding. Preserve relative dominance rather than exact proportions. Header groups share a vertical centerline. Keep global search beside the profile; give search approximately 280–360 pixels in the wide layout. Align titles, list text and inspector labels to their own consistent left edges; inspector tabs and the content below them start on the same edge.

Display the navigation destinations defined by the accepted product contract; do not invent new pages to fill the tab strip. Treat the collaboration journey as a restrained horizontal indicator, with short labels and modest connectors, not oversized navigation cards.

### Narrow and scaled layouts

At roughly 1100 logical pixels and below, use a compact version of the same workbench: reduce rail widths, present secondary rails in accessible collapsible/drawer surfaces, and keep the active code or feedback surface dominant. The compact region switcher is the segmented control; the visible pane keeps a 6-pixel gap to the window. At approximately 800 by 550 logical pixels, do not insist on three simultaneous fixed-width columns. Search remains reachable; the profile does not push anything offscreen, and each pane keeps its tabs above it.

These are responsive compositions, not a mobile-support promise. Do not remove functionality, shrink text below the type scale, clip actions or shrink the whole application with a visual transform. Let document content scroll within its viewport; allow deliberate horizontal scrolling for code. Long labels wrap or truncate with a way to inspect the full value under the functional contract. At 200% text/UI scaling, controls and focus must remain visible.

## 3. Color tokens

Define semantic token names centrally (`crates/lade-desktop/src/assets/style.css`); do not scatter color literals across components. Set alpha on the color, not on a parent widget and all its descendants.

### Surfaces

| Token | Value | Use |
| --- | --- | --- |
| chrome | `#141416` | Window background: header, Objectives rail, empty and connection states |
| surface | `#1C1C1F` | Pane fill: work area, inspector, code viewer, terminal, dialogs |
| raised / raised.hover / raised.pressed | `#27272B` / `#2E2E33` / `#232327` | Buttons, selects, the active segment, drafts, the composer, your activity bubbles, the avatar |
| well | `#161618` | Text fields, search, segmented tracks, unchecked checkboxes |
| popover | `#232327` | Popovers, menus, tooltips |
| hover fill | white at 6% | Ghost items under the pointer: quiet buttons, rows, tabs, menu rows |
| selected fill | white at 10% | Current Objective, file, tab, menu row |
| rim / rim.raised | white at 8% / 14% | Hairline edge of raised, well and floating surfaces; the brighter top edge of raised ones |
| hairline | white at 6% | In-list separators |

### Ink

| Token | Value | Use |
| --- | --- | --- |
| ink.strong | `#F4F4F5` | Titles, the current item, text on raised selections |
| ink.primary | `#E4E4E7` | Body text, controls |
| ink.secondary | `#B9B9C0` | Descriptions, readable supporting text, icons |
| ink.metadata | `#A4A4AC` | Functional metadata, inactive tabs, placeholders; at least 4.5:1 on every surface and fill above |
| ink.optional | `#6E6E76` | Journey connectors only, never text |

### Accent and indicators

| Token | Value | Use |
| --- | --- | --- |
| accent / accent.hover | `#F2937E` / `#F6A694` | Primary action fill, checked checkbox, done and current journey markers, the LADE mark |
| accent.ink | `#1B1311` | Text and check signs on the accent |
| focus | `#F2937E` | 2-pixel outline, 2-pixel offset where space permits; inset 2 pixels on tabs and rows that touch a pane edge |
| text selection | accent at 16% | Selected text, including over diff fills |
| control edge | white at 42% | Edge of checkboxes and upcoming journey markers (the only edges that must read as an indicator) |
| border.disabled | white at 6% | Edge of a disabled control |

Selection, hover and focus must not collapse into the same appearance: hover and selection are fills of different strength, the selected item also takes the strong ink and (for Objectives) semibold weight, and focus is the accent outline. Region dividers are not used; an Objective-side accent is never.

### Semantic content

| Meaning | Ink / fill / rim | Redundant visible encoding |
| --- | --- | --- |
| Addition / passed result | `#5BD08F` / `#132A1E` | `+` for diff lines; text/check for results |
| Deletion / error / destructive | `#FF6B81` / `#36161C` / ink at 32% | `−` for diff lines; text/error sign |
| Warning / stale context | `#F2BE4A` / `#2E2510` / ink at 32% | Explicit label and warning sign |

Deletion is a cool rose red, kept apart from the warmer salmon accent in hue and always paired with a sign or label. These are narrow semantic exceptions, not colored panel chrome. Syntax inks are `#B0B0B8` comments, `#8CC2F0` strings and `#C9A8F2` constants, each at least 4.5:1 on the code surface, both diff fills and the selection overlay. Restrict syntax weight and saturation so it does not compete with feedback selections or diff signs. Never use only color to distinguish added/deleted text, failure, focus or an active control. Passing computed pairs is not full accessibility conformance; rendered contrast must still be checked.

### Elevation

| Token | Drawing | Used by |
| --- | --- | --- |
| shadow.soft | `0 1px 2px` black at 28% | Raised controls; the composer adds `0 3px 10px` |
| shadow.popover | `0 8px 24px` black at 45% | Popovers, menus (`0 3px 9px` for tooltips) |
| shadow.dialog | `0 16px 40px` black at 60% | Dialogs |

Panes, rows, tabs, wells and text carry no shadow.

## 4. Typography, spacing and shape

UI text uses **Inter** when it is installed, falling back to the platform sans-serif; code, paths and terminal text use **JetBrains Mono** when installed, falling back to the platform monospace. Neither face is bundled: shipping font files needs an explicit dependency/license decision. Keep source code, names and acronyms in their original case; LADE remains uppercase.

| Role | Size / weight | Leading and treatment |
| --- | --- | --- |
| Objective title | 22 / 600, −0.2 tracking, strong ink | About 28; leads the work area without becoming a hero headline |
| Dialog title | 17 / 600 | About 22; in-content heading of every dialog |
| Empty-state heading | 17 / 600 | About 22 |
| Section title | 15 / 600 | About 20 |
| Group label (rail) | 12 / 500, metadata ink | About 16; sentence case |
| Field label | 13 / 500, secondary ink | About 18; labels form fields |
| UI body | platform size (14 at 100%) / 400 | About 19 (1.38); same baseline across peers |
| Controls and tabs | 13 / 500 | About 18 |
| Metadata | 12 / 500 | About 16; readable contrast, never below 12 |
| Code and diff | 12.5 monospace / 400 | About 20; aligned gutters and selection |
| Brand wordmark | 14 / 600, +0.6 tracking | Beside the accent mark, which is 20 by 18 |

Use available font weights honestly; do not assume every platform face has all weights. Respect user text scaling rather than clipping to fixed heights.

Spacing scale: **2, 4, 6, 8, 10, 12, 16, 20, 24, 32** (a 4-pixel base with 2-pixel half steps for control padding). Use 2–4 within a label/value pair, 8–12 within a component and 24–32 between unrelated sections. Put a dimension on one wrapper per axis to avoid double padding.

- **Whitespace groups.** A title sits closer to its own metadata (4) than to the next record (12 or more); groups of records sit 24 apart, inspector sections 32. Never spread lines evenly to fill a region.
- **Words keep their inset.** Text stays at least 10 from the edge of any row or field, 12 from a card, 20 from a pane edge in the inspector and 32 in the work area; carry the same inset round the bottom.
- **Rows breathe.** Objective rows are one line, 28 high with 8 side padding and 1 between; file rows 6 by 10 with 2 between; evidence rows 10 vertical; code lines 3 above and below.
- **Running text has leading and a measure.** Read-only prose (activity, intent, empty-state bodies) uses 1.38 line height; agent and system text wrap at about 88 characters, your messages at about 64.
- **Compact gives space back first.** Below the wide layout, insets drop to 16 and pane gaps to 6; rows keep their padding.

Radii by role, so surfaces agree without one-off values:

| Radius | Role |
| --- | --- |
| 4 | Checkboxes, focus rings around text |
| 6 | File rows, menu and result rows |
| 7 | Items inside a segmented track, tooltips |
| 8 | Buttons, selects, fields, search, Objective rows |
| 10 | Segmented tracks, inline messages, notices, the top corners of a tab and its concave feet |
| 12 | Popovers, drafts, error and warning boxes |
| 14 | Panes and dialogs |
| 16–17 | Activity bubbles and the composer |

Default control height is 32 (28 inside a segmented track, 30 for fields), expanding for scaled text; icon-only hit areas are at least 32 by 32. Dense file rows begin around 30 high; Objective rows are one line, 28 high. Status labels stay sign plus text, not pills, except in Objective rows, where the group heading names the status and the row's sign is backed by its tooltip and accessible name. Cards appear only where a record is a unit the person acts on (a feedback draft, the composer, an activity bubble); lists of peers stay ghost rows.

## 5. Component appearance

| Component | Visual contract |
| --- | --- |
| Objective row | One line: the title, truncated, and a small status sign at the right when the Objective has badges or is archived; the full title, state, badges and outcome in the tooltip and accessible name. Ghost row; unselected titles medium weight; hover fill; selected fill with a semibold title in strong ink; no lateral rule |
| Header/search | On the chrome; the mark in the accent beside the wordmark; search a well with a rim beside the aligned avatar/name group; no account greeting |
| Pane tabs (work area and inspector) | Fixed destinations, never closed, reordered or added. No track. Each tab is 30 high with 12 side padding, 13/500 text and 10-radius top corners. Inactive tabs use metadata ink on the chrome and take the hover fill under the pointer; the current tab takes the pane's fill and strong ink and joins the pane through its concave feet. Focus is an inset accent ring |
| Segmented control (Diff/Full, compact regions) | A well track (10 radius, 2 padding) holding ghost items (7 radius); the current item is raised with its rim and soft shadow and strong ink |
| File list | Ghost rows; file name first, directory dimmed below with change counts right-aligned beside it; full path in tooltip and accessible name; selected row on the selected fill; symbols distinguish file changes |
| Code/diff | Stable line-number/sign gutters, monospaced alignment, restrained syntax; semantically colored changed lines; selection remains visible over diff fill |
| Feedback reference | Compact file/range/version label above the comment text; no oversized author avatar or decorative quote card |
| Feedback draft | One raised card per draft (12 radius, rim, no shadow), holding reference, excerpt, comment field, save state and Remove on one text edge |
| Feedback composer | Well field marked by its placeholder and focus ring, readable draft text and small save-state line; comments remain visible and do not obscure the selected code |
| Activity transcript | No rule between items; items 12 apart. Your turns are right-aligned raised bubbles sized to their text; tool and system output plain under a role label, its output monospaced; thinking plain; agent text plain, labelled *agent claim*, wrapped at a readable measure |
| Activity composer | One raised card (17 radius, lit top rim, soft two-layer shadow) containing the borderless input, delivery mode, hint and Send; focus draws the accent ring round the whole card |
| Dialog | A pane-coloured surface with 14 radius, rim and dialog shadow; in-content heading, body, and actions at the bottom right with no rule above them; primary action last |
| Popover/menu | Popover fill, 12 radius, rim with a brighter top edge, popover shadow; menu rows are ghost rows with 6 radius |
| Evidence | Aligned name/result rows (candidates and checks share one shape: title, value at the right, metadata beneath), inspectable details and explicit failure/stale/missing labels; coverage displayed as a metric, not a success check |
| Primary/secondary actions | Primary: accent fill with accent ink, semibold. Secondary: raised. Quiet: ghost. Consistent dimensions; no permanently emphasized unavailable Integrate |
| Error/warning | Readable local message near the relevant control on its semantic fill and rim; sign and text; preserve room for correction rather than toast-only styling |
| Empty/loading | Functional heading or progress plus the relevant action (Select an Objective offers New Objective); no empty side columns, illustrations, motivational copy or perpetual decorative animation |
| Disclosure | Theme chevron in metadata ink beside a medium-weight secondary label; no text triangles |
| Destructive action | Raised with deletion ink and a deletion rim; hover takes the deletion fill; never visually identical to an ordinary neutral action |

The above describes how supplied state looks, not which action is allowed. A disabled action keeps its place and a readable explanation; lowering the opacity of an entire panel is not an acceptable substitute. Agent claims and observed evidence must remain visually distinguishable using labels/provenance, not merely icon color.

One coherent symbolic icon family in secondary ink, strong ink on hover; the accent is not an icon colour. Preserve legitimate content imagery and user-authored emoji. Do not invent a replacement for the existing logo or turn an unavailable profile asset into a fabricated face.

## 6. Visual states and motion

Capture default, hover, pressed, selected, focused, disabled, loading and error appearances. Also capture the supplied product states of unsaved/saving/saved/failed draft, stale reference, interrupted connection and unavailable integration. This document prescribes their visual distinction only.

| State | Ghost item | Raised control | Primary | Well |
| --- | --- | --- | --- | --- |
| Hover | Hover fill, ink brightens | raised.hover | accent.hover | Rim brightens (search) |
| Pressed | Selected fill | raised.pressed, no shadow | accent | — |
| Selected/checked | Selected fill, strong ink | raised.hover, strong ink | — | Accent fill (checkbox) |
| Focus | Accent outline | Accent outline | Accent outline outside the fill | Accent border plus inset 1 px ring |
| Disabled | Metadata ink, no fill | raised.pressed, disabled rim, metadata ink | Same as raised | Unchanged fill, metadata ink |

Default to no animation for typing, selection, frequent navigation or status streams. A short opacity/color transition up to about 120 milliseconds is an optional refinement, never a performance requirement. No animated panel entrances for keyboard navigation, no pulsing brand and no perpetual shimmer. Honor reduced motion; state remains visible without movement.

## 7. Visual acceptance sheet

The visual sheet must include the workbench, Objective list, file/diff selection, feedback, evidence and action group with realistic long text and all relevant states.

Capture the actual GTK application at 1440 by 900 and 1280 by 800 logical pixels, a compact window, and the supported desktop's real 200% text/UI scaling. The ignored `capture_fixture_views` test renders populated fixture views at 1440 by 900 and 800 by 550 into `LADE_CAPTURE_DIR` for before/after comparison; it does not replace the scaled and assistive-technology passes. Review label/avatar vertical alignment, matched control dimensions, the even chrome gap round the panes, seamless current tabs (no hairline, gap or colour step where a tab, its feet and its pane meet, at 100% and 200%), readable small text, full focus outlines, that the accent appears only in its listed roles, and the absence of lateral Objective accents.

Measure text contrast against actual composed surfaces: normal functional text at least 4.5:1, eligible large text 3:1, and required non-text indicators 3:1 under the accepted accessibility contract. Compare source/diff signs and selection in combination. Check actual hover/focus/pressed rendering, not declarations alone. The full keyboard/Orca acceptance belongs to the functional checklist, not a claim made by screenshots.

Record what was inspected and what remains unverified. A mockup, HTML-only prototype, CSS inspection or pure color calculation is not a native-application pass. Change a numerical token deliberately when legibility or the native component requires it, and retain before/after evidence.
