#!/usr/bin/env python3
"""Writes the Video Player's sample clip (docs/video-player-plan.md).

    python tools/video/make_sample.py            # write it
    python tools/video/make_sample.py --check    # fail if the checked-in one is stale

`assets/samples/lazyos-sample.avi` ships in every desktop image
(`/system/share/samples/lazyos-sample.avi`); the player's session opens it.
It is a Motion JPEG AVI drawn here with Pillow (colour bars sliding, a
bouncing square, a frame counter and a progress bar), so it needs no other
tool. The text uses the bundled Droid Sans, not Pillow's default font; the
JPEG bytes still come from the host's libjpeg, so `--check` validates the
checked-in clip's structure (size, rate, frame count, every frame a JPEG)
instead of comparing bytes.

A host tool only; nothing it links ends up in the image.
"""

from __future__ import annotations

import argparse
import io
import struct
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[2]
SAMPLE = ROOT / "assets" / "samples" / "lazyos-sample.avi"

WIDTH, HEIGHT, FPS, SECONDS = 320, 180, 25, 5
FRAMES = FPS * SECONDS
BARS = [(235, 235, 235), (235, 235, 20), (20, 235, 235), (20, 235, 20),
        (235, 20, 235), (235, 20, 20), (20, 20, 235), (30, 30, 30)]


def frame(index: int, font: ImageFont.ImageFont) -> bytes:
    """One picture as a baseline JPEG."""
    im = Image.new("RGB", (WIDTH, HEIGHT))
    d = ImageDraw.Draw(im)
    bar_w = WIDTH // len(BARS)
    shift = (index * 4) % WIDTH
    for n, colour in enumerate(BARS * 2):
        x = n * bar_w - shift
        d.rectangle([x, 0, x + bar_w - 1, HEIGHT - 1], fill=colour)
    # A square bouncing on a triangle wave, so motion is easy to see.
    span_x, span_y = WIDTH - 40, HEIGHT - 60
    px = abs((index * 5) % (2 * span_x) - span_x)
    py = abs((index * 3) % (2 * span_y) - span_y)
    d.rectangle([px, py + 20, px + 39, py + 59], fill=(0, 0, 0), outline=(255, 255, 255), width=2)
    d.text((8, 4), f"LazyOS  {index:03d}/{FRAMES}", fill=(0, 0, 0), font=font)
    d.rectangle([0, HEIGHT - 8, WIDTH * index // (FRAMES - 1), HEIGHT - 1], fill=(255, 255, 255))
    out = io.BytesIO()
    im.save(out, "JPEG", quality=70, subsampling=2)
    return out.getvalue()


def chunk(tag: bytes, body: bytes) -> bytes:
    pad = b"\0" if len(body) % 2 else b""
    return tag + struct.pack("<I", len(body)) + body + pad


def lst(form: bytes, *parts: bytes) -> bytes:
    return chunk(b"LIST", form + b"".join(parts))


def avi(frames: list[bytes]) -> bytes:
    """The frames in an AVI with an `idx1` index."""
    u32 = lambda *v: struct.pack("<%dI" % len(v), *v)  # noqa: E731
    avih = u32(1_000_000 // FPS, 0, 0, 0x10, len(frames), 0, 1,
               max(map(len, frames)), WIDTH, HEIGHT, 0, 0, 0, 0)
    strh = b"vidsMJPG" + u32(0, 0, 0, 1, FPS, 0, len(frames),
                             max(map(len, frames)), 0xFFFFFFFF, 0) + struct.pack("<4h", 0, 0, WIDTH, HEIGHT)
    strf = u32(40, WIDTH, HEIGHT) + struct.pack("<HH", 1, 24) + b"MJPG" + u32(WIDTH * HEIGHT * 3, 0, 0, 0, 0)
    hdrl = lst(b"hdrl", chunk(b"avih", avih), lst(b"strl", chunk(b"strh", strh), chunk(b"strf", strf)))
    movi_parts, index, offset = [], b"", 4  # offsets count from the `movi` tag
    for f in frames:
        c = chunk(b"00dc", f)
        movi_parts.append(c)
        index += b"00dc" + u32(0x10, offset, len(f))
        offset += len(c)
    movi = lst(b"movi", *movi_parts)
    return chunk(b"RIFF", b"AVI " + hdrl + movi + chunk(b"idx1", index))


def check(data: bytes) -> str | None:
    """Why `data` is not the expected clip, or None when it is."""
    if data[:4] != b"RIFF" or data[8:12] != b"AVI ":
        return "not an AVI"
    movi = data.find(b"movi")
    if movi < 0:
        return "no movi list"
    u32 = lambda at: struct.unpack_from("<I", data, at)[0]  # noqa: E731
    avih = data.find(b"avih") + 8  # the chunk's body
    if (u32(avih + 32), u32(avih + 36)) != (WIDTH, HEIGHT):
        return f"not {WIDTH}x{HEIGHT}"
    if u32(data.find(b"strh") + 8 + 24) != FPS:
        return f"not {FPS} fps"
    frames, pos = 0, movi + 4
    while pos + 8 <= len(data) and data[pos : pos + 4] == b"00dc":
        size = u32(pos + 4)
        body = data[pos + 8 : pos + 8 + size]
        if body[:2] != b"\xff\xd8" or body[-2:] != b"\xff\xd9":
            return f"frame {frames} is not a JPEG"
        frames += 1
        pos += 8 + size + (size & 1)
    return None if frames == FRAMES else f"{frames} frames, expected {FRAMES}"


def build() -> bytes:
    font = ImageFont.truetype(str(ROOT / "assets" / "fonts" / "DroidSans.ttf"), 11)
    return avi([frame(i, font) for i in range(FRAMES)])


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true", help="fail if the checked-in clip is not the expected video")
    args = parser.parse_args()
    if args.check:
        problem = check(SAMPLE.read_bytes() if SAMPLE.exists() else b"")
        if problem:
            print(f"{SAMPLE.relative_to(ROOT)}: {problem}; run tools/video/make_sample.py", file=sys.stderr)
            return 1
        print(f"{SAMPLE.relative_to(ROOT)}: ok ({SAMPLE.stat().st_size} bytes)")
        return 0
    data = build()
    SAMPLE.parent.mkdir(parents=True, exist_ok=True)
    SAMPLE.write_bytes(data)
    print(f"wrote {SAMPLE.relative_to(ROOT)} ({len(data)} bytes, {FRAMES} frames)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
