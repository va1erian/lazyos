#!/usr/bin/env python3
"""Writes tests/fixtures/foreign.ora with Python's zipfile, the way other
OpenRaster editors write one: deflated entries, layers cropped to their
content with x/y offsets, a nested stack, extra attributes.

    python tests/fixtures/make_foreign_ora.py

The Rust test `reads_a_file_another_zip_writer_made` (src/model/format/tests.rs)
opens the result. Everything is deterministic (fixed timestamps)."""
import os
import struct
import zipfile
import zlib


def png(width, height, rgba):
    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    raw = b"".join(b"\x00" + bytes(rgba) * width for _ in range(height))
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


STACK = """<?xml version='1.0' encoding='UTF-8'?>
<image w="6" h="4" version="0.0.5" xres="72" yres="72">
  <stack opacity="1" name="root">
    <layer name="Spot" src="data/spot.png" x="2" y="1" opacity="0.75" visibility="visible" composite-op="svg:multiply" selected="true"/>
    <stack name="Group" opacity="1" visibility="hidden">
      <layer name="Inside" src="data/spot.png" x="0" y="0" visibility="visible"/>
    </stack>
    <layer name="Paper" src="data/paper.png" x="0" y="0" opacity="1.0" visibility="visible"/>
  </stack>
</image>
"""


def main():
    out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "foreign.ora")
    stamp = (2024, 1, 1, 0, 0, 0)
    with zipfile.ZipFile(out, "w") as z:
        def add(name, data, method):
            info = zipfile.ZipInfo(name, stamp)
            info.compress_type = method
            info.external_attr = 0o644 << 16
            z.writestr(info, data)

        add("mimetype", b"image/openraster", zipfile.ZIP_STORED)
        add("stack.xml", STACK.encode(), zipfile.ZIP_DEFLATED)
        add("data/spot.png", png(2, 2, (255, 0, 0, 255)), zipfile.ZIP_DEFLATED)
        add("data/paper.png", png(6, 4, (240, 240, 200, 255)), zipfile.ZIP_DEFLATED)
        add("mergedimage.png", png(6, 4, (240, 240, 200, 255)), zipfile.ZIP_DEFLATED)
    print(out, os.path.getsize(out), "bytes")


if __name__ == "__main__":
    main()
