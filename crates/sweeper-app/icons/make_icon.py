#!/usr/bin/env python3
"""Generates icons/source.png (1024x1024), the Sweeper Kit app icon.

Standard library only. Run from anywhere:
    python3 crates/sweeper-app/icons/make_icon.py
then, inside crates/sweeper-app:
    cargo tauri icon icons/source.png
"""
import math
import os
import struct
import zlib

SIZE = 1024
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "source.png")

# macOS icon grid: the rounded square fills ~824px of the 1024px canvas.
HALF = 412
RADIUS = 185
TOP = (86, 204, 242)
BOTTOM = (0, 98, 214)
ANGLE = math.radians(35)
COS, SIN = math.cos(ANGLE), math.sin(ANGLE)
WHITE = (255, 255, 255)


def clamp01(x):
    return 0.0 if x < 0 else 1.0 if x > 1 else x


def cover(distance):
    """Anti-aliased coverage from a signed distance (negative = inside)."""
    return clamp01(0.5 - distance)


def rounded_square(x, y):
    qx, qy = abs(x) - (HALF - RADIUS), abs(y) - (HALF - RADIUS)
    outside = math.hypot(max(qx, 0), max(qy, 0))
    return outside + min(max(qx, qy), 0) - RADIUS


def capsule(u, v, u0, u1, r):
    cu = min(max(u, u0), u1)
    return math.hypot(u - cu, v) - r


def broom_head(u, v):
    """Trapezoid widening away from the handle, with softened corners."""
    top, bottom, narrow, wide, soft = 70, 330, 62, 175, 18
    t = clamp01((u - top) / (bottom - top))
    half = narrow + (wide - narrow) * t - soft
    du = max(top + soft - u, u - (bottom - soft), 0)
    dv = max(abs(v) - half, 0)
    return math.hypot(du, dv) - soft


def mix(a, b, t):
    return tuple(a[i] + (b[i] - a[i]) * t for i in range(3))


def pixel(px, py):
    x, y = px + 0.5 - SIZE / 2, py + 0.5 - SIZE / 2
    alpha = cover(rounded_square(x, y))
    if alpha <= 0:
        return (0, 0, 0, 0)
    color = mix(TOP, BOTTOM, clamp01((y + HALF) / (2 * HALF)))

    # The broom is drawn in a frame rotated by ANGLE: u along the handle.
    u = x * COS + y * SIN
    v = -x * SIN + y * COS

    # Soft shadow under the broom.
    shadow = clamp01(1 - min(capsule(u, v - 14, -330, 90, 24), broom_head(u, v - 14)) / 30)
    color = mix(color, (0, 40, 110), 0.35 * shadow)

    handle = cover(capsule(u, v, -330, 90, 22))
    head = cover(broom_head(u, v))
    # Bristle grooves along the head.
    groove = 0.0
    if 100 < u < 330:
        stripe = abs(((v + 400) % 46) - 23)
        groove = clamp01((9 - stripe) / 9) * clamp01((u - 100) / 40) * 0.28
    color = mix(color, WHITE, handle)
    color = mix(color, (255, 214, 120) if 70 < u < 120 else (250, 250, 252), head)
    color = mix(color, (60, 110, 180), groove * head)

    # Specks of swept dust.
    for sx, sy, sr in ((-250, 215, 20), (-150, 290, 13), (-300, 120, 10)):
        color = mix(color, WHITE, 0.9 * cover(math.hypot(x - sx, y - sy) - sr))
    return tuple(int(round(c)) for c in color) + (int(round(alpha * 255)),)


def chunk(tag, data):
    body = tag + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)


def main():
    rows = bytearray()
    for py in range(SIZE):
        rows.append(0)
        for px in range(SIZE):
            rows.extend(pixel(px, py))
    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(rows), 9))
    png += chunk(b"IEND", b"")
    with open(OUT, "wb") as f:
        f.write(png)


if __name__ == "__main__":
    main()
