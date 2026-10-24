#!/usr/bin/env python3
"""Write the kernel's boot splash, assets/boot/splash.rgb.

    python tools/wallpaper/splash.py

The kernel mux paints it (with a spinner on top) until the desktop session
takes the screen, and has no image decoder, so the picture is the shipped
LazyOS-Night wallpaper reduced to a raw 480x270 RGB file (8-bit, row after
row, no header) that the kernel scales to the screen with `include_bytes!`.
Needs Pillow. The output is checked in; the build never runs this.
"""

from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "assets" / "wallpapers" / "LazyOS-Night.jpg"
OUT = ROOT / "assets" / "boot" / "splash.rgb"
SIZE = (480, 270)


def main():
    picture = Image.open(SOURCE).convert("RGB").resize(SIZE, Image.LANCZOS)
    OUT.write_bytes(picture.tobytes())
    print(f"{OUT}: {SIZE[0]}x{SIZE[1]}, {OUT.stat().st_size} bytes")


if __name__ == "__main__":
    main()
