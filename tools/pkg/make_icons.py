#!/usr/bin/env python3
"""Generate the sample packages' PNG icons (checked in; rerun to regenerate).

Valid, tiny, deterministic PNGs made with nothing but `zlib` and `struct`: a
rounded-looking tile in the app's colour with a lighter inner square, in the
three sizes a package must carry (`icons/app-16.png`, `-32`, `-128`).

    python tools/pkg/make_icons.py tools/pkg/samples/counter/icons --colour 3b82f6
"""

from __future__ import annotations

import argparse
import struct
import sys
import zlib
from pathlib import Path

SIZES = (16, 32, 128)
SIGNATURE = b"\x89PNG\r\n\x1a\n"


def chunk(kind: bytes, data: bytes) -> bytes:
    body = kind + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)


def tile(size: int, colour: tuple[int, int, int]) -> list[bytes]:
    """RGBA rows: a tile with cut corners and a lighter inner square."""
    corner = max(1, size // 8)
    inset = size // 4
    light = tuple(min(255, c + 90) for c in colour)
    rows = []
    for y in range(size):
        row = bytearray()
        for x in range(size):
            dx = min(x, size - 1 - x)
            dy = min(y, size - 1 - y)
            if dx + dy < corner:
                row += bytes((0, 0, 0, 0))  # transparent corner
            elif inset <= x < size - inset and inset <= y < size - inset:
                row += bytes((*light, 255))
            else:
                row += bytes((*colour, 255))
        rows.append(bytes(row))
    return rows


def png(size: int, colour: tuple[int, int, int]) -> bytes:
    raw = b"".join(b"\x00" + row for row in tile(size, colour))  # filter 0 per row
    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)  # 8-bit RGBA
    return SIGNATURE + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("out", type=Path, help="the icons/ directory to write")
    parser.add_argument("--colour", default="3b82f6", help="tile colour as RRGGBB (default 3b82f6)")
    args = parser.parse_args(argv)
    if len(args.colour) != 6:
        print("error: --colour must be RRGGBB", file=sys.stderr)
        return 1
    colour = tuple(int(args.colour[i : i + 2], 16) for i in (0, 2, 4))
    args.out.mkdir(parents=True, exist_ok=True)
    for size in SIZES:
        path = args.out / f"app-{size}.png"
        path.write_bytes(png(size, colour))
        print(path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
