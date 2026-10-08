#!/usr/bin/env python3
"""Generates crates/mycode-desktop/assets/icon.ico for MYCode.

Pure-python rasterizer: no PIL/imaging dependency. Draws the M-mark (black
stroke "M" over a white rounded square — matching the in-app logo tiles)
with 4x supersampling, then packs a multi-size .ico (uncompressed BMP
entries for 16/24/32/48 px, PNG entry for 256 px) that Windows loads as
resource id 1.
"""

import struct
import zlib
from pathlib import Path

MASTER = 256
SS = 4  # supersample factor


def rounded_rect_sdf(x, y, cx, cy, hw, hh, r):
    dx = abs(x - cx) - (hw - r)
    dy = abs(y - cy) - (hh - r)
    ax, ay = max(dx, 0.0), max(dy, 0.0)
    return (ax * ax + ay * ay) ** 0.5 + min(max(dx, dy), 0.0) - r


def segment_sdf(px, py, ax, ay, bx, by):
    abx, aby = bx - ax, by - ay
    t = ((px - ax) * abx + (py - ay) * aby) / (abx * abx + aby * aby)
    t = max(0.0, min(1.0, t))
    qx, qy = ax + t * abx, ay + t * aby
    return ((px - qx) ** 2 + (py - qy) ** 2) ** 0.5


def m_distance(x, y):
    """Distance to the M stroke skeleton, in 256-space units."""
    a = (76, 82)
    apex = (128, 138)
    b = (180, 82)
    bottom = 182
    segs = [
        (a, (a[0], bottom)),
        (a, apex),
        (apex, b),
        (b, (b[0], bottom)),
    ]
    return min(segment_sdf(x, y, *s[0], *s[1]) for s in segs)


def render(size):
    """RGBA rows for one square size, supersampled from the master space."""
    scale = size / MASTER
    rows = []
    half_stroke = 11.5
    cx = cy = MASTER / 2
    hw = hh = MASTER / 2 - 8  # 8px outer margin
    radius = 58
    top = (255, 255, 255)  # white tile
    bottom = (255, 255, 255)  # white tile
    for py in range(size):
        row = bytearray()
        for px in range(size):
            r = g = b = 0
            alpha = 0.0
            acc = [0.0, 0.0, 0.0]
            for sy in range(SS):
                for sx in range(SS):
                    x = (px + (sx + 0.5) / SS) / scale
                    y = (py + (sy + 0.5) / SS) / scale
                    if rounded_rect_sdf(x, y, cx, cy, hw, hh, radius) <= 0:
                        t = max(0.0, min(1.0, (y - (cy - hh)) / (2 * hh)))
                        cr = top[0] + (bottom[0] - top[0]) * t
                        cg = top[1] + (bottom[1] - top[1]) * t
                        cb = top[2] + (bottom[2] - top[2]) * t
                        if m_distance(x, y) <= half_stroke:
                            cr = cg = cb = 0.0
                        acc[0] += cr
                        acc[1] += cg
                        acc[2] += cb
                        alpha += 1.0
            samples = SS * SS
            if alpha > 0:
                cover = alpha / samples
                r = int(acc[0] / alpha)
                g = int(acc[1] / alpha)
                b = int(acc[2] / alpha)
            else:
                cover = 0.0
            a = int(round(cover * 255))
            # Straight alpha over transparency; premultiply-ish blend for AA.
            row += bytes((r, g, b, a))
        rows.append(bytes(row))
    return rows


def png_bytes(rows, size):
    def chunk(tag, data):
        raw = tag + data
        return struct.pack(">I", len(data)) + raw + struct.pack(">I", zlib.crc32(raw) & 0xFFFFFFFF)

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    raw = b"".join(b"\x00" + row for row in rows)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def bmp_entry(rows, size):
    """ICO BMP entry: 32bpp BGRA bottom-up plus a zero AND mask."""
    header = struct.pack(
        "<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, size * size * 4, 0, 0, 0, 0
    )
    body = bytearray()
    for row in reversed(rows):
        for i in range(size):
            r, g, b, a = row[i * 4 : i * 4 + 4]
            body += bytes((b, g, r, a))
    mask_row = ((size + 31) // 32) * 4
    mask = b"\x00" * (mask_row * size)
    return bytes(header) + bytes(body) + mask


def write_ico(path, entries):
    # entries: list of (size, payload)
    header = struct.pack("<HHH", 0, 1, len(entries))
    offset = 6 + 16 * len(entries)
    directory = b""
    blobs = b""
    for size, payload in entries:
        w = 0 if size >= 256 else size
        directory += struct.pack("<BBBBHHII", w, w, 0, 0, 1, 32, len(payload), offset)
        blobs += payload
        offset += len(payload)
    path.write_bytes(header + directory + blobs)


def main():
    out = Path(__file__).resolve().parents[1] / "crates/mycode-desktop/assets/icon.ico"
    out.parent.mkdir(parents=True, exist_ok=True)
    entries = []
    for size in (16, 24, 32, 48):
        entries.append((size, bmp_entry(render(size), size)))
    entries.append((256, png_bytes(render(256), 256)))
    write_ico(out, entries)
    print(f"wrote {out} ({out.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
