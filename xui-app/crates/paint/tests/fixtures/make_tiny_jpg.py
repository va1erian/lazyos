#!/usr/bin/env python3
"""Writes tests/fixtures/tiny.jpg: a 24 x 16 baseline JPEG, red on the left
half and blue on the right, for the tests that open a real JPEG (Paint opens
JPEG but cannot write one, so no test can make its own).

    python tests/fixtures/make_tiny_jpg.py          (needs Pillow)
"""
import os

from PIL import Image

image = Image.new("RGB", (24, 16), (200, 40, 40))
for x in range(12, 24):
    for y in range(16):
        image.putpixel((x, y), (40, 40, 200))
here = os.path.dirname(os.path.abspath(__file__))
image.save(os.path.join(here, "tiny.jpg"), quality=95, subsampling=0)
