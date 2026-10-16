#!/usr/bin/env python3
"""Draw the Nimbus app icon and write it in the sizes each platform needs.

The icon is a cloud outline in the editor's cursor amber, with a text
cursor inside, on a dark rounded square: the editor's own colours. It is
drawn here from simple shapes (circles and rectangles), so it needs nothing
beyond the Python standard library; edges are smoothed by taking several
samples per pixel.

Writes, into assets/icon/:
  nimbus-1024.png             the master image
  nimbus.icns                 macOS (with `iconutil`, on macOS only)
  nimbus.ico                  Windows (PNG images packed in an ICO file)

Run from the repository root: python3 scripts/make-icons.py
"""

import math
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import zlib

BACKGROUND = (0x1A, 0x1C, 0x22)
BORDER = (0x2C, 0x30, 0x3A)
AMBER = (0xFA, 0xBF, 0x4C)
CURSOR = (0xD9, 0xDE, 0xE8)

# Shapes on a 1024-pixel canvas.
SQUARE = (64, 64, 960, 960, 200)  # x0, y0, x1, y1, corner radius
BORDER_WIDTH = 8
DOWN = 30  # the cloud and cursor sit this far below their drawn position
CLOUD_CIRCLES = [(300, 575, 100), (440, 490, 130), (590, 455, 165), (730, 575, 100)]
CLOUD_BODY = (300, 500, 730, 675)  # fills the cloud between the circles
STROKE = 48
CURSOR_BAR = (496, 455, 528, 635, 12)


def rounded_rect_sd(x, y, rect):
    """Signed distance to a rounded rectangle: negative inside."""
    x0, y0, x1, y1, r = rect
    cx, cy = (x0 + x1) / 2, (y0 + y1) / 2
    hx, hy = (x1 - x0) / 2 - r, (y1 - y0) / 2 - r
    dx, dy = abs(x - cx) - hx, abs(y - cy) - hy
    outside = math.hypot(max(dx, 0), max(dy, 0))
    return outside + min(max(dx, dy), 0) - r


def cloud_sd(x, y):
    """Signed distance to the cloud, as the union of its shapes."""
    y -= DOWN
    d = min(math.hypot(x - cx, y - cy) - r for cx, cy, r in CLOUD_CIRCLES)
    # The body's sides run through the side circles' centres, hidden
    # inside them, so only its top and bottom count; otherwise the stroke
    # would show notches where its sides meet the circles.
    x0, y0, x1, y1 = CLOUD_BODY
    if x0 <= x <= x1:
        d = min(d, max(y - y1, y0 - y))
    return d


def colour_at(x, y):
    """The colour and coverage (0 or 1) of one sample."""
    square = rounded_rect_sd(x, y, SQUARE)
    if square > 0:
        return None
    if abs(cloud_sd(x, y)) <= STROKE / 2:
        return AMBER
    x0, y0, x1, y1, r = CURSOR_BAR
    if rounded_rect_sd(x, y - DOWN, (x0, y0, x1, y1, r)) <= 0:
        return CURSOR
    if square > -BORDER_WIDTH:
        return BORDER
    return BACKGROUND


def render(size, samples=3):
    """RGBA bytes of the icon at `size` pixels, `samples`² per pixel."""
    scale = 1024 / size
    out = bytearray()
    n = samples * samples
    for py in range(size):
        for px in range(size):
            r = g = b = a = 0
            for sy in range(samples):
                for sx in range(samples):
                    x = (px + (sx + 0.5) / samples) * scale
                    y = (py + (sy + 0.5) / samples) * scale
                    c = colour_at(x, y)
                    if c:
                        r += c[0]
                        g += c[1]
                        b += c[2]
                        a += 1
            if a:
                out += bytes((r // a, g // a, b // a, 255 * a // n))
            else:
                out += b"\0\0\0\0"
    return bytes(out)


def png(size, rgba):
    """A PNG file of `size`×`size` RGBA pixels."""
    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    rows = b"".join(
        b"\0" + rgba[y * size * 4:(y + 1) * size * 4] for y in range(size)
    )
    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header)
            + chunk(b"IDAT", zlib.compress(rows, 9)) + chunk(b"IEND", b""))


def ico(images):
    """An ICO file holding PNG `images`, a list of (size, png bytes)."""
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries, data = b"", b""
    for size, blob in images:
        dim = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(blob), offset)
        offset += len(blob)
        data += blob
    return header + entries + data


def main():
    out = os.path.join("assets", "icon")
    os.makedirs(out, exist_ok=True)
    pngs = {}
    for size in (16, 32, 48, 64, 128, 256, 512, 1024):
        print(f"drawing {size}px", file=sys.stderr)
        pngs[size] = png(size, render(size, samples=4 if size <= 64 else 3))
    with open(os.path.join(out, "nimbus-1024.png"), "wb") as f:
        f.write(pngs[1024])
    with open(os.path.join(out, "nimbus.ico"), "wb") as f:
        f.write(ico([(s, pngs[s]) for s in (16, 32, 48, 64, 128, 256)]))
    if shutil.which("iconutil"):
        with tempfile.TemporaryDirectory() as tmp:
            iconset = os.path.join(tmp, "nimbus.iconset")
            os.makedirs(iconset)
            for size in (16, 32, 128, 256, 512):
                for scale in (1, 2):
                    name = f"icon_{size}x{size}{'@2x' if scale == 2 else ''}.png"
                    with open(os.path.join(iconset, name), "wb") as f:
                        f.write(pngs[size * scale])
            subprocess.run(
                ["iconutil", "-c", "icns", iconset, "-o", os.path.join(out, "nimbus.icns")],
                check=True,
            )
    else:
        print("iconutil not found (it is macOS only): nimbus.icns not written", file=sys.stderr)


if __name__ == "__main__":
    main()
