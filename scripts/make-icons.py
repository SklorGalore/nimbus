#!/usr/bin/env python3
"""Draw the Nimbus app icon and write it in the sizes each platform needs.

The icon is a white cloud with a dark text cursor (an I-beam, the shape
the mouse pointer takes over text) drawn over it, on a rounded square in
the editor's cursor amber. Each shape is filled with a gentle
top-to-bottom gradient. Solid shapes rather than outlines, because the
icon is mostly seen at 16 and 32 pixels, where thin strokes blur away; the
I-beam's caps are what make the bar read as a cursor and not a seam.

It is drawn here from simple shapes (circles and rounded rectangles), so it
needs nothing beyond the Python standard library; edges are smoothed by
taking several samples per pixel.

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

# Each fill is a vertical gradient: (colour at the top, colour at the bottom).
TILE = ((0xFF, 0xCF, 0x66), (0xF0, 0x9A, 0x2E))
CLOUD = ((0xFF, 0xFF, 0xFF), (0xFB, 0xE9, 0xCF))
BEAM = ((0x2B, 0x2F, 0x3B), (0x1A, 0x1C, 0x22))

# Shapes on a 1024-pixel canvas. The square follows Apple's icon grid (an
# 824-pixel square with a 100-pixel margin), so the icon is the same size
# as its neighbours in the Dock; Windows and Linux show it the same way.
SQUARE = (100, 100, 924, 924, 185)  # x0, y0, x1, y1, corner radius
CLOUD_CIRCLES = [(301, 605, 104), (425, 514, 128), (596, 481, 166), (733, 605, 104)]
CLOUD_BODY = (301, 552, 733, 709)  # fills the cloud between the circles
# The I-beam: a stem with a cap at each end. The caps sit clear of the
# cloud, 23 pixels above its highest point and below its base, so they
# read as the cursor's ends rather than cutting into the cloud's outline;
# the cloud and I-beam together are centred in the square.
BEAM_X = 560  # centre of the stem, a little right of centre, like a cursor mid-word
BEAM_TOP, BEAM_BOTTOM = 254, 770
STEM_WIDTH = 42
CAP_HEIGHT, CAP_WIDTH = 38, 162


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
    d = min(math.hypot(x - cx, y - cy) - r for cx, cy, r in CLOUD_CIRCLES)
    # The body's sides run through the side circles' centres, hidden
    # inside them, so only its top and bottom count.
    x0, y0, x1, y1 = CLOUD_BODY
    if x0 <= x <= x1:
        d = min(d, max(y - y1, y0 - y))
    return d


def beam_sd(x, y):
    """Signed distance to the I-beam: its stem and two caps."""
    half_stem, half_cap = STEM_WIDTH / 2, CAP_WIDTH / 2
    stem = (BEAM_X - half_stem, BEAM_TOP, BEAM_X + half_stem, BEAM_BOTTOM, half_stem)
    top = (BEAM_X - half_cap, BEAM_TOP, BEAM_X + half_cap, BEAM_TOP + CAP_HEIGHT, CAP_HEIGHT / 2)
    bottom = (BEAM_X - half_cap, BEAM_BOTTOM - CAP_HEIGHT, BEAM_X + half_cap, BEAM_BOTTOM,
              CAP_HEIGHT / 2)
    return min(rounded_rect_sd(x, y, r) for r in (stem, top, bottom))


def gradient(fill, y, top, bottom):
    """The colour of a vertical gradient `fill` at height `y`."""
    t = max(0.0, min(1.0, (y - top) / (bottom - top)))
    a, b = fill
    return tuple(round(a[i] + (b[i] - a[i]) * t) for i in range(3))


def colour_at(x, y):
    """The colour of one sample, or None outside the icon."""
    if rounded_rect_sd(x, y, SQUARE) > 0:
        return None
    tile = gradient(TILE, y, SQUARE[1], SQUARE[3])
    if beam_sd(x, y) <= 0:
        return gradient(BEAM, y, BEAM_TOP, BEAM_BOTTOM)
    if cloud_sd(x, y) <= 0:
        top = min(cy - r for _, cy, r in CLOUD_CIRCLES)
        return gradient(CLOUD, y, top, CLOUD_BODY[3])
    return tile


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
