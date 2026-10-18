#!/usr/bin/env python3
"""Generates crates/mycode-cli/assets/icon.ico for the MYCode CLI.

Pure-python rasterizer like make_icon.py: a flat Ferris-style crab (orange
gradient body, claw pair with pincer notches, eye stalks, legs) drawn with
4x supersampling, packed as a multi-size .ico that build.rs embeds as the
executable's icon resource.
"""

import math
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from make_icon import bmp_entry, png_bytes, segment_sdf, write_ico

MASTER = 256
SS = 4  # supersample factor

BODY_TOP = (247, 92, 20)
BODY_BOTTOM = (206, 55, 8)
CLAW_TOP = (235, 78, 12)
CLAW_BOTTOM = (188, 46, 6)
LIMB = (176, 52, 8)
EYE = (255, 255, 255)
PUPIL = (32, 22, 18)

CX, CY, RX, RY = 128.0, 152.0, 64.0, 42.0
CLAWS = ((56.0, 98.0), (200.0, 98.0))
CLAW_R = 25.0
EYES = ((96.0, 66.0), (160.0, 66.0))
EYE_R = 14.0
PUPIL_R = 6.5
STALKS = (((104.0, 116.0), (96.0, 80.0)), ((152.0, 116.0), (160.0, 80.0)))
STALK_W = 11.0
LEGS = (
    ((72.0, 158.0), (46.0, 178.0)),
    ((64.0, 170.0), (38.0, 194.0)),
    ((66.0, 184.0), (44.0, 214.0)),
)
LEG_W = 9.0


def mirror(point):
    return (256.0 - point[0], point[1])


def ellipse(x, y, cx, cy, rx, ry):
    return ((x - cx) / rx) ** 2 + ((y - cy) / ry) ** 2 <= 1.0


def in_circle(x, y, cx, cy, r):
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def in_claw(x, y, cx, cy):
    """Circle claw with a pincer wedge cut toward the body centre."""
    if not in_circle(x, y, cx, cy, CLAW_R):
        return False
    dx, dy = x - cx, y - cy
    dist = math.hypot(dx, dy)
    if dist <= 11.0:
        return True
    toward_body = math.atan2(CY - cy, CX - cx)
    angle = math.atan2(dy, dx)
    diff = abs(math.atan2(math.sin(angle - toward_body), math.cos(angle - toward_body)))
    return diff >= 0.48


def gradient(top, bottom, y, y0, y1):
    t = max(0.0, min(1.0, (y - y0) / (y1 - y0)))
    return tuple(top[i] + (bottom[i] - top[i]) * t for i in range(3))


def sample(x, y):
    """Color of the topmost shape at (x, y) in 256-space, or None."""
    color = None
    for a, b in LEGS:
        if segment_sdf(x, y, *a, *b) <= LEG_W / 2:
            color = LIMB
        if segment_sdf(x, y, *mirror(a), *mirror(b)) <= LEG_W / 2:
            color = LIMB
    for a, b in STALKS:
        if segment_sdf(x, y, *a, *b) <= STALK_W / 2:
            color = LIMB
    if ellipse(x, y, CX, CY, RX, RY):
        color = gradient(BODY_TOP, BODY_BOTTOM, y, CY - RY, CY + RY)
    for cx, cy in CLAWS:
        if in_claw(x, y, cx, cy):
            color = gradient(CLAW_TOP, CLAW_BOTTOM, y, cy - CLAW_R, cy + CLAW_R)
    for ex, ey in EYES:
        if in_circle(x, y, ex, ey, EYE_R):
            color = EYE
        if in_circle(x, y, ex, ey - 1.0, PUPIL_R):
            color = PUPIL
    return color


def render(size):
    scale = size / MASTER
    rows = []
    for py in range(size):
        row = bytearray()
        for px in range(size):
            acc = [0.0, 0.0, 0.0]
            alpha = 0.0
            for sy in range(SS):
                for sx in range(SS):
                    x = (px + (sx + 0.5) / SS) / scale
                    y = (py + (sy + 0.5) / SS) / scale
                    color = sample(x, y)
                    if color is not None:
                        acc[0] += color[0]
                        acc[1] += color[1]
                        acc[2] += color[2]
                        alpha += 1.0
            if alpha > 0:
                cover = alpha / (SS * SS)
                r = int(acc[0] / alpha)
                g = int(acc[1] / alpha)
                b = int(acc[2] / alpha)
            else:
                cover = 0.0
                r = g = b = 0
            row += bytes((r, g, b, int(round(cover * 255))))
        rows.append(bytes(row))
    return rows


def main():
    assets = Path(__file__).resolve().parents[1] / "crates/mycode-cli/assets"
    assets.mkdir(parents=True, exist_ok=True)
    entries = [(size, bmp_entry(render(size), size)) for size in (16, 24, 32, 48)]
    entries.append((256, png_bytes(render(256), 256)))
    ico = assets / "icon.ico"
    write_ico(ico, entries)
    print(f"wrote {ico} ({ico.stat().st_size} bytes)")
    png = assets / "icon.png"
    png.write_bytes(png_bytes(render(256), 256))
    print(f"wrote {png} ({png.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
