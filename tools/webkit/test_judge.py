#!/usr/bin/env python3
"""The judge must fail when it should (tools/webkit/wkjudge.py)."""

from __future__ import annotations

import hashlib
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import wkjudge  # noqa: E402

BIN = "ab" * 32
INFO = ("WEBKIT:W3:INFO:zlib:crc32=e52dc18f\nWEBKIT:W3:INFO:libpng:pixels=ebe5e73b\n"
        "WEBKIT:W3:INFO:libjpeg-turbo:pixels=1c905bb2\nWEBKIT:W3:INFO:libwebp:pixels=ebe5e73b\n"
        "WEBKIT:W3:INFO:freetype:glyph-A=edb00a86\nWEBKIT:W3:INFO:harfbuzz:glyphs=32\n"
        "WEBKIT:W3:INFO:harfbuzz:advance=28953\nWEBKIT:W3:INFO:skia:pixels=9ab89b56\n"
        "WEBKIT:W3:INFO:skia:inked=900+1439\n")
HOST = f"WEBKIT:W3:UP\nWEBKIT:W3:BIN:{BIN}\n" + INFO + "WEBKIT:W3:OFFLINE\n"
GOOD = (f"WEBKIT:W3:UP\nWEBKIT:W3:BIN:{BIN}\n" + INFO + "WEBKIT:W3:INFO:curl:sha256=0123456789abcdef\n"
        + "".join(f"WEBKIT:W3:PASS:{lib}\n" for lib in wkjudge.LIBS) + "WEBKIT:W3:PASS\n")
BIN_LINE = f"WEBKIT:W3:BIN:{BIN}\n"


def failing(serial: str, host: str = HOST, **kwargs) -> list[str]:
    return [v.name for v in wkjudge.judge(serial, host, **kwargs) if not v.ok]


class JudgeTests(unittest.TestCase):
    def test_a_complete_log_passes(self):
        self.assertTrue(wkjudge.passed(wkjudge.judge(GOOD, HOST, "0123456789abcdef", [], BIN)))

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


class BinaryTests(unittest.TestCase):
    """The guest, the host reference and target/webkit/bin must be one build."""

    def test_a_guest_that_ran_another_binary_fails_with_the_cause(self):
        serial = GOOD.replace(BIN, "cd" * 32)
        self.assertEqual(failing(serial), ["the guest ran the binary the host reference ran"])
        detail = [v for v in wkjudge.judge(serial, HOST) if not v.ok][0].detail
        self.assertIn("cdcdcdcdcdcd", detail)
        self.assertIn("abababababab", detail)
        self.assertIn("without --no-build", detail)

    def test_a_guest_that_printed_no_digest_fails(self):
        self.assertEqual(failing(GOOD.replace(BIN_LINE, "")),
                         ["the guest ran the binary the host reference ran"])
        self.assertEqual(failing(GOOD.replace(BIN, "unreadable")),
                         ["the guest ran the binary the host reference ran"])

    def test_a_host_reference_without_a_digest_fails(self):
        names = failing(GOOD, HOST.replace(BIN_LINE, ""))
        self.assertIn("the host reference names its binary", names)
        self.assertIn("the guest ran the binary the host reference ran", names)

    def test_a_reference_older_than_the_binary_fails(self):
        # deps.py --only X rebuilt target/webkit/bin/depsproof after the reference.
        other = "ef" * 32
        self.assertEqual(failing(GOOD, binary=other), ["the host reference is of target/webkit/bin/depsproof"])
        detail = [v for v in wkjudge.judge(GOOD, HOST, binary=other) if not v.ok][0].detail
        self.assertIn("efefefefefef", detail)

    def test_the_binary_is_checked_when_given_and_skipped_when_not(self):
        checked = "the host reference is of target/webkit/bin/depsproof"
        self.assertIn(checked, [v.name for v in wkjudge.judge(GOOD, HOST, binary=BIN)])
        self.assertNotIn(checked, [v.name for v in wkjudge.judge(GOOD, HOST)])

    def test_the_digest_is_a_whole_line_of_sixty_four_hex_digits(self):
        self.assertIsNone(wkjudge.binary_digest("WEBKIT:W3:BIN:abc\n"))
        self.assertIsNone(wkjudge.binary_digest(f"x WEBKIT:W3:BIN:{BIN}\n"))
        self.assertEqual(wkjudge.binary_digest(f"WEBKIT:W3:BIN:{BIN}\r\n"), BIN)

    def test_sha256_of_a_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "f"
            path.write_bytes(b"abc" * 1000)
            self.assertEqual(wkjudge.sha256_of(path), hashlib.sha256(b"abc" * 1000).hexdigest())


class TrivialValueTests(unittest.TestCase):
    """Two runs that both printed nothing meaningful are not agreement."""

    def both(self, old: str, new: str) -> list[str]:
        return failing(GOOD.replace(old, new), HOST.replace(old, new))

    def test_nothing_inked_is_not_a_pass(self):
        for value in ("0+0", "900+0", "0+1439", "900+", "x+y"):
            self.assertEqual(self.both("skia:inked=900+1439", f"skia:inked={value}"),
                             ["skia inked equals the host's"], value)

    def test_empty_digests_are_not_a_pass(self):
        for lib, key, was in (("libpng", "pixels", "ebe5e73b"), ("libjpeg-turbo", "pixels", "1c905bb2"),
                              ("libwebp", "pixels", "ebe5e73b"), ("skia", "pixels", "9ab89b56"),
                              ("freetype", "glyph-A", "edb00a86"), ("zlib", "crc32", "e52dc18f")):
            self.assertEqual(self.both(f"{lib}:{key}={was}", f"{lib}:{key}="),
                             [f"{lib} {key} equals the host's"], f"{lib} {key}")

    def test_zero_digests_and_counts_are_not_a_pass(self):
        self.assertEqual(self.both("zlib:crc32=e52dc18f", "zlib:crc32=00000000"), ["zlib crc32 equals the host's"])
        self.assertEqual(self.both("harfbuzz:glyphs=32", "harfbuzz:glyphs=0"), ["harfbuzz glyphs equals the host's"])
        self.assertEqual(self.both("harfbuzz:advance=28953", "harfbuzz:advance=0"),
                         ["harfbuzz advance equals the host's"])

    def test_the_detail_says_why(self):
        both = lambda text: text.replace("inked=900+1439", "inked=0+0")  # noqa: E731
        verdict = [v for v in wkjudge.judge(both(GOOD), both(HOST)) if not v.ok][0]
        self.assertIn("something was not drawn", verdict.detail)

    def test_real_values_are_not_trivial(self):
        for key, value in (("pixels", "ebe5e73b"), ("inked", "900+1439"), ("glyphs", "32"), ("crc32", "e52dc18f")):
            self.assertEqual(wkjudge.trivial(key, value), "", value)


if __name__ == "__main__":
    unittest.main()
