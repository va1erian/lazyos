#!/usr/bin/env python3
"""The W4 judge (``portjudge.py``) passes a good ``webkit-smoke`` log and fails each way it can go wrong.

Run: python tools/webkit/test_port_judge.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import portjudge  # noqa: E402

URL = "data:text/html,<title>hello lazyos</title><p>x</p>"
GOOD = f"""boot noise
WEBKIT:W4:UP
WEBKIT:W4:LOAD:about:blank
WEBKIT:W4:LOAD:{URL}
WEBKIT:W4:TITLE:hello lazyos
WEBKIT:W4:WEBPROCESS:UP:12
WEBKIT:W4:NETWORKPROCESS:UP:13
WEBKIT:W4:PAINT:800x600:0badf00d
WEBKIT:W4:INFO:PAINT:pixels=480000:displayCallbacks=3
WEBKIT:W4:PASS
"""


def failed(serial: str) -> set[str]:
    return {v.name for v in portjudge.judge(serial) if not v.ok}


class PortJudgeTests(unittest.TestCase):
    def test_a_good_log_passes(self) -> None:
        self.assertEqual(failed(GOOD), set())
        self.assertTrue(portjudge.passed(portjudge.judge(GOOD)))

    def test_a_log_without_pixels_still_passes(self) -> None:
        log = GOOD.replace("WEBKIT:W4:PAINT:800x600:0badf00d\n", "").replace(
            "pixels=480000:displayCallbacks=3", "nothing arrived (displayCallbacks=0)")
        self.assertEqual(failed(log), set())

    def test_each_missing_marker_fails(self) -> None:
        for marker, name in (("WEBKIT:W4:UP\n", "WEBKIT:W4:UP"),
                             ("WEBKIT:W4:WEBPROCESS:UP:12\n", "WEBKIT:W4:WEBPROCESS:UP"),
                             ("WEBKIT:W4:NETWORKPROCESS:UP:13\n", "WEBKIT:W4:NETWORKPROCESS:UP"),
                             ("WEBKIT:W4:LOAD:about:blank\n", "WEBKIT:W4:LOAD:about:blank"),
                             ("WEBKIT:W4:TITLE:hello lazyos\n", "WEBKIT:W4:TITLE:hello lazyos"),
                             ("WEBKIT:W4:PASS\n", "WEBKIT:W4:PASS")):
            self.assertIn(name, failed(GOOD.replace(marker, "")), marker)

    def test_a_wrong_title_fails(self) -> None:
        self.assertIn("WEBKIT:W4:TITLE:hello lazyos", failed(GOOD.replace("TITLE:hello lazyos", "TITLE:oops")))

    def test_a_dead_process_pid_fails(self) -> None:
        self.assertIn("WEBKIT:W4:WEBPROCESS:UP", failed(GOOD.replace("WEBPROCESS:UP:12", "WEBPROCESS:UP:0")))

    def test_pixels_without_a_paint_line_fail(self) -> None:
        log = GOOD.replace("WEBKIT:W4:PAINT:800x600:0badf00d\n", "")
        self.assertIn("WEBKIT:W4:PAINT with the pixels it counted", failed(log))

    def test_a_fail_line_or_a_crash_fails_even_after_pass(self) -> None:
        self.assertIn("no failure lines", failed(GOOD + "WEBKIT:W4:FAIL:paint:oops\n"))
        self.assertIn("no failure lines", failed(GOOD + "user: task 7 killed by SIGSEGV\n"))
        self.assertIn("no failure lines", failed(GOOD + "user: code at 0x1234: 00 00\n"))

    def test_pass_must_be_a_whole_line(self) -> None:
        self.assertIn("WEBKIT:W4:PASS", failed(GOOD.replace("WEBKIT:W4:PASS\n", "WEBKIT:W4:PASS:partial\n")))

    def test_the_stage_reached(self) -> None:
        self.assertEqual(portjudge.reached(""), "none")
        self.assertEqual(portjudge.reached("WEBKIT:W4:UP\n"), "UP")
        self.assertEqual(portjudge.reached("WEBKIT:W4:UP\nWEBKIT:W4:LOAD:about:blank\n"), "LOAD")
        self.assertEqual(portjudge.reached(GOOD), "PASS")


if __name__ == "__main__":
    unittest.main()
