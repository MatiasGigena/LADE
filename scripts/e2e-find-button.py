#!/usr/bin/env python3
"""Finds a button in an X screen capture, for the e2e scripts (test tooling, not shipped).

    import -window root -depth 8 ppm:- | scripts/e2e-find-button.py black X1 X2 Y_MIN Y_MAX
    import -window root -depth 8 ppm:- | scripts/e2e-find-button.py label TEMPLATE X Y_MIN Y_MAX

Prints the vertical centre of the first match from the top; exit status 1 if there is none.

- `black`: a band in which every pixel row is black (≤ 40 in each channel) at both x = X1 and
  x = X2 for at least 20 rows — a filled primary button such as "Start agent".
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


def main():
    args = sys.argv[1:]
    width, height, pixels = read_ppm(sys.stdin.buffer.read())
    if len(args) == 5 and args[0] == 'black':
        found = find_black(width, height, pixels, *map(int, args[1:]))
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
