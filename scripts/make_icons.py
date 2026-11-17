"""Generate the app icons (PNG + multi-size ICO) with the standard library only.
    python scripts/make_icons.py      (run from the repository root)
Draws a blue rounded square with a white envelope.
"""
from __future__ import annotations

import struct
import zlib
from pathlib import Path

OUT = Path(__file__).resolve().parents[1] / "icons"
BLUE = (15, 108, 189)
WHITE = (255, 255, 255)


def inside_rounded(x: float, y: float, r: float) -> bool:
    cx = min(max(x, r), 1 - r)
    cy = min(max(y, r), 1 - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def near_segment(x, y, ax, ay, bx, by, w) -> bool:
    dx, dy = bx - ax, by - ay
    t = max(0.0, min(1.0, ((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)))
    px, py = ax + t * dx, ay + t * dy
    return (x - px) ** 2 + (y - py) ** 2 <= w * w


def color_at(x: float, y: float):
    if not inside_rounded(x, y, 0.2):
        return None
    l, r, t, b = 0.2, 0.8, 0.3, 0.7  # envelope
    if l <= x <= r and t <= y <= b:
        if near_segment(x, y, l, t, 0.5, 0.53, 0.035) or near_segment(x, y, r, t, 0.5, 0.53, 0.035):
            return BLUE
        return WHITE
    return BLUE


def render(n: int, ss: int = 4) -> bytes:
    rows = []
    for j in range(n):
        row = bytearray([0])
        for i in range(n):
            acc = [0, 0, 0, 0]
            for sj in range(ss):
                for si in range(ss):
                    c = color_at((i + (si + .5) / ss) / n, (j + (sj + .5) / ss) / n)
                    if c:
                        acc[0] += c[0]; acc[1] += c[1]; acc[2] += c[2]; acc[3] += 255
            k = ss * ss
            a = acc[3] // k
            row += bytes([acc[0] // max(1, acc[3] // 255), acc[1] // max(1, acc[3] // 255), acc[2] // max(1, acc[3] // 255), a]) if a else b"\0\0\0\0"
        rows.append(bytes(row))
    raw = b"".join(rows)

    def chunk(tag: bytes, data: bytes) -> bytes:
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", n, n, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def ico(pngs: dict[int, bytes]) -> bytes:
    head = struct.pack("<HHH", 0, 1, len(pngs))
    offset = 6 + 16 * len(pngs)
    entries, data = b"", b""
    for size, png in sorted(pngs.items()):
        entries += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(png), offset + len(data))
        data += png
    return head + entries + data


if __name__ == "__main__":
    OUT.mkdir(exist_ok=True)
    pngs = {n: render(n) for n in (16, 24, 32, 48, 64, 128, 256)}
    (OUT / "32x32.png").write_bytes(pngs[32])
    (OUT / "128x128.png").write_bytes(pngs[128])
    (OUT / "icon.png").write_bytes(pngs[256])
    (OUT / "icon.ico").write_bytes(ico({n: pngs[n] for n in (16, 24, 32, 48, 64, 256)}))
    print("icons written to", OUT)
