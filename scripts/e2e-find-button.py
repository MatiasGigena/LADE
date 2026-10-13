#!/usr/bin/env python3
"""Finds a button in an X screen capture, for the e2e scripts (test tooling, not shipped).

    import -window root -depth 8 ppm:- | scripts/e2e-find-button.py black X1 X2 Y_MIN Y_MAX
    import -window root -depth 8 ppm:- | scripts/e2e-find-button.py edge INK X1 X2 Y_MIN Y_MAX
    import -window root -depth 8 ppm:- | scripts/e2e-find-button.py text INK X1 X2 Y_MIN Y_MAX
    import -window root -depth 8 ppm:- | scripts/e2e-find-button.py label TEMPLATE X Y_MIN Y_MAX

Prints the vertical centre of the first match from the top; exit status 1 if there is none.

- `black`: a band in which every pixel row is black (≤ 40 in each channel) at both x = X1 and
  x = X2 for at least 20 rows — a filled primary button such as "Start agent".
- `edge`: `edge INK X1 X2 Y_MIN Y_MAX` — like `black`, for a full-width *outlined* button: its
  1-px left and right borders (within 2 px of x = X1 and x = X2) have the ink for at least 20
  rows. INK is `outline` (the neutral control boundary, an enabled neutral button such as
  "Approve Candidate 1" when it needs an override) or `red` (a destructive next action such as
  "Clean up branch and worktree"). A disabled button draws its border in another ink.
- `text`: `text INK X1 X2 Y_MIN Y_MAX` — the first line of text in that ink between x = X1 and
  x = X2 (at least 3 such pixels on each of 8 consecutive rows), for a quiet list action whose
  label is the only text in that ink, such as the red "Delete Git history" in More actions once
  every other destructive action is done or disabled.
- `box`: `box X_MIN X_MAX Y_MIN Y_MAX` — the first filled black button anywhere in that area
  (black over at least 50 px on each of 20 consecutive rows), printed as "X Y" (its centre),
  for a black button whose horizontal position varies, such as Activity's "Start agent" (in the
  "Set up the agent" card) or "Send".
- `label`: an exact pixel match of TEMPLATE (any image ImageMagick reads, a crop of an *enabled*
  button's label band from an earlier capture under the same Xvfb, font and scale) with its left
  edge at x = X. A disabled button draws its label in another ink and does not match.

Inspector buttons move as the inspector scrolls and as reasons wrap; matching them by pixels
keeps the scripts from clicking a stale position. Rendering must be identical to when the
template was cut (same GTK, fonts and 1440x900 Xvfb screen); re-cut the templates otherwise.
"""
import subprocess
import sys


def read_ppm(data):
    fields = []
    pos = 0
    while len(fields) < 4:
        while data[pos:pos + 1].isspace():
            pos += 1
        if data[pos:pos + 1] == b'#':
            while data[pos:pos + 1] not in (b'\n', b''):
                pos += 1
            continue
        start = pos
        while data[pos:pos + 1] and not data[pos:pos + 1].isspace():
            pos += 1
        fields.append(data[start:pos])
    if fields[0] != b'P6' or int(fields[3]) != 255:
        sys.exit('e2e-find-button: expected an 8-bit binary PPM')
    return int(fields[1]), int(fields[2]), data[pos + 1:]


def find_black(width, height, pixels, x1, x2, y_min, y_max):
    def black(x, y):
        offset = (y * width + x) * 3
        return max(pixels[offset:offset + 3]) <= 40

    run = 0
    for y in range(y_min, min(y_max, height)):
        if black(x1, y) and black(x2, y):
            run += 1
            continue
        if run >= 20:
            return y - run // 2 - 1
        run = 0
    return min(y_max, height) - run // 2 - 1 if run >= 20 else None


# Border inks of style.css (`border_control`, `del_ink`), with a tolerance for antialiasing.
EDGE_INKS = {'outline': ((112, 112, 112), 24), 'red': ((180, 35, 24), 40)}


def find_edge(width, height, pixels, ink, x1, x2, y_min, y_max):
    colour, tolerance = EDGE_INKS[ink]

    def inked(x, y):
        offset = (y * width + x) * 3
        return all(abs(c - k) <= tolerance for c, k in zip(pixels[offset:offset + 3], colour))

    def edge(x, y):
        return any(inked(c, y) for c in range(max(0, x - 2), min(width, x + 3)))

    run = 0
    for y in range(y_min, min(y_max, height)):
        if edge(x1, y) and edge(x2, y):
            run += 1
            continue
        if run >= 20:
            return y - run // 2 - 1
        run = 0
    return min(y_max, height) - run // 2 - 1 if run >= 20 else None


def find_text(width, height, pixels, ink, x1, x2, y_min, y_max):
    colour, tolerance = EDGE_INKS[ink]

    def inked(r, g, b):
        # Antialiased red glyphs are lighter than the ink: any clearly red pixel counts.
        if ink == 'red':
            return r - g > 60 and r - b > 60
        return all(abs(c - k) <= tolerance for c, k in zip((r, g, b), colour))

    def line(y):
        count = 0
        for x in range(x1, min(x2, width)):
            offset = (y * width + x) * 3
            if inked(*pixels[offset:offset + 3]):
                count += 1
        return count >= 3

    run = 0
    for y in range(y_min, min(y_max, height)):
        if line(y):
            run += 1
            continue
        if run >= 8:
            return y - run // 2 - 1
        run = 0
    return None


def find_label(width, height, pixels, template, x, y_min, y_max):
    t_width, t_height, t_pixels = template
    rows = [t_pixels[r * t_width * 3:(r + 1) * t_width * 3] for r in range(t_height)]

    def row(y):
        offset = (y * width + x) * 3
        return pixels[offset:offset + t_width * 3]

    for y in range(y_min, min(y_max, height) - t_height):
        if all(row(y + r) == rows[r] for r in range(t_height)):
            return y + t_height // 2
    return None


def find_box(width, height, pixels, x_min, x_max, y_min, y_max):
    """The first filled black box (black pixels spanning at least 50 px, at least 30 of them
    black — white label glyphs between them — on at least 20 consecutive rows) in the area; its
    centre as "X Y". A short label such as "Start agent" leaves fewer than 50 black pixels on
    the rows through its glyphs, so the span decides, not the count."""
    def run(y):
        black = [x for x in range(x_min, min(x_max, width))
                 if max(pixels[(y * width + x) * 3:(y * width + x) * 3 + 3]) <= 40]
        if len(black) < 30 or black[-1] - black[0] + 1 < 50:
            return None
        return (black[0], black[-1] - black[0] + 1)

    rows = []
    for y in range(y_min, min(y_max, height)):
        found = run(y)
        if found:
            rows.append((y, found))
            continue
        if len(rows) >= 20:
            break
        rows = []
    if len(rows) < 20:
        return None
    y = rows[len(rows) // 2][0]
    start, length = rows[len(rows) // 2][1]
    return f'{start + length // 2} {y}'


def main():
    args = sys.argv[1:]
    width, height, pixels = read_ppm(sys.stdin.buffer.read())
    if len(args) == 5 and args[0] == 'black':
        found = find_black(width, height, pixels, *map(int, args[1:]))
    elif len(args) == 6 and args[0] == 'edge' and args[1] in EDGE_INKS:
        found = find_edge(width, height, pixels, args[1], *map(int, args[2:]))
    elif len(args) == 6 and args[0] == 'text' and args[1] in EDGE_INKS:
        found = find_text(width, height, pixels, args[1], *map(int, args[2:]))
    elif len(args) == 5 and args[0] == 'box':
        found = find_box(width, height, pixels, *map(int, args[1:]))
    elif len(args) == 5 and args[0] == 'label':
        ppm = subprocess.run(['convert', args[1], '-depth', '8', 'ppm:-'], check=True,
                             capture_output=True).stdout
        found = find_label(width, height, pixels, read_ppm(ppm), *map(int, args[2:]))
    else:
        sys.exit(__doc__.strip().splitlines()[2].strip())
    if found is None:
        sys.exit(1)
    print(found)


if __name__ == '__main__':
    main()
