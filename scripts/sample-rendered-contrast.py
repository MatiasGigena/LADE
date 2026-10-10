#!/usr/bin/env python3
"""Optional: sample contrast on a real native capture (needs Pillow; not part of scripts/verify).

    scripts/sample-rendered-contrast.py CAPTURE.png X,Y,W,H [X,Y,W,H ...]

For each region (physical pixels of the capture) the most frequent colour is the background and
the ink is the colour with the strongest contrast against it among colours covering at least 2 %
of the region (so antialiased glyph edges are ignored); their WCAG ratio is printed as JSON.
Choose regions that contain one text run or one indicator on one fill (for example a metadata label, an error line or a focus
outline). This samples rendered pixels, which scripts/check-contrast.py cannot; it is still not an
Orca pass or a human visual review. Very thin text may have no solid ink pixel at all and then
reports a blended colour, which understates the contrast.
"""
from collections import Counter
import json
from pathlib import Path
import sys

from PIL import Image


def luminance(rgb):
    linear = [c / 255 / 12.92 if c / 255 <= 0.04045 else ((c / 255 + 0.055) / 1.055) ** 2.4 for c in rgb]
    return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2]


def ratio(a, b):
    high, low = sorted((luminance(a), luminance(b)), reverse=True)
    return (high + 0.05) / (low + 0.05)


if len(sys.argv) < 3:
    sys.exit(__doc__)
image = Image.open(Path(sys.argv[1])).convert('RGB')
samples = []
for spec in sys.argv[2:]:
    x, y, w, h = (int(v) for v in spec.split(','))
    if w <= 0 or h <= 0 or x + w > image.width or y + h > image.height:
        sys.exit(f'region {spec} is outside the {image.width}x{image.height} capture')
    region = image.crop((x, y, x + w, y + h))
    pixels = list(region.get_flattened_data() if hasattr(region, 'get_flattened_data') else region.getdata())
    counts = Counter(pixels)
    background = counts.most_common(1)[0][0]
    floor = max(3, len(pixels) // 50)
    candidates = [c for c, n in counts.items() if n >= floor and c != background]
    if not candidates:
        samples.append({'region': spec, 'background': list(background), 'ink': None, 'ratio': None})
        continue
    ink = max(candidates, key=lambda c: ratio(c, background))
    samples.append({'region': spec, 'background': list(background), 'ink': list(ink),
                    'ratio': round(ratio(background, ink), 2)})
print(json.dumps({'capture': sys.argv[1], 'size': [image.width, image.height], 'samples': samples}, indent=2))
