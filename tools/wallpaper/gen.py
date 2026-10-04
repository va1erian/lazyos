#!/usr/bin/env python3
"""Render the desktop pictures LazyOS ships in /system/share/wallpapers.

    python tools/wallpaper/gen.py            # rewrite assets/wallpapers/*.jpg
    python tools/wallpaper/gen.py --only Aurora

Two abstract pictures (Aurora, Dunes) and two branded ones (LazyOS-Night,
LazyOS-Green), all original and procedural, so they carry no third-party
licence. They are 2560x1440 (the HiDPI screen; LazyShell scales them down on
a 1280x720 one) JPEGs: a smooth gradient is several megabytes as a PNG.

Needs numpy and Pillow. The outputs are checked in: the image build only
copies them (build_support/wallpapers_embed.rs), so nobody needs this script
to build LazyOS.
"""

import argparse
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "assets" / "wallpapers"
FONT = ROOT / "assets" / "fonts" / "DroidSans-Bold.ttf"
W, H = 2560, 1440
QUALITY = 88

# The desktop's own colours (libs/uitheme): the dark background and the accent.
NAVY = (18, 22, 36)
ACCENT = (44, 112, 74)


def grid():
    """Pixel coordinates normalised to x in 0..1 and y in 0..1."""
    y, x = np.mgrid[0:H, 0:W].astype(np.float32)
    return x / (W - 1), y / (H - 1)


def rgb(color):
    return np.array(color, dtype=np.float32)


def lerp(a, b, t):
    """Blend colour `a` toward `b` by the per-pixel weight `t`."""
    return rgb(a) + (rgb(b) - rgb(a)) * t[..., None]


def over(image, color, alpha):
    """Composite a flat `color` over `image` with per-pixel `alpha`."""
    return image + (rgb(color) - image) * alpha[..., None]


def smoothstep(edge0, edge1, value):
    t = np.clip((value - edge0) / (edge1 - edge0), 0.0, 1.0)
    return t * t * (3.0 - 2.0 * t)


def stars(image, rng, count, below=0.75):
    """Scatter faint one- and two-pixel stars over the top of the picture."""
    for _ in range(count):
        x, y = rng.integers(0, W), rng.integers(0, int(H * below))
        glow = rng.uniform(40, 150) * (1.0 - y / (H * below)) ** 0.5
        size = 2 if rng.random() < 0.3 else 1
        image[y:y + size, x:x + size] += glow
    return image


def crescent(x, y, cx, cy, radius, bite=(0.42, -0.18)):
    """Coverage of a crescent moon: a disc minus a disc offset by `bite`."""
    aspect = W / H
    edge = 1.5 / H
    def disc(px, py, r):
        d = np.hypot((x - px) * aspect, y - py)
        return 1.0 - smoothstep(r - edge, r + edge, d)
    cut = disc(cx + bite[0] * radius, cy + bite[1] * radius, radius * 0.86)
    return disc(cx, cy, radius) * (1.0 - cut)


def wordmark(image, text, center, height, color, alpha=1.0):
    """Draw `text` centred on `center` (pixels) with capitals `height` tall."""
    font = ImageFont.truetype(str(FONT), int(height * 1.4))
    mask = Image.new("L", (W, H), 0)
    ImageDraw.Draw(mask).text(center, text, font=font, fill=255, anchor="mm")
    return over(image, color, np.asarray(mask, dtype=np.float32) / 255.0 * alpha)


def hills(image, x, y, layers):
    """Stack rolling silhouettes: (base, amplitude, frequency, phase, colour)."""
    for base, amp, freq, phase, color in layers:
        ridge = (base
                 + amp * np.sin(x * freq + phase)
                 + amp * 0.45 * np.sin(x * freq * 2.3 + phase * 1.7))
        image = over(image, color, smoothstep(-1.0 / H, 1.0 / H, y - ridge))
    return image


def aurora():
    x, y = grid()
    rng = np.random.default_rng(7)
    image = lerp((6, 9, 22), (16, 30, 48), y)
    image = stars(image, rng, 900)
    ribbons = [
        # centre, sway, frequency, phase, thickness, colour, strength
        (0.52, 0.10, 5.0, 0.4, 0.085, (60, 220, 140), 0.95),
        (0.40, 0.08, 3.4, 2.1, 0.070, (40, 190, 190), 0.70),
        (0.30, 0.06, 6.2, 4.0, 0.060, (130, 90, 220), 0.55),
        (0.64, 0.05, 4.1, 5.2, 0.050, (44, 150, 96), 0.60),
    ]
    for centre, sway, freq, phase, thick, color, strength in ribbons:
        curve = (centre + sway * np.sin(x * freq + phase)
                 + sway * 0.4 * np.sin(x * freq * 2.7 + phase * 0.6))
        d = y - curve
        # A sharp lower edge and a long curtain rising above it.
        glow = np.where(d > 0, np.exp(-(d / (thick * 0.35)) ** 2),
                        np.exp(-(np.abs(d) / (thick * 2.6)) ** 1.5))
        rays = (0.84 + 0.10 * np.sin(x * 71.0 + 7.0 * np.sin(x * 9.0 + phase))
                + 0.06 * np.sin(x * 173.0 + phase * 3.0))
        image += rgb(color) * (glow * rays * strength)[..., None]
    return hills(image, x, y, [
        (0.90, 0.020, 7.0, 1.0, (10, 20, 28)),
        (0.95, 0.015, 11.0, 3.0, (5, 10, 16)),
    ])


def dunes():
    x, y = grid()
    image = lerp((236, 226, 204), (246, 214, 176), y)
    # A low pale sun and its haze.
    sun = np.hypot((x - 0.70) * W / H, y - 0.30)
    image = over(image, (255, 246, 226), 0.9 * (1.0 - smoothstep(0.070, 0.074, sun)))
    image = over(image, (255, 240, 214), 0.35 * np.exp(-(sun / 0.30) ** 2))
    return hills(image, x, y, [
        (0.50, 0.030, 4.0, 0.5, (226, 196, 160)),
        (0.58, 0.040, 3.1, 2.4, (208, 170, 134)),
        (0.67, 0.050, 2.6, 4.4, (176, 136, 112)),
        (0.77, 0.045, 3.6, 1.2, (122, 100, 100)),
        (0.87, 0.040, 2.9, 3.3, (74, 72, 88)),
        (0.95, 0.030, 4.4, 5.6, (44, 48, 66)),
    ])


def lazyos_night():
    x, y = grid()
    rng = np.random.default_rng(21)
    centre = np.hypot((x - 0.5) * 1.2, y - 0.42)
    image = lerp((30, 40, 70), NAVY, smoothstep(0.0, 0.75, centre))
    image = stars(image, rng, 700, below=0.7)
    moon = (0.5, 0.27, 0.085)
    halo = np.hypot((x - moon[0]) * W / H, y - moon[1])
    image = over(image, (120, 200, 160), 0.22 * np.exp(-(halo / 0.22) ** 2))
    image = over(image, (238, 240, 222), crescent(x, y, *moon))
    # The sleeper's "z z z" drifting up and to the right of the moon.
    for i, (dx, dy, size) in enumerate([(0.085, -0.02, 44), (0.115, -0.07, 60), (0.150, -0.13, 80)]):
        at = (int((moon[0] + dx * H / W * 1.78) * W), int((moon[1] + dy) * H))
        image = wordmark(image, "z", at, size, (200, 226, 210), alpha=0.55 + 0.15 * i)
    image = hills(image, x, y, [
        (0.80, 0.030, 3.4, 0.8, (26, 62, 52)),
        (0.87, 0.028, 4.3, 2.9, (22, 46, 44)),
        (0.94, 0.022, 5.6, 4.8, (14, 26, 32)),
    ])
    image = wordmark(image, "LazyOS", (W // 2, int(H * 0.56)), 150, (236, 240, 245))
    return image


def lazyos_green():
    x, y = grid()
    rng = np.random.default_rng(3)
    image = lerp((20, 62, 46), (64, 150, 100), np.clip(0.65 * x + 0.35 * (1.0 - y), 0, 1))
    # Soft translucent discs, like out-of-focus lights.
    for _ in range(16):
        cx, cy = rng.uniform(-0.1, 1.1), rng.uniform(-0.1, 1.1)
        radius = rng.uniform(0.08, 0.30)
        d = np.hypot((x - cx) * W / H, y - cy)
        disc = 1.0 - smoothstep(radius - 0.004, radius + 0.004, d)
        tint = (190, 240, 205) if rng.random() < 0.6 else (10, 40, 30)
        image = over(image, tint, disc * rng.uniform(0.05, 0.11))
    mark_y = 0.47
    image = over(image, (240, 248, 240), crescent(x, y, 0.345, mark_y - 0.004, 0.058))
    image = wordmark(image, "LazyOS", (int(W * 0.545), int(H * mark_y)), 150, (240, 248, 240))
    return image


PICTURES = {
    "Aurora": aurora,
    "Dunes": dunes,
    "LazyOS-Night": lazyos_night,
    "LazyOS-Green": lazyos_green,
}


def save(name, image):
    # A little noise hides the banding 8 bits leave in a slow gradient.
    rng = np.random.default_rng(len(name))
    image = image + rng.uniform(-1.2, 1.2, image.shape[:2])[..., None]
    pixels = np.clip(image + 0.5, 0, 255).astype(np.uint8)
    path = OUT / f"{name}.jpg"
    Image.fromarray(pixels, "RGB").save(path, quality=QUALITY, optimize=True, subsampling=0)
    print(f"{path.relative_to(ROOT)} {path.stat().st_size // 1024} KiB")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--only", choices=sorted(PICTURES), help="render one picture")
    args = parser.parse_args()
    OUT.mkdir(parents=True, exist_ok=True)
    for name, render in PICTURES.items():
        if args.only in (None, name):
            save(name, render())


if __name__ == "__main__":
    main()
