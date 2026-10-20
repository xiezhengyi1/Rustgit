#!/usr/bin/env python3
"""Generate the 1024x1024 source icon (a branching graph on a rounded square).
Run once; `npx tauri icon scripts/icon-source.png` then derives every size."""
import struct, zlib, math, sys

N = 1024
def blend(dst, src, a):
    return tuple(int(d * (1 - a) + s * a) for d, s in zip(dst, src))

def sd_round_rect(x, y, cx, cy, hw, hh, r):
    qx, qy = abs(x - cx) - (hw - r), abs(y - cy) - (hh - r)
    return math.hypot(max(qx, 0), max(qy, 0)) + min(max(qx, qy), 0) - r

def sd_seg(px, py, ax, ay, bx, by):
    dx, dy = bx - ax, by - ay
    t = max(0, min(1, ((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)))
    return math.hypot(px - (ax + t * dx), py - (ay + t * dy))

nodes = [(360, 300), (360, 724), (664, 420)]
segs = [(360, 300, 360, 724), (360, 560, 664, 420)]
rows = []
for j in range(N):
    row = bytearray([0])
    for i in range(N):
        d = sd_round_rect(i, j, N / 2, N / 2, N / 2 - 40, N / 2 - 40, 210)
        a = max(0, min(1, 0.5 - d))  # 1px antialias
        t = j / N
        bg = (int(47 + 30 * t), int(95 + 20 * t), int(224 - 20 * t))
        col = bg
        fg = (255, 255, 255)
        k = min(sd_seg(i, j, *s) for s in segs) - 34
        k = min(k, min(math.hypot(i - nx, j - ny) for nx, ny in nodes) - 88)
        col = blend(col, fg, max(0, min(1, 0.5 - k)))
        row += bytes(col) + bytes([int(255 * a)])
    rows.append(bytes(row))
raw = b"".join(rows)
def chunk(t, d):
    c = struct.pack(">I", len(d)) + t + d
    return c + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", N, N, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")
open(sys.argv[1] if len(sys.argv) > 1 else "icon-source.png", "wb").write(png)
