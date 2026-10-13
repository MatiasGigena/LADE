#!/usr/bin/env python3
"""Contrast of the desktop client's colour tokens (pure computation; no display needed).

Ported from the IMP-002 checkpoint's check-component-contrast.py and adapted to the single
stylesheet crates/lade-desktop/src/assets/style.css. It checks that:

- every colour in the stylesheet is a named @define-color token (no scattered literals);
- the colours the viewer sets from Rust (diff signs, line fills) are the same tokens;
- every colour of the syntax scheme (assets/code-scheme.xml) is the style.css token of the same
  name and value, and each syntax ink stays readable on the code surface, the diff fills and the
  selection overlay;
- each specified ink/fill pair, alpha-composited onto the white surface as GTK draws it, meets
  WCAG 2.2 AA: 4.5:1 for functional text, 3:1 for required non-text indicators (control
  boundaries, focus outline, the inverse focus ring of black buttons, journey markers).

This verifies token pairs only. It is not a rendered-surface measurement, an Orca pass or a
blanket accessibility conformance claim; see scripts/sample-rendered-contrast.py for optional
pixel sampling of real captures.
"""
import json
from pathlib import Path
import re
import sys

root = Path(__file__).resolve().parent.parent
css_path = root / 'crates/lade-desktop/src/assets/style.css'
viewer_path = root / 'crates/lade-desktop/src/views/viewer.rs'
scheme_path = root / 'crates/lade-desktop/src/assets/code-scheme.xml'
css = css_path.read_text()
errors = []


def parse(value):
    value = value.strip()
    if re.fullmatch(r'#[0-9A-Fa-f]{6}', value):
        return tuple(int(value[i:i + 2], 16) / 255 for i in (1, 3, 5)) + (1.0,)
    match = re.fullmatch(r'rgba\(\s*(\d+),\s*(\d+),\s*(\d+),\s*([\d.]+)\s*\)', value)
    if match:
        r, g, b, a = match.groups()
        return (int(r) / 255, int(g) / 255, int(b) / 255, float(a))
    raise SystemExit(f'unsupported colour value {value!r}')


tokens = {name: parse(value) for name, value in re.findall(r'^@define-color (\w+) ([^;]+);', css, re.M)}
for number, line in enumerate(css.splitlines(), 1):
    if not line.startswith('@define-color') and re.search(r'#[0-9A-Fa-f]{3,8}\b|rgba?\(', line):
        errors.append(f'style.css:{number}: colour literal outside @define-color: {line.strip()}')


def composite(fg, bg):
    alpha = fg[3]
    return tuple(fg[i] * alpha + bg[i] * (1 - alpha) for i in range(3)) + (1.0,)


def rgb(color):
    return [round(c * 255) for c in color[:3]]


def luminance(color):
    linear = [c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4 for c in color[:3]]
    return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2]


def ratio(a, b):
    high, low = sorted((luminance(a), luminance(b)), reverse=True)
    return (high + 0.05) / (low + 0.05)


def solid(*layers):
    """Composes token layers bottom-up onto the white surface."""
    color = tokens['surface']
    for layer in layers:
        color = composite(tokens[layer] if isinstance(layer, str) else layer, color)
    return color


# The viewer's diff colours are set from Rust (GtkTextTag / gutter markup): it reads them from
# style.css by name (`token("…")`), so every colour name it uses must be a token and any literal a
# token value. Its tests (which also probe a missing name) are not scanned.
viewer = viewer_path.read_text().split('#[cfg(test)]')[0]
token_hex = {('#%02X%02X%02X' % tuple(rgb(v))): k for k, v in tokens.items() if v[3] == 1.0}
viewer_colours = {}
for literal in sorted(set(re.findall(r'#[0-9A-Fa-f]{6}', viewer))):
    name = token_hex.get(literal.upper())
    viewer_colours[literal] = name
    if name is None:
        errors.append(f'viewer.rs: {literal} is not a style.css token')
for name in sorted(set(re.findall(r'"((?:add|del|warn|ink|fill)_\w+)"', viewer))):
    viewer_colours[name] = name if name in tokens else None
    if name not in tokens:
        errors.append(f'viewer.rs: colour "{name}" is not a style.css token')

# The syntax scheme names its colours; each must be the style.css token of that name and value,
# and every style may use only those named colours (no literal foreground/background).
scheme = scheme_path.read_text()
scheme_colours = dict(re.findall(r'<color name="(\w+)" value="([^"]+)"', scheme))
for name, value in scheme_colours.items():
    if name not in tokens:
        errors.append(f'code-scheme.xml: colour {name} is not a style.css token')
    elif parse(value) != tokens[name]:
        errors.append(f'code-scheme.xml: {name} {value} differs from the style.css token')
for attribute, value in re.findall(r'(foreground|background)="([^"]+)"', scheme):
    if value not in scheme_colours:
        errors.append(f'code-scheme.xml: {attribute}="{value}" is not a named token colour')
syntax_inks = sorted(set(re.findall(r'foreground="(\w+)"', scheme)))

TEXT, NON_TEXT = 4.5, 3.0
text_fills = ['surface', 'fill_hover', 'fill_selected', 'fill_band', 'fill_inset', 'fill_user']
pairs = []
for ink in ['ink_primary', 'ink_secondary', 'ink_metadata']:
    for fill in text_fills:
        pairs.append((ink, [fill], TEXT, 'text'))
pairs += [
    # Semantic inks on their fills and on plain surfaces (diff signs, results, errors, warnings).
    ('add_ink', ['add_fill'], TEXT, 'text'),
    ('add_ink', ['surface'], TEXT, 'text'),
    ('del_ink', ['del_fill'], TEXT, 'text'),
    ('del_ink', ['surface'], TEXT, 'text'),
    ('warn_ink', ['warn_fill'], TEXT, 'text'),
    ('warn_ink', ['surface'], TEXT, 'text'),
    ('ink_metadata', ['warn_fill'], TEXT, 'text'),
    # Selected text stays readable over plain and diff fills.
    ('ink_primary', ['text_selection'], TEXT, 'text'),
    ('ink_primary', ['add_fill', 'text_selection'], TEXT, 'text'),
    ('ink_primary', ['del_fill', 'text_selection'], TEXT, 'text'),
    # Primary (black) buttons: white label in every state.
    ('surface', ['ink_primary'], TEXT, 'text'),
    ('surface', ['primary_hover'], TEXT, 'text'),
    ('surface', ['primary_pressed'], TEXT, 'text'),
    # Destructive buttons: label on hover fill.
    ('del_ink', ['del_fill'], TEXT, 'text'),
    # Inline messages (views::widgets::inline_message): text and its Details toggle on hover.
    ('ink_primary', ['fill_band', 'fill_hover'], TEXT, 'text'),
    ('warn_ink', ['warn_fill', 'fill_hover'], TEXT, 'text'),
    ('del_ink', ['del_fill', 'fill_hover'], TEXT, 'text'),
    # Syntax inks (code-scheme.xml) on the code surface, the diff line fills and the selection.
    *[(ink, fills, TEXT, 'syntax text') for ink in syntax_inks
      for fills in (['surface'], ['add_fill'], ['del_fill'], ['text_selection'],
                    ['add_fill', 'text_selection'], ['del_fill', 'text_selection'])],
    # Required non-text indicators.
    ('border_control', ['surface'], NON_TEXT, 'control boundary'),
    ('border_control', ['fill_hover'], NON_TEXT, 'control boundary'),
    ('border_control', ['fill_selected'], NON_TEXT, 'control boundary'),
    ('focus_ring', ['surface'], NON_TEXT, 'focus outline'),
    ('focus_ring', ['fill_band'], NON_TEXT, 'focus outline'),
    ('focus_ring', ['fill_selected'], NON_TEXT, 'focus outline'),
    ('focus_ring', ['warn_fill'], NON_TEXT, 'focus outline'),
    ('focus_ring', ['del_fill'], NON_TEXT, 'focus outline'),
    ('surface', ['ink_primary'], NON_TEXT, 'inverse focus ring on black buttons'),
    ('ink_optional', ['surface'], NON_TEXT, 'upcoming journey marker'),
    ('ink_secondary', ['surface'], NON_TEXT, 'done journey marker'),
    ('surface', ['ink_secondary'], NON_TEXT, 'check sign on a done journey marker'),
    ('ink_primary', ['surface'], NON_TEXT, 'checked tab / nav underline'),
]
results = []
for ink, fills, threshold, kind in pairs:
    background = solid(*fills)
    foreground = composite(tokens[ink], background)
    measured = ratio(foreground, background)
    results.append({'ink': ink, 'on': ' + '.join(fills), 'kind': kind,
                    'composed_rgb': [rgb(foreground), rgb(background)],
                    'ratio': round(measured, 2), 'threshold': threshold})
    if measured < threshold:
        errors.append(f'{ink} on {" + ".join(fills)} ({kind}): {measured:.2f} < {threshold}')

print(json.dumps({'stylesheet': str(css_path.relative_to(root)), 'tokens': len(tokens) - 1,
                  'viewer_colours': viewer_colours,
                  'syntax_scheme': {'colours': scheme_colours, 'inks': syntax_inks}, 'pairs': results,
                  'limits': 'Computed token pairs only; rendered contrast, Orca and human review are separate.'},
                 indent=2))
if errors:
    sys.exit('contrast check failed:\n' + '\n'.join(errors))
