#!/usr/bin/env python3
"""The Mail harness's reading-pane judges fail when they should (tools/mail/webjudge.py, hostile.py).
Run: python tools/mail/test_judge.py"""

from __future__ import annotations

import struct
import sys
import tempfile
import unittest
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "webd"))
sys.path.insert(0, str(HERE))  # first: `run` is this directory's (tools/webd has one too)
import hostile  # noqa: E402
import run  # noqa: E402  (this directory's: it loads tools/webd/judge.py under another name)
import webjudge  # noqa: E402

WIDTH, HEIGHT = 1000, 640
BOX = (560, 1000, 100, 600)


def write_png(path: Path, painter) -> Path:
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


def text_page(x: int, y: int):
    """White with text-like grey shades."""
    return (30 + x % 13 * 12, 30, 30) if y % 14 < 3 and x % 50 < 40 else (255, 255, 255)


def blue_heading_page(x: int, y: int):
    return (0, 0, 255) if 150 < y < 190 and 600 < x < 900 else (255, 255, 255)


class ShotTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.dir = tempfile.TemporaryDirectory()
        root = Path(cls.dir.name)
        cls.text = write_png(root / "text.png", text_page)
        cls.blank = write_png(root / "blank.png", lambda x, y: (255, 255, 255))
        cls.blue = write_png(root / "blue.png", blue_heading_page)

    @classmethod
    def tearDownClass(cls) -> None:
        cls.dir.cleanup()

    def test_a_pane_with_text_passes_and_a_blank_one_fails(self) -> None:
        self.assertEqual(webjudge.judge_message_shot(self.text, BOX, "fixture"), [])
        self.assertTrue(webjudge.judge_message_shot(self.blank, BOX, "fixture"))

    def test_the_hostile_heading_must_paint_in_blue(self) -> None:
        self.assertEqual(webjudge.judge_hostile_shot(self.blue, BOX), [])
        self.assertTrue(webjudge.judge_hostile_shot(self.text, BOX))
        self.assertTrue(webjudge.judge_hostile_shot(self.blank, BOX))


class RequestTests(unittest.TestCase):
    def judge(self, record: dict) -> list[str]:
        return webjudge.judge_requests(run.webd_judge, record)

    def test_only_the_guests_own_control_request_passes(self) -> None:
        control = {"method": "GET", "path": "/control", "client": "127.0.0.1"}
        self.assertEqual(self.judge({"requests": [control], "connections": 1}), [])

    def test_a_request_from_the_page_fails(self) -> None:
        control = {"method": "GET", "path": "/control", "client": "127.0.0.1"}
        for path in ("/img.png", "/script.js", "/refresh", "/form", "/frame"):
            hit = {"method": "GET", "path": path, "client": "127.0.0.1"}
            self.assertTrue(self.judge({"requests": [control, hit], "connections": 2}), path)

    def test_a_connection_that_sent_nothing_fails(self) -> None:
        control = {"method": "GET", "path": "/control", "client": "127.0.0.1"}
        self.assertTrue(self.judge({"requests": [control], "connections": 2}))

    def test_a_missing_control_request_fails(self) -> None:
        """Nothing reached the listener at all: the harness cannot tell a clean pane from a dead listener."""
        self.assertTrue(self.judge({"requests": [], "connections": 0}))


class EngineTests(unittest.TestCase):
    GOOD = "WEBD:UP:PASS\nWEBD:VIEW:CREATE client=label#7 view=1\nMAIL:RENDER:PASS"

    def test_a_run_with_webd_passes(self) -> None:
        self.assertEqual(webjudge.judge_engine(self.GOOD), [])

    def test_a_run_without_webd_or_a_view_fails(self) -> None:
        self.assertTrue(webjudge.judge_engine("MAIL:RENDER:PASS"))
        self.assertTrue(webjudge.judge_engine("WEBD:UP:PASS"))

    def test_a_webd_failure_fails_but_a_note_does_not(self) -> None:
        self.assertTrue(webjudge.judge_engine(self.GOOD + "\nWEBD:HANG:FAIL:stuck"))
        self.assertEqual(webjudge.judge_engine(self.GOOD + "\nWEBD:NOTE:FAIL:light:x"), [])


class HostileMessageTests(unittest.TestCase):
    def test_it_points_the_page_at_the_listener_every_way_it_can(self) -> None:
        urls = hostile.requests_in(hostile.BODY)
        for kind in ("refresh", "style.css", "script.js", "fetch", "image", "onload", "img.png", "frame",
                     "form", "bg.png", "link"):
            self.assertIn(f"{hostile.LISTENER}/{kind}", urls)

    def test_it_is_a_crlf_html_message_with_a_blue_heading(self) -> None:
        raw = hostile.raw_message().decode()
        self.assertIn("Content-Type: text/html", raw)
        self.assertIn(f"Subject: {hostile.SUBJECT}", raw)
        self.assertIn(hostile.HEADING, raw)
        self.assertNotIn("\n", raw.replace("\r\n", ""))

    def test_the_listener_is_the_one_the_webd_harness_uses(self) -> None:
        import listener
        self.assertEqual(hostile.LISTENER, f"http://10.0.2.2:{listener.PORT}")


if __name__ == "__main__":
    unittest.main()
