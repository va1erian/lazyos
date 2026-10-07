#!/usr/bin/env python3
"""Draw the pictures of the LazyWeb harness's theoldnet.com test site.

The live theoldnet.com shows a maintenance notice, so the harness serves a
representative late-90s home page instead (`fixtures/theoldnet.com/`). Its
pictures cover every format Blitz decodes on the classic web, each drawn
here with the standard library only (`imgenc.py`, `jpegenc.py`):

    images/bg_tile.gif        48x48 GIF, a starfield the body tiles (`background=`)
    images/logo.png           400x90 RGBA PNG, the site's name with a drop shadow
    images/rainbow.gif        400x6 GIF, the rainbow rule
    images/photo.jpg          240x160 baseline JPEG, a synthwave sunset
    images/construction.gif   88x31 animated GIF (4 frames), "UNDER CONSTRUCTION"

The output is deterministic and checked in, so a build needs no generator run:

    python tools/web/gen_fixtures.py           # rewrite the pictures
    python tools/web/gen_fixtures.py --check   # exit 1 when a checked-in picture is stale
"""

from __future__ import annotations

import argparse
import math
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import imgenc  # noqa: E402
import jpegenc  # noqa: E402

IMAGES = HERE / "fixtures" / "theoldnet.com" / "images"

#: A 5x7 bitmap font: the glyphs the pictures use, one string of 7 rows of 5 bits each.
FONT = {
    "A": "01110 10001 10001 11111 10001 10001 10001", "B": "11110 10001 10001 11110 10001 10001 11110",
    "C": "01110 10001 10000 10000 10000 10001 01110", "D": "11110 10001 10001 10001 10001 10001 11110",
    "E": "11111 10000 10000 11110 10000 10000 11111", "F": "11111 10000 10000 11110 10000 10000 10000",
    "H": "10001 10001 10001 11111 10001 10001 10001", "I": "01110 00100 00100 00100 00100 00100 01110",
    "L": "10000 10000 10000 10000 10000 10000 11111", "M": "10001 11011 10101 10101 10001 10001 10001",
    "N": "10001 11001 10101 10011 10001 10001 10001", "O": "01110 10001 10001 10001 10001 10001 01110",
    "R": "11110 10001 10001 11110 10100 10010 10001", "S": "01111 10000 10000 01110 00001 00001 11110",
    "T": "11111 00100 00100 00100 00100 00100 00100", "U": "10001 10001 10001 10001 10001 10001 01110",
    "W": "10001 10001 10001 10101 10101 10101 01010", ".": "00000 00000 00000 00000 00000 01100 01100",
    " ": "00000 00000 00000 00000 00000 00000 00000",
}


def text_mask(text: str, scale: int) -> tuple[int, int, set[tuple[int, int]]]:
    """(width, height, set of lit pixels) of `text` at `scale`, 1 column apart."""
    lit = set()
    for n, char in enumerate(text):
        for row, bits in enumerate(FONT[char].split()):
            for col, bit in enumerate(bits):
                if bit == "1":
                    for dy in range(scale):
                        for dx in range(scale):
                            lit.add(((n * 6 + col) * scale + dx, row * scale + dy))
    return (len(text) * 6 - 1) * scale, 7 * scale, lit


def _random(seed: int):
    """A small deterministic LCG (the pictures must not depend on Python's)."""
    state = seed
    while True:
        state = (state * 1103515245 + 12345) & 0x7FFFFFFF
        yield state


def bg_tile() -> bytes:
    size = 48
    palette = [(0, 0, 51), (60, 60, 120), (150, 150, 210), (255, 255, 255)]
    pixels = [0] * (size * size)
    rng = _random(1999)
    for _ in range(14):
        x, y, tone = next(rng) % size, next(rng) % size, 1 + next(rng) % 3
        pixels[y * size + x] = tone
        if tone == 3:  # a bright star twinkles into a cross
            for dx, dy in ((1, 0), (-1, 0), (0, 1), (0, -1)):
                pixels[(y + dy) % size * size + (x + dx) % size] = 1
    return imgenc.gif(size, size, palette, [pixels])


def logo() -> bytes:
    width, height = 400, 90
    _, text_h, title = text_mask("THEOLDNET.COM", 5)
    _, _, motto = text_mask("SURF THE OLD WEB", 2)
    ox, oy = 4, 6
    pixels = []
    for y in range(height):
        for x in range(width):
            here = (x - ox, y - oy)
            shadow = (x - ox - 4, y - oy - 4)
            if here in title:
                t = (y - oy) / text_h  # yellow at the top to red at the bottom
                pixels.append((255, int(255 - 200 * t), int(40 * (1 - t)), 255))
            elif shadow in title:
                pixels.append((0, 0, 0, 160))
            elif (x - 104, y - 66) in motto:
                pixels.append((0, 255, 255, 255))
            else:
                pixels.append((0, 0, 0, 0))
    return imgenc.png(width, height, pixels)


def rainbow() -> bytes:
    width, height = 400, 6
    palette = []
    for i in range(32):
        h = i / 32 * 6
        k = int(h)
        f = h - k
        rgb = [(1, f, 0), (1 - f, 1, 0), (0, 1, f), (0, 1 - f, 1), (f, 0, 1), (1, 0, 1 - f)][k]
        palette.append(tuple(int(255 * c) for c in rgb))
    pixels = [x * 32 // width for _ in range(height) for x in range(width)]
    return imgenc.gif(width, height, palette, [pixels])


def photo() -> bytes:
    width, height, horizon = 240, 160, 100
    pixels = []
    for y in range(height):
        for x in range(width):
            if y < horizon:
                t = y / horizon  # violet sky to orange at the horizon
                rgb = (60 + 195 * t, 20 + 120 * t * t, 110 - 70 * t)
                if (x - 120) ** 2 + (y - 78) ** 2 < 38 ** 2 and (y < 70 or (y // 4) % 2 == 0):
                    rgb = (255, 200 - (y - 40) * 2, 60)  # the striped sun
                ridge = 82 + 9 * math.sin(x / 17) + 5 * math.sin(x / 7 + 1)
                if y > ridge:
                    rgb = (40, 10, 70)
            else:
                depth = (y - horizon + 4)
                on_line = (y - horizon) % max(2, int(depth / 4)) == 0
                vanish = (x - 120) * 24 / depth
                on_line = on_line or abs(vanish - round(vanish / 12) * 12) < 0.9
                rgb = (255, 40, 200) if on_line else (25, 0, 45)
            pixels.append(tuple(max(0, min(255, int(c))) for c in rgb))
    return jpegenc.jpeg(width, height, pixels)


def construction() -> bytes:
    width, height = 88, 31
    palette = [(0, 0, 0), (255, 215, 0), (255, 255, 255), (220, 0, 0)]
    _, _, under = text_mask("UNDER", 1)
    _, _, works = text_mask("CONSTRUCTION", 1)
    frames = []
    for frame in range(4):
        pixels = []
        for y in range(height):
            for x in range(width):
                if y < 4 or y >= height - 4:
                    pixels.append(1 if ((x + y + frame * 2) // 4) % 2 == 0 else 0)
                elif (x - 30, y - 6) in under:
                    pixels.append(3 if frame % 2 else 2)  # the word blinks
                elif (x - 9, y - 17) in works:
                    pixels.append(1)
                else:
                    pixels.append(0)
        frames.append(pixels)
    return imgenc.gif(width, height, palette, frames, delay_cs=25)


PICTURES = {
    "bg_tile.gif": bg_tile,
    "logo.png": logo,
    "rainbow.gif": rainbow,
    "photo.jpg": photo,
    "construction.gif": construction,
}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true", help="only compare with the checked-in files")
    args = parser.parse_args(argv)
    IMAGES.mkdir(parents=True, exist_ok=True)
    stale = []
    for name, draw in PICTURES.items():
        data, path = draw(), IMAGES / name
        if args.check:
            if not path.is_file() or path.read_bytes() != data:
                stale.append(name)
            continue
        path.write_bytes(data)
        print(f"{path.relative_to(HERE)} ({len(data)} bytes)")
    if stale:
        print(f"stale: {', '.join(stale)}; run python tools/web/gen_fixtures.py", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
