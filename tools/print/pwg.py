#!/usr/bin/env python3
"""Read a PWG Raster stream (PWG 5102.4) back, for the print harness's
verdict, and write a page as a PNG for a human to look at.

    python tools/print/pwg.py shots/print/jobs/job-1.pwg --png page.png
"""

from __future__ import annotations

import argparse
import struct
import sys
import zlib
from dataclasses import dataclass
from pathlib import Path

HEADER = 1796


@dataclass
class Page:
    width: int
    height: int
    dpi: int
    channels: int
    media: str
    total_pages: int
    pixels: bytes


def decode(data: bytes, budget: int = 1 << 30) -> list[Page]:
    if data[:4] != b"RaS2":
        raise ValueError("no RaS2 sync word")
    at, pages, used = 4, [], 0
    while at < len(data):
        h = data[at:at + HEADER]
        if len(h) < HEADER:
            raise ValueError("truncated header")
        at += HEADER
        u = lambda off: struct.unpack(">I", h[off:off + 4])[0]  # noqa: E731
        width, height, bpp, line = u(372), u(376), u(388), u(392)
        channels = bpp // 8
        if channels not in (1, 3) or line != width * channels:
            raise ValueError(f"unsupported header: {bpp} bits per pixel, {line} bytes per line")
        used += line * height
        if used > budget:
            raise ValueError("pages too large")
        out = bytearray()
        rows = 0
        while rows < height:
            repeat = data[at] + 1
            at += 1
            row = bytearray()
            while len(row) < line:
                control = data[at]
                at += 1
                if control < 128:
                    row += data[at:at + channels] * (control + 1)
                    at += channels
                else:
                    n = (257 - control) * channels
                    row += data[at:at + n]
                    at += n
            if len(row) != line or rows + repeat > height:
                raise ValueError(f"row {rows} overruns its line or page")
            out += bytes(row) * repeat
            rows += repeat
        media = h[1732:1796].split(b"\0")[0].decode("utf-8", "replace")
        pages.append(Page(width, height, u(276), channels, media, u(452), bytes(out)))
    return pages


def ink(page: Page) -> float:
    """The fraction of pixels darker than mid grey."""
    c = page.channels
    dark = sum(1 for i in range(0, len(page.pixels), c) if min(page.pixels[i:i + c]) < 128)
    return dark / (page.width * page.height)


def ink_box(page: Page) -> tuple[int, int, int, int] | None:
    """The bounding box `(left, top, right, bottom)` of the dark pixels."""
    c, w = page.channels, page.width
    box = None
    for y in range(page.height):
        row = page.pixels[y * w * c:(y + 1) * w * c]
        xs = [x for x in range(w) if min(row[x * c:x * c + c]) < 128]
        if xs:
            l, r = min(xs), max(xs)
            box = (l, y, r, y) if box is None else (min(box[0], l), box[1], max(box[2], r), y)
    return box


def png(page: Page, path: Path, scale: int = 4) -> None:
    """Writes `page` as an 8-bit PNG, shrunk `scale` times (nearest pixel)."""
    c = page.channels
    w, h = page.width // scale, page.height // scale
    rows = bytearray()
    for y in range(h):
        src = page.pixels[(y * scale) * page.width * c:(y * scale + 1) * page.width * c]
        rows.append(0)
        for x in range(w):
            rows += src[x * scale * c:x * scale * c + c]
    color = 0 if c == 1 else 2
    chunk = lambda kind, data: (struct.pack(">I", len(data)) + kind + data  # noqa: E731
                                + struct.pack(">I", zlib.crc32(kind + data)))
    path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, color, 0, 0, 0))
                     + chunk(b"IDAT", zlib.compress(bytes(rows), 6)) + chunk(b"IEND", b""))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("stream", type=Path)
    parser.add_argument("--png", type=Path, help="write the first page here")
    args = parser.parse_args()
    pages = decode(args.stream.read_bytes())
    for i, page in enumerate(pages, 1):
        print(f"page {i}: {page.width}x{page.height} at {page.dpi} dpi, {page.channels} channel(s), "
              f"{page.media}, ink {ink(page):.4f}")
    if args.png and pages:
        png(pages[0], args.png)
    return 0


if __name__ == "__main__":
    sys.exit(main())
