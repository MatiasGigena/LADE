#!/usr/bin/env python3
"""Contrast of the desktop client's colour tokens (pure computation; no display needed).

Ported from the IMP-002 checkpoint's check-component-contrast.py and adapted to the single
stylesheet crates/lade-desktop/src/assets/style.css. It checks that:

- every colour in the stylesheet is a named @define-color token (no scattered literals);
- the colours the viewer sets from Rust (diff signs, line fills) are the same tokens;
- every colour of the syntax scheme (assets/code-scheme.xml) is the style.css token of the same
  name and value, and each syntax ink stays readable on the code surface, the diff fills and the
  selection overlay;
- each specified ink/fill pair, alpha-composited onto the opaque surface it sits on (the window
  chrome, the pane surface, a raised control, a sunken well or a popover) as GTK draws it, meets
  WCAG 2.2 AA: 4.5:1 for functional text, 3:1 for required non-text indicators (focus outline,
  checkbox and journey-marker edges, the accent fill of checked controls).

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

# A token pair only holds if the ink is drawn at full opacity. A rule that sets a token colour
# must not dim it, and rules for GTK's placeholder node must reset opacity to 1 because themes
# dim placeholders (Mint-Y-Aqua rendered @ink_metadata at about 1.9:1).
for match in re.finditer(r'([^{}]+)\{([^{}]*)\}', css):
    selector, body = ' '.join(match.group(1).split()), match.group(2)
    number = css.count('\n', 0, match.start(2)) + 1
    opacity = re.search(r'(?<![-\w])opacity:\s*([\d.]+)', body)
    if opacity and float(opacity.group(1)) < 1 and re.search(r'(?<![-\w])color:\s*@\w+', body):
        errors.append(f'style.css:{number}: {selector} dims its token colour with opacity {opacity.group(1)}')
    if re.search(r'(?<![-.\w])placeholder\b', selector) and (not opacity or float(opacity.group(1)) != 1):
        errors.append(f'style.css:{number}: {selector} must set opacity: 1 (themes dim placeholders)')


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
    """Composes token layers bottom-up; the first layer is an opaque surface token."""
    base, *rest = layers
    color = tokens[base]
    if color[3] != 1.0:
        errors.append(f'{base} is not an opaque surface')
    for layer in rest:
        color = composite(tokens[layer], color)
    return color


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
# Every opaque surface text can sit on, with the hover and selected fills composited where rows
# and tabs use them.
grounds = [['chrome'], ['chrome', 'hover_fill'], ['chrome', 'selected_fill'], ['surface'],
           ['surface', 'hover_fill'], ['surface', 'selected_fill'], ['raised'], ['raised_hover'],
           ['raised_pressed'], ['well'], ['popover'], ['popover', 'hover_fill'],
           ['popover', 'selected_fill']]
pairs = [(ink, ground, TEXT, 'text') for ink in ['ink_strong', 'ink_primary', 'ink_secondary', 'ink_metadata']
         for ground in grounds]
pairs += [
    *[(ink, ground, TEXT, 'semantic text') for ink in ['add_ink', 'del_ink', 'warn_ink']
      for ground in (['surface'], ['chrome'], ['raised'], ['well'], ['popover'])],
    ('add_ink', ['surface', 'add_fill'], TEXT, 'text'),
    ('del_ink', ['surface', 'del_fill'], TEXT, 'text'),
    ('warn_ink', ['surface', 'warn_fill'], TEXT, 'text'),
    ('del_ink', ['raised', 'del_fill'], TEXT, 'destructive hover'),
    ('ink_metadata', ['surface', 'warn_fill'], TEXT, 'text'),
    ('ink_primary', ['surface', 'hover_fill'], TEXT, 'info message'),
    ('ink_strong', ['surface', 'text_selection'], TEXT, 'text'),
    ('ink_primary', ['surface', 'text_selection'], TEXT, 'text'),
    ('ink_primary', ['add_fill', 'text_selection'], TEXT, 'text'),
    ('ink_primary', ['del_fill', 'text_selection'], TEXT, 'text'),
    ('accent_ink', ['accent'], TEXT, 'text on the primary action'),
    ('accent_ink', ['accent_hover'], TEXT, 'text on the hovered primary action'),
    *[(ink, fills, TEXT, 'syntax text') for ink in syntax_inks
      for fills in (['surface'], ['add_fill'], ['del_fill'], ['surface', 'text_selection'],
                    ['add_fill', 'text_selection'], ['del_fill', 'text_selection'])],
    *[('focus_ring', ground, NON_TEXT, 'focus outline') for ground in grounds],
    ('focus_ring', ['surface', 'warn_fill'], NON_TEXT, 'focus outline'),
    ('focus_ring', ['surface', 'del_fill'], NON_TEXT, 'focus outline'),
    ('control_edge', ['well'], NON_TEXT, 'checkbox edge'),
    ('control_edge', ['surface'], NON_TEXT, 'upcoming journey marker'),
    ('accent', ['well'], NON_TEXT, 'checked checkbox fill'),
    ('accent', ['surface'], NON_TEXT, 'done and current journey markers'),
    ('accent_ink', ['accent'], NON_TEXT, 'check sign on a checked control or done marker'),
    ('accent', ['chrome'], NON_TEXT, 'the LADE mark'),
    ('ink_secondary', ['chrome'], NON_TEXT, 'icon ink'),
    ('ink_secondary', ['surface'], NON_TEXT, 'icon ink'),
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
