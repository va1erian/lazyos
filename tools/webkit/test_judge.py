#!/usr/bin/env python3
"""The judge must fail when it should (tools/webkit/wkjudge.py)."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import wkjudge  # noqa: E402

INFO = ("WEBKIT:W3:INFO:zlib:crc32=e52dc18f\nWEBKIT:W3:INFO:libpng:pixels=ebe5e73b\n"
        "WEBKIT:W3:INFO:libjpeg-turbo:pixels=1c905bb2\nWEBKIT:W3:INFO:libwebp:pixels=ebe5e73b\n"
        "WEBKIT:W3:INFO:freetype:glyph-A=edb00a86\nWEBKIT:W3:INFO:harfbuzz:glyphs=32\n"
        "WEBKIT:W3:INFO:harfbuzz:advance=28953\nWEBKIT:W3:INFO:skia:pixels=9ab89b56\n"
        "WEBKIT:W3:INFO:skia:inked=900+1439\n")
HOST = "WEBKIT:W3:UP\n" + INFO + "WEBKIT:W3:OFFLINE\n"
GOOD = ("WEBKIT:W3:UP\n" + INFO + "WEBKIT:W3:INFO:curl:sha256=0123456789abcdef\n"
        + "".join(f"WEBKIT:W3:PASS:{lib}\n" for lib in wkjudge.LIBS) + "WEBKIT:W3:PASS\n")


def failing(serial: str, **kwargs) -> list[str]:
    return [v.name for v in wkjudge.judge(serial, HOST, **kwargs) if not v.ok]


class JudgeTests(unittest.TestCase):
    def test_a_complete_log_passes(self):
        self.assertTrue(wkjudge.passed(wkjudge.judge(GOOD, HOST, "0123456789abcdef", [])))

    def test_an_empty_log_fails_every_marker(self):
        names = failing("")
        self.assertIn("WEBKIT:W3:UP", names)
        self.assertIn("WEBKIT:W3:PASS", names)
        for lib in wkjudge.LIBS:
            self.assertIn(f"WEBKIT:W3:PASS:{lib}", names)

    def test_the_final_line_is_not_a_library_line(self):
        # `WEBKIT:W3:PASS:curl` must not stand in for `WEBKIT:W3:PASS`.
        serial = GOOD.replace("WEBKIT:W3:PASS\n", "")
        self.assertEqual(failing(serial), ["WEBKIT:W3:PASS"])

    def test_each_missing_library_is_named(self):
        for lib in wkjudge.LIBS:
            serial = GOOD.replace(f"WEBKIT:W3:PASS:{lib}\n", "")
            self.assertEqual(failing(serial), [f"WEBKIT:W3:PASS:{lib}"], lib)

    def test_a_failure_line_fails_even_with_every_pass_marker(self):
        serial = GOOD + "WEBKIT:W3:FAIL:skia:no typeface for Liberation Sans\n"
        self.assertEqual(failing(serial), ["no failure lines"])
        self.assertIn("typeface", wkjudge.judge(serial, HOST)[len(wkjudge.LIBS) + 2].detail)

    def test_a_killed_task_fails(self):
        serial = GOOD + "user: task 12 killed by SIGSEGV at 0x0\n"
        self.assertEqual(failing(serial), ["no failure lines"])

    def test_the_offline_host_run_is_not_a_pass(self):
        serial = GOOD.replace("WEBKIT:W3:PASS\n", "WEBKIT:W3:OFFLINE\n").replace("PASS:curl\n", "SKIP:curl:offline\n")
        self.assertIn("WEBKIT:W3:PASS", failing(serial))
        self.assertIn("WEBKIT:W3:PASS:curl", failing(serial))

    def test_pixels_must_match_the_host(self):
        serial = GOOD.replace("skia:pixels=9ab89b56", "skia:pixels=deadbeef")
        self.assertEqual(failing(serial), ["skia pixels equals the host's"])
        self.assertIn("deadbeef", [v for v in wkjudge.judge(serial, HOST) if not v.ok][0].detail)

    def test_a_missing_value_is_not_a_match(self):
        serial = GOOD.replace("WEBKIT:W3:INFO:libwebp:pixels=ebe5e73b\n", "")
        self.assertEqual(failing(serial), ["libwebp pixels equals the host's"])

    def test_a_missing_host_reference_fails_the_comparison(self):
        self.assertIn("skia pixels equals the host's",
                      [v.name for v in wkjudge.judge(GOOD, "") if not v.ok])

    def test_the_fetched_page_must_be_the_servers(self):
        names = [v.name for v in wkjudge.judge(GOOD, HOST, "ffffffffffffffff") if not v.ok]
        self.assertEqual(names, ["curl fetched the page the server holds"])

    def test_the_server_must_have_seen_the_request(self):
        verdicts = wkjudge.judge(GOOD, HOST, None, ["the server was never asked for /"])
        self.assertEqual([v.name for v in verdicts if not v.ok],
                         ["the harness server saw the handshake and the request"])


if __name__ == "__main__":
    unittest.main()
