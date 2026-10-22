#!/usr/bin/env python3
"""The Docs judges fail when they should (tools/docs/judge.py): each check is shown passing on a good run
and failing on a doctored one. Run: python tools/docs/test_judge.py"""

from __future__ import annotations

import struct
import sys
import tempfile
import unittest
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import judge as j  # noqa: E402

WIDTH, HEIGHT = 1000, 700

GOOD_LOG = "\n".join([
    "WEBD:UP:PASS", "WEBD:SERVE:app.os.lazy.webd.light", "DOCS:UP:PASS",
    "WEBD:VIEW:CREATE client=label#12 view=1", "DOCS:RENDER:PASS",
    "DOCS:OPEN:PASS:/system/share/samples/testdoc.md", "DOCS:RENDER:PASS:/system/share/samples/testdoc.md",
])


def write_png(path: Path, painter) -> Path:
    """A WIDTH x HEIGHT RGB PNG whose pixel (x, y) is `painter(x, y)`."""
    rows = bytearray()
    for y in range(HEIGHT):
        rows.append(0)
        for x in range(WIDTH):
            rows += bytes(painter(x, y))
    def chunk(kind: bytes, data: bytes) -> bytes:
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))
    path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", WIDTH, HEIGHT, 8, 2, 0, 0, 0))
                     + chunk(b"IDAT", zlib.compress(bytes(rows))) + chunk(b"IEND", b""))
    return path


def page(offset: int):
    """White with stripes of a few shades `offset` rows down: a page of text scrolled."""
    return lambda x, y: (20 + x % 11 * 9, 20, 20) if (y + offset) % 12 < 3 and x % 40 < 30 else (255, 255, 255)


class MarkerTests(unittest.TestCase):
    def test_a_good_run_passes(self) -> None:
        self.assertEqual(j.judge_markers(GOOD_LOG, j.OPEN_MARKERS), [])

    def test_a_missing_marker_fails(self) -> None:
        text = GOOD_LOG.replace("DOCS:RENDER:PASS\n", "")
        self.assertEqual(j.judge_markers(text, ["DOCS:UP:PASS", "DOCS:RENDER:PASS:/nowhere"]),
                         ["DOCS:RENDER:PASS:/nowhere never appeared"])
        # The page that never paints is the symptom of an image without webd.
        self.assertTrue(j.judge_markers("DOCS:UP:PASS\nDOCS:ENGINE:FAIL:not running", j.WHEEL_MARKERS))

    def test_the_welcome_render_marker_is_not_proved_by_a_documents_longer_one(self) -> None:
        only_document = chr(10).join(line for line in GOOD_LOG.splitlines() if line != "DOCS:RENDER:PASS")
        self.assertEqual(j.judge_markers(only_document, ["DOCS:RENDER:PASS"]), ["DOCS:RENDER:PASS never appeared"])

    def test_a_failure_marker_fails_but_a_webd_note_does_not(self) -> None:
        self.assertTrue(j.judge_markers(GOOD_LOG + "\nDOCS:OPEN:FAIL:/x", j.WHEEL_MARKERS))
        self.assertTrue(j.judge_markers(GOOD_LOG + "\nWEBD:HANG:FAIL:stuck", j.WHEEL_MARKERS))
        self.assertEqual(j.judge_markers(GOOD_LOG + "\nWEBD:NOTE:FAIL:light:x", j.WHEEL_MARKERS), [])

    def test_a_refused_call_of_docs_or_webd_fails(self) -> None:
        self.assertEqual(j.judge_denies(GOOD_LOG), [])
        self.assertTrue(j.judge_denies(GOOD_LOG + "\nLABEL:DENY label=app:os.lazy.docs iface=9 method=1"))
        self.assertTrue(j.judge_denies(GOOD_LOG + "\nLABEL:DENY label=app:os.lazy.webd iface=9 method=1"))
        self.assertTrue(j.judge_denies(
            GOOD_LOG + "\nLABEL:DENY label=app:os.lazy.webd/webprocess file=/home/user access=read"))
        self.assertEqual(j.judge_denies(GOOD_LOG + "\nLABEL:DENY label=app:os.lazy.files iface=9 method=1"), [])

    def test_the_documented_optional_refusals_of_webds_children_pass(self) -> None:
        optional = ("LABEL:DENY label=app:os.lazy.webd/webprocess file=/etc/localtime access=stat\n"
                    "LABEL:DENY label=app:os.lazy.webd/networkprocess file=/etc/ssl/openssl.cnf access=stat\n"
                    "LABEL:DENY label=app:os.lazy.webd/webprocess file=/tmp/fontconfig access=write")
        self.assertEqual(j.judge_denies(GOOD_LOG + "\n" + optional), [])


class PictureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.dir = tempfile.TemporaryDirectory()
        root = Path(cls.dir.name)
        cls.top = write_png(root / "top.png", page(0))
        cls.down = write_png(root / "down.png", page(5))
        cls.blank = write_png(root / "blank.png", lambda x, y: (255, 255, 255))
        cls.moved = write_png(root / "moved.png", page(7))

    @classmethod
    def tearDownClass(cls) -> None:
        cls.dir.cleanup()

    def test_the_wheel_down_and_back_passes(self) -> None:
        self.assertEqual(j.judge_wheel(self.top, self.down, self.top), [])

    def test_a_page_that_did_not_move_fails(self) -> None:
        self.assertIn("the mouse wheel did not scroll the page down", j.judge_wheel(self.top, self.top, self.top))

    def test_a_page_that_did_not_return_fails(self) -> None:
        self.assertIn("wheel-up did not return the page to where it started",
                      j.judge_wheel(self.top, self.down, self.moved))

    def test_a_blank_page_fails(self) -> None:
        problems = j.judge_wheel(self.blank, self.down, self.blank)
        self.assertTrue(any("nearly blank" in p for p in problems), problems)

    def test_an_opened_document_passes_and_an_unchanged_or_blank_one_fails(self) -> None:
        self.assertEqual(j.judge_opened(self.blank, self.top), [])
        self.assertTrue(j.judge_opened(self.top, self.top))
        self.assertTrue(j.judge_opened(self.top, self.blank))

    def test_scrolling_the_opened_document(self) -> None:
        self.assertEqual(j.judge_scrolled(self.top, self.down), [])
        self.assertTrue(j.judge_scrolled(self.top, self.top))


if __name__ == "__main__":
    unittest.main()
