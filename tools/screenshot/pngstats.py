#!/usr/bin/env python3
"""Analyse a PNG screenshot using only the Python standard library.

This is the companion to `qemu_shot.py`: after capturing pixels from headless
QEMU, an agent or CI job needs a cheap, deterministic way to assert "something
was actually rendered". It decodes the PNG (no Pillow dependency), reports
summary statistics, and optionally fails with a non-zero exit code when
expectations are not met.

Supported PNGs: 8-bit greyscale (type 0), RGB (type 2), RGBA (type 6),
non-interlaced -- which covers QEMU `screendump ...,format=png`.

Usage
-----
    python tools/screenshot/pngstats.py shots/shot_5s.png
    python tools/screenshot/pngstats.py shots/*.png --min-nonblack 0.01 --json
    python tools/screenshot/pngstats.py shot.png --expect-width 1280 --expect-height 720

Exit code is 0 when all requested expectations pass, 1 otherwise.
"""

from __future__ import annotations

import argparse
import json
import struct
import sys
import zlib
from pathlib import Path

_PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
_CHANNELS = {0: 1, 2: 3, 4: 2, 6: 4}


def _paeth(a: int, b: int, c: int) -> int:
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    if pa <= pb and pa <= pc:
        return a
    if pb <= pc:
        return b
    return c


def decode_png(path: Path) -> tuple[int, int, int, bytes]:
    """Return (width, height, channels, raw_pixels). Raises ValueError on
    unsupported PNG variants."""
    data = path.read_bytes()
    if data[:8] != _PNG_SIGNATURE:
        raise ValueError(f"{path}: not a PNG file")

    pos = 8
    idat = bytearray()
    width = height = bit_depth = color_type = interlace = None
    while pos + 8 <= len(data):
        length = struct.unpack(">I", data[pos : pos + 4])[0]
        chunk_type = data[pos + 4 : pos + 8]
        pos += 8
        chunk = data[pos : pos + length]
        pos += length + 4  # skip CRC
        if chunk_type == b"IHDR":
            width, height, bit_depth, color_type, _comp, _filt, interlace = struct.unpack(
                ">IIBBBBB", chunk
            )
        elif chunk_type == b"IDAT":
            idat += chunk
        elif chunk_type == b"IEND":
            break

    if width is None or height is None:
        raise ValueError(f"{path}: missing IHDR")
    if bit_depth != 8:
        raise ValueError(f"{path}: unsupported bit depth {bit_depth} (only 8-bit supported)")
    if interlace != 0:
        raise ValueError(f"{path}: interlaced PNGs are not supported")
    if color_type not in _CHANNELS:
        raise ValueError(f"{path}: unsupported colour type {color_type}")

    channels = _CHANNELS[color_type]
    stride = width * channels
    raw = zlib.decompress(bytes(idat))
    out = bytearray(height * stride)
    previous = bytearray(stride)
    cursor = 0

    for y in range(height):
        filter_type = raw[cursor]
        cursor += 1
        line = bytearray(raw[cursor : cursor + stride])
        cursor += stride

        if filter_type == 0:  # None
            pass
        elif filter_type == 1:  # Sub
            for x in range(channels, stride):
                line[x] = (line[x] + line[x - channels]) & 0xFF
        elif filter_type == 2:  # Up
            for x in range(stride):
                line[x] = (line[x] + previous[x]) & 0xFF
        elif filter_type == 3:  # Average
            for x in range(stride):
                left = line[x - channels] if x >= channels else 0
                line[x] = (line[x] + ((left + previous[x]) >> 1)) & 0xFF
        elif filter_type == 4:  # Paeth
            for x in range(stride):
                left = line[x - channels] if x >= channels else 0
                up = previous[x]
                up_left = previous[x - channels] if x >= channels else 0
                line[x] = (line[x] + _paeth(left, up, up_left)) & 0xFF
        else:
            raise ValueError(f"{path}: unknown filter type {filter_type}")

        out[y * stride : (y + 1) * stride] = line
        previous = line

    return width, height, channels, bytes(out)


def analyse(width: int, height: int, channels: int, pixels: bytes,
            bg: tuple[int, int, int] = (0, 0, 0), threshold: int = 12) -> dict:
    total = width * height
    luminance_sum = 0
    non_background = 0
    colours: set[tuple[int, int, int]] = set()
    min_lum, max_lum = 255, 0

    for i in range(total):
        o = i * channels
        if channels >= 3:
            r, g, b = pixels[o], pixels[o + 1], pixels[o + 2]
            alpha = pixels[o + 3] if channels == 4 else 255
        else:  # greyscale (type 0) or grey+alpha (type 4)
            r = g = b = pixels[o]
            alpha = pixels[o + 1] if channels == 2 else 255
        # Treat fully transparent pixels as background.
        if alpha == 0:
            r = g = b = 0
        luminance_sum += r + g + b
        lum = (r * 299 + g * 587 + b * 114) // 1000
        min_lum = min(min_lum, lum)
        max_lum = max(max_lum, lum)
        if abs(r - bg[0]) + abs(g - bg[1]) + abs(b - bg[2]) > threshold:
            non_background += 1
        colours.add((r >> 4, g >> 4, b >> 4))

    return {
        "width": width,
        "height": height,
        "channels": channels,
        "mean_rgb": round(luminance_sum / (total * 3), 3),
        "nonbackground_ratio": round(non_background / total, 5),
        "distinct_colors_q4": len(colours),
        "min_luminance": min_lum,
        "max_luminance": max_lum,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("files", nargs="+", help="PNG file(s) to analyse")
    parser.add_argument("--json", action="store_true", help="emit JSON only")
    parser.add_argument("--min-nonblack", type=float, default=None,
                        help="fail if non-background pixel ratio is below this")
    parser.add_argument("--min-colors", type=int, default=None,
                        help="fail if fewer than this many distinct colours")
    parser.add_argument("--expect-width", type=int, default=None)
    parser.add_argument("--expect-height", type=int, default=None)
    parser.add_argument("--max-mean", type=float, default=None,
                        help="fail if mean RGB exceeds this (detect all-white)")
    args = parser.parse_args(argv)

    results = {}
    failures: list[str] = []

    for name in args.files:
        path = Path(name)
        try:
            width, height, channels, pixels = decode_png(path)
            stats = analyse(width, height, channels, pixels)
        except (ValueError, OSError) as exc:
            failures.append(f"{path}: {exc}")
            results[str(path)] = {"error": str(exc)}
            continue

        results[str(path)] = stats

        if args.expect_width is not None and width != args.expect_width:
            failures.append(f"{path}: width {width} != expected {args.expect_width}")
        if args.expect_height is not None and height != args.expect_height:
            failures.append(f"{path}: height {height} != expected {args.expect_height}")
        if args.min_nonblack is not None and stats["nonbackground_ratio"] < args.min_nonblack:
            failures.append(
                f"{path}: non-background ratio {stats['nonbackground_ratio']} "
                f"< {args.min_nonblack}"
            )
        if args.min_colors is not None and stats["distinct_colors_q4"] < args.min_colors:
            failures.append(
                f"{path}: distinct colours {stats['distinct_colors_q4']} < {args.min_colors}"
            )
        if args.max_mean is not None and stats["mean_rgb"] > args.max_mean:
            failures.append(f"{path}: mean RGB {stats['mean_rgb']} > {args.max_mean}")

    if args.json:
        print(json.dumps(results, indent=2))
    else:
        for name, stats in results.items():
            if "error" in stats:
                print(f"{name}: ERROR: {stats['error']}")
            else:
                print(
                    f"{name}: {stats['width']}x{stats['height']} "
                    f"ch={stats['channels']} mean={stats['mean_rgb']} "
                    f"nonbg={stats['nonbackground_ratio']} "
                    f"colors={stats['distinct_colors_q4']}"
                )
        if failures:
            print("\nFAILURES:", file=sys.stderr)
            for failure in failures:
                print(f"  - {failure}", file=sys.stderr)

    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
