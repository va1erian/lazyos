"""Unit tests for pngstats.py (no QEMU, no third-party libs):
python tools/screenshot/test_pngstats.py"""

from __future__ import annotations

import contextlib
import io
import struct
import sys
import tempfile
import unittest
import zlib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pngstats  # noqa: E402

_CHANNELS = {0: 1, 2: 3, 4: 2, 6: 4}


def _paeth(a: int, b: int, c: int) -> int:
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    if pa <= pb and pa <= pc:
        return a
    if pb <= pc:
        return b
    return c


def _chunk(kind: bytes, data: bytes) -> bytes:
    return (struct.pack(">I", len(data)) + kind + data
            + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF))


def encode_png(path: Path, width: int, height: int, color_type: int,
               rows: list[bytes], filters: list[int]) -> None:
    """Write a non-interlaced 8-bit PNG; `rows` are unfiltered scanlines."""
    channels = _CHANNELS[color_type]
    stride = width * channels
    raw = bytearray()
    previous = bytearray(stride)
    for line, filter_type in zip(rows, filters):
        assert len(line) == stride
        filtered = bytearray(line)
        if filter_type == 1:  # Sub
            for x in range(channels, stride):
                filtered[x] = (line[x] - line[x - channels]) & 0xFF
        elif filter_type == 2:  # Up
            for x in range(stride):
                filtered[x] = (line[x] - previous[x]) & 0xFF
        elif filter_type == 3:  # Average
            for x in range(stride):
                left = line[x - channels] if x >= channels else 0
                filtered[x] = (line[x] - ((left + previous[x]) >> 1)) & 0xFF
        elif filter_type == 4:  # Paeth
            for x in range(stride):
                left = line[x - channels] if x >= channels else 0
                up = previous[x]
                up_left = previous[x - channels] if x >= channels else 0
                filtered[x] = (line[x] - _paeth(left, up, up_left)) & 0xFF
        elif filter_type != 0:
            raise ValueError(filter_type)
        raw.append(filter_type)
        raw += filtered
        previous = bytearray(line)
    ihdr = struct.pack(">IIBBBBB", width, height, 8, color_type, 0, 0, 0)
    path.write_bytes(
        b"\x89PNG\r\n\x1a\n" + _chunk(b"IHDR", ihdr)
        + _chunk(b"IDAT", zlib.compress(bytes(raw))) + _chunk(b"IEND", b"")
    )


def reference_analyse(width: int, height: int, channels: int, pixels: bytes,
                      bg: tuple[int, int, int] = (0, 0, 0),
                      threshold: int = 12) -> dict:
    """The pre-Counter implementation, kept as the correctness reference."""
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
        else:
            r = g = b = pixels[o]
            alpha = pixels[o + 1] if channels == 2 else 255
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


def rgb_rows(width: int, height: int) -> list[bytes]:
    rows = []
    for y in range(height):
        row = bytearray()
        for x in range(width):
            row += bytes(((x * 40 + y * 7) % 256,
                          (x * 13 + y * 80) % 256,
                          (x * 90 + y * 3) % 256))
        rows.append(bytes(row))
    return rows


class AnalyseReferenceTests(unittest.TestCase):
    def assert_matches_reference(self, path: Path) -> None:
        width, height, channels, pixels = pngstats.decode_png(path)
        self.assertEqual(pngstats.analyse(width, height, channels, pixels),
                         reference_analyse(width, height, channels, pixels))

    def test_rgb_matches_the_reference_for_every_filter_type(self):
        width, height = 5, 3
        rows = rgb_rows(width, height)
        for filter_type in range(5):
            with self.subTest(filter=filter_type), tempfile.TemporaryDirectory() as work:
                path = Path(work) / "rgb.png"
                encode_png(path, width, height, 2, rows, [filter_type] * height)
                decoded = pngstats.decode_png(path)
                self.assertEqual(decoded[3], b"".join(rows))
                self.assert_matches_reference(path)

    def test_rgba_with_transparent_pixels_matches_the_reference(self):
        width, height = 4, 3
        rows = []
        for y in range(height):
            row = bytearray()
            for x in range(width):
                alpha = 0 if (x + y) % 3 == 0 else 255
                row += bytes(((x * 31 + y * 11) % 256,
                              (x * 17 + y * 53) % 256,
                              (x * 97 + y * 5) % 256, alpha))
            rows.append(bytes(row))
        with tempfile.TemporaryDirectory() as work:
            path = Path(work) / "rgba.png"
            encode_png(path, width, height, 6, rows, [0, 4, 3])
            self.assert_matches_reference(path)

    def test_greyscale_matches_the_reference(self):
        width, height = 4, 3
        rows = [bytes((x * 60 + y * 20) % 256 for x in range(width))
                for y in range(height)]
        with tempfile.TemporaryDirectory() as work:
            path = Path(work) / "grey.png"
            encode_png(path, width, height, 0, rows, [2, 1, 4])
            self.assert_matches_reference(path)

    def test_grey_alpha_matches_the_reference(self):
        width, height = 3, 2
        rows = []
        for y in range(height):
            row = bytearray()
            for x in range(width):
                row += bytes(((x * 70 + y * 30) % 256, 0 if x == y else 255))
            rows.append(bytes(row))
        with tempfile.TemporaryDirectory() as work:
            path = Path(work) / "grey_alpha.png"
            encode_png(path, width, height, 4, rows, [0, 4])
            self.assert_matches_reference(path)

    def test_a_fully_transparent_rgba_image_is_black(self):
        width, height = 2, 2
        rows = [bytes((200, 100, 50, 0)) * width for _ in range(height)]
        with tempfile.TemporaryDirectory() as work:
            path = Path(work) / "clear.png"
            encode_png(path, width, height, 6, rows, [0, 0])
            stats = pngstats.analyse(*pngstats.decode_png(path))
        self.assertEqual(stats["mean_rgb"], 0.0)
        self.assertEqual(stats["min_luminance"], 0)
        self.assertEqual(stats["max_luminance"], 0)
        self.assertEqual(stats["distinct_colors_q4"], 1)


class MainTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.work = Path(self._tmp.name)

    def make(self, name: str, colour: tuple[int, int, int]) -> Path:
        path = self.work / name
        rows = [bytes(colour) * 3, bytes(colour) * 3]
        encode_png(path, 3, 2, 2, rows, [0, 0])
        return path

    def run_main(self, argv: list[str]) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = pngstats.main(argv)
        return code, out.getvalue(), err.getvalue()

    def test_one_file_succeeds(self):
        path = self.make("one.png", (10, 200, 30))
        code, out, _ = self.run_main([str(path), "--min-nonblack", "0.5"])
        self.assertEqual(code, 0)
        self.assertIn(f"{path}: 3x2 ch=3", out)

    def test_output_order_follows_the_argument_order(self):
        first = self.make("first.png", (255, 0, 0))
        second = self.make("second.png", (0, 255, 0))
        third = self.make("third.png", (0, 0, 255))
        code, out, _ = self.run_main([str(third), str(first), str(second)])
        self.assertEqual(code, 0)
        self.assertLess(out.index(str(third)), out.index(str(first)))
        self.assertLess(out.index(str(first)), out.index(str(second)))

    def test_a_broken_file_is_reported_and_does_not_abort_the_others(self):
        good_before = self.make("before.png", (40, 40, 40))
        broken = self.work / "broken.png"
        broken.write_bytes(b"definitely not a png")
        good_after = self.make("after.png", (200, 200, 200))
        code, out, err = self.run_main([str(good_before), str(broken), str(good_after)])
        self.assertEqual(code, 1)
        self.assertIn(f"{good_before}: 3x2", out)
        self.assertIn(f"{good_after}: 3x2", out)
        self.assertIn(f"{broken}: ERROR:", out)
        self.assertIn("FAILURES:", err)
        # The broken file is reported in argument order, between the two good ones.
        self.assertLess(out.index(str(good_before)), out.index(str(broken)))
        self.assertLess(out.index(str(broken)), out.index(str(good_after)))

    def test_a_corrupt_idat_is_reported_for_that_file(self):
        ihdr = struct.pack(">IIBBBBB", 2, 2, 8, 2, 0, 0, 0)
        corrupt = (b"\x89PNG\r\n\x1a\n" + _chunk(b"IHDR", ihdr)
                   + _chunk(b"IDAT", b"not zlib data") + _chunk(b"IEND", b""))
        broken = self.work / "corrupt.png"
        broken.write_bytes(corrupt)
        good = self.make("good.png", (10, 20, 30))
        code, out, _ = self.run_main([str(broken), str(good)])
        self.assertEqual(code, 1)
        self.assertIn(f"{broken}: ERROR:", out)
        self.assertIn(f"{good}: 3x2", out)

    def test_json_output_keeps_argument_order_and_errors(self):
        import json

        good = self.make("good.png", (1, 2, 3))
        broken = self.work / "bad.png"
        broken.write_bytes(b"nope")
        code, out, _ = self.run_main([str(good), str(broken), "--json"])
        self.assertEqual(code, 1)
        parsed = json.loads(out)
        self.assertEqual(list(parsed), [str(good), str(broken)])
        self.assertIn("error", parsed[str(broken)])


if __name__ == "__main__":
    unittest.main()
