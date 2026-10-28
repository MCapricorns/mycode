#!/usr/bin/env python3
"""Render MYCode's bracket-M SVG into desktop icons and visual proofs.

Run `python scripts/make_icon.py` to regenerate, or add `--check` to verify
committed artifacts. Uses only the standard library. icon.svg is the source:
its rounded rect and polygon share a square viewBox with no transforms.
Each size is rasterized directly with 4x supersampling and straight alpha;
no host fonts, external images, timestamps, or platform tools are involved.
"""

import argparse
import math
import struct
import xml.etree.ElementTree as ET
import zlib
from pathlib import Path

SS = 4
PAPER = (250, 246, 238)
INK = (20, 19, 27)
ICO_SIZES = (16, 24, 32, 48, 64, 128, 256)
ICNS_TYPES = (
    ("icp4", 16),
    ("icp5", 32),
    ("icp6", 64),
    ("ic11", 32),   # 16 pt @2x
    ("ic12", 64),   # 32 pt @2x
    ("ic07", 128),
    ("ic08", 256),
    ("ic13", 256),  # 128 pt @2x
    ("ic09", 512),
    ("ic14", 512),  # 256 pt @2x
    ("ic10", 1024),  # 512 pt @2x
)


def rgb(hex_color):
    return tuple(bytes.fromhex(hex_color.removeprefix("#")))


def coverage(size, scale, spans):
    """Count covered subpixel centers using spans, without a huge SS bitmap."""
    edges = [0] * size
    deltas = [0] * (size + 1)
    for left, right in spans:
        start = max(0, math.ceil(left * scale * SS - 0.5))
        end = min(size * SS, math.ceil(right * scale * SS - 0.5))
        if start >= end:
            continue
        first, last = start // SS, (end - 1) // SS
        if first == last:
            edges[first] += end - start
        else:
            edges[first] += SS - start % SS
            edges[last] += (end - 1) % SS + 1
            deltas[first + 1] += SS
            deltas[last] -= SS
    running = 0
    for x in range(size):
        running += deltas[x]
        yield running + edges[x]


def render(svg, size):
    """RGBA rows from the SVG's rounded tile and wholly contained monogram."""
    ns = {"svg": "http://www.w3.org/2000/svg"}
    tile = svg.find("svg:rect", ns)
    mark = svg.find("svg:polygon", ns)
    frame = tuple(map(float, svg.attrib["viewBox"].split()))
    if frame[:2] != (0, 0) or frame[2] != frame[3]:
        raise ValueError("icon.svg requires a square viewBox starting at 0,0")
    scale = size / frame[2]
    tx, ty, tw, th, radius = (
        float(tile.attrib[k]) for k in ("x", "y", "width", "height", "rx"))
    points = [tuple(map(float, point.split(",")))
              for point in mark.attrib["points"].split()]
    segments = list(zip(points, points[1:] + points[:1]))
    ground, ink = rgb(tile.attrib["fill"]), rgb(mark.attrib["fill"])
    samples = SS * SS
    # Average premultiplied samples, then recover straight-alpha RGB. This
    # keeps red edges clean on both paper and ink backgrounds.
    palette = {(0, 0): bytes(4)}
    for bg in range(1, samples + 1):
        for fg in range(bg + 1):
            color = tuple((g * (bg - fg) + i * fg + bg // 2) // bg
                          for g, i in zip(ground, ink))
            palette[bg, fg] = bytes((*color, (bg * 255 + samples // 2) // samples))
    rows = []
    for py in range(size):
        tile_spans, mark_spans = [], []
        for sy in range(SS):
            y = (py + (sy + 0.5) / SS) / scale
            if ty <= y < ty + th:
                dy = max(ty + radius - y, y - (ty + th - radius), 0)
                inset = radius - math.sqrt(max(0, radius * radius - dy * dy))
                tile_spans.append((tx + inset, tx + tw - inset))
            hits = sorted(ax + (y - ay) * (bx - ax) / (by - ay)
                          for (ax, ay), (bx, by) in segments
                          if min(ay, by) <= y < max(ay, by))
            mark_spans.extend(zip(hits[::2], hits[1::2]))
        rows.append(b"".join(palette[bg, fg] for bg, fg in zip(
            coverage(size, scale, tile_spans), coverage(size, scale, mark_spans))))
    return rows


def png_bytes(rows, width, height=None):
    def chunk(tag, data):
        raw = tag + data
        return struct.pack(">I", len(data)) + raw + struct.pack(">I", zlib.crc32(raw) & 0xFFFFFFFF)

    height = width if height is None else height
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    raw = b"".join(b"\x00" + row for row in rows)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr)
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def bmp_entry(rows, size):
    """ICO DIB: bottom-up 32bpp BGRA and an aligned transparency AND mask."""
    header = struct.pack(
        "<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, size * size * 4, 0, 0, 0, 0)
    body, mask = bytearray(), bytearray()
    mask_stride = ((size + 31) // 32) * 4
    for row in reversed(rows):
        mask_row = bytearray(mask_stride)
        for x in range(size):
            r, g, b, a = row[x * 4:x * 4 + 4]
            body.extend((b, g, r, a))
            if a == 0:
                mask_row[x // 8] |= 0x80 >> (x % 8)
        mask.extend(mask_row)
    return header + body + mask


def ico_bytes(images, pngs):
    entries = [(size, pngs[size] if size == 256 else bmp_entry(images[size], size))
               for size in ICO_SIZES]
    offset = 6 + 16 * len(entries)
    directory, blobs = bytearray(), bytearray()
    for size, payload in entries:
        w = 0 if size == 256 else size
        directory.extend(struct.pack("<BBBBHHII", w, w, 0, 0, 1, 32, len(payload), offset))
        blobs.extend(payload)
        offset += len(payload)
    return struct.pack("<HHH", 0, 1, len(entries)) + directory + blobs


def icns_bytes(pngs):
    """Modern PNG representations, including native small sizes and Retina."""
    body = b"".join(tag.encode("ascii") + struct.pack(">I", len(pngs[size]) + 8) + pngs[size]
                    for tag, size in ICNS_TYPES)
    return b"icns" + struct.pack(">I", len(body) + 8) + body


# Only numerals and the magnification sign are needed for the proof sheet.
DIGITS = {
    "0": (7, 5, 5, 5, 7), "1": (2, 6, 2, 2, 7),
    "2": (7, 1, 7, 4, 7), "3": (7, 1, 7, 1, 7),
    "4": (5, 5, 7, 1, 1), "5": (7, 4, 7, 1, 7),
    "6": (7, 4, 7, 5, 7), "7": (7, 1, 1, 1, 1),
    "8": (7, 5, 7, 5, 7), "9": (7, 5, 7, 1, 7),
    "x": (0, 5, 2, 5, 0), " ": (0, 0, 0, 0, 0),
}


def preview(images):
    """Light/dark proof: 256px, native small sizes, then nearest 4x detail."""
    width, height = 1024, 800
    line = bytes((*PAPER, 255)) * 512 + bytes((*INK, 255)) * 512
    canvas = bytearray(line * height)

    def stamp(rows, x, y, background, zoom=1):
        for py, row in enumerate(rows):
            line = bytearray()
            for px in range(len(row) // 4):
                *color, alpha = row[px * 4:px * 4 + 4]
                color = tuple((c * alpha + b * (255 - alpha) + 127) // 255
                              for c, b in zip(color, background))
                line.extend(bytes((*color, 255)) * zoom)
            for dy in range(zoom):
                start = ((y + py * zoom + dy) * width + x) * 4
                canvas[start:start + len(line)] = line

    def label(text, cx, y, color):
        x = cx - (len(text) * 4 - 1)
        for char in text:
            for gy, bits in enumerate(DIGITS[char]):
                for gx in range(3):
                    if bits & (4 >> gx):
                        for dy in range(2):
                            start = ((y + gy * 2 + dy) * width + x + gx * 2) * 4
                            canvas[start:start + 8] = bytes((*color, 255)) * 2
            x += 8

    for origin, background, foreground in ((0, PAPER, INK), (512, INK, PAPER)):
        stamp(images[256], origin + 128, 24, background)
        label("256", origin + 256, 300, foreground)
        for cx, size in zip((40, 120, 200, 288, 416), (16, 24, 32, 48, 128)):
            stamp(images[size], origin + cx - size // 2, 496 - size, background)
            label(str(size), origin + cx, 512, foreground)
        for cx, size in zip((84, 224, 392), (16, 24, 32)):
            stamp(images[size], origin + cx - size * 2, 752 - size * 4, background, 4)
            label(f"{size} x 4", origin + cx, 768, foreground)
    stride = width * 4
    rows = [bytes(canvas[y * stride:(y + 1) * stride]) for y in range(height)]
    return png_bytes(rows, width, height)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="fail if artifacts are stale")
    args = parser.parse_args()
    assets = Path(__file__).resolve().parents[1] / "crates/mycode-desktop/assets"
    svg = ET.parse(assets / "icon.svg").getroot()
    sizes = sorted(set(ICO_SIZES) | {size for _, size in ICNS_TYPES})
    images = {size: render(svg, size) for size in sizes}
    pngs = {size: png_bytes(images[size], size) for size in sizes}
    artifacts = {
        "icon.ico": ico_bytes(images, pngs),
        "icon.icns": icns_bytes(pngs),
        "icon.png": pngs[1024],
        "icon-preview.png": preview(images),
    }
    stale = False
    for name, data in artifacts.items():
        path = assets / name
        if args.check:
            matches = path.is_file() and path.read_bytes() == data
            print(f"{'ok' if matches else 'STALE'} {path} ({len(data)} bytes)")
            stale |= not matches
        else:
            path.write_bytes(data)
            print(f"wrote {path} ({len(data)} bytes)")
    return int(stale)


if __name__ == "__main__":
    raise SystemExit(main())
